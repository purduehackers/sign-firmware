//! Runs user Rhai scripts against the LED blocks. One script at a time:
//! `ScriptRunner::start` stops any running script (abort flag + join, so
//! at most one interpreter and one script stack ever exist), performs a
//! heap admission check, and spawns the interpreter on its own thread.
//! Outcomes flow back to the WebSocket thread over a channel.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chrono::Local;
use chrono_tz::US::Eastern;
use lightning_time::LightningTime;
use log::info;
use script_env::rhai::{Dynamic, EvalAltResult};
use script_env::{Handlers, LightningSnapshot};

use crate::{Block, Leds};

pub type SharedLeds = Arc<Mutex<Leds>>;

/// The interpreter stack is one contiguous allocation. The grain VM is
/// an iterative bytecode loop, so it runs far shallower than the old
/// tree-walking evaluator did.
const SCRIPT_STACK_BYTES: usize = 20 * 1024;
/// Rough heap cost of the curated engine, tightened against the numbers
/// this module logs on real hardware.
const ENGINE_HEAP_BYTES: usize = 40 * 1024;
/// Estimated heap per wire byte of a loaded grain artifact, from the
/// signal firmware's on-device measurements.
const ARTIFACT_BYTES_PER_WIRE_BYTE: usize = 5;
const RUN_MARGIN_BYTES: usize = 8 * 1024;

/// Abort latency bound: blocking script calls wake at least this often
/// to check the abort flag.
const SLEEP_CHUNK: Duration = Duration::from_millis(10);

/// Hard wall-clock cap on a script run. When it expires the script is
/// terminated, `script_done` is reported, and the sign returns to
/// Lightning Time.
const MAX_RUN: Duration = Duration::from_secs(30);

/// What the interpreter thread reports back to the WebSocket thread.
pub enum ScriptEvent {
    /// The script compiled and is running — acknowledge the push.
    Started { request_id: String },
    /// The script was refused (admission check or compile error).
    Rejected {
        request_id: String,
        message: String,
        line: Option<usize>,
        position: Option<usize>,
    },
    /// The script returned on its own; the sign is back on Lightning Time.
    Done,
    /// The script died with a runtime error.
    Failed { message: String },
}

pub struct ScriptRunner {
    abort: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
    script_active: Arc<AtomicBool>,
    leds: SharedLeds,
    events: mpsc::Sender<ScriptEvent>,
}

impl ScriptRunner {
    pub fn new(
        leds: SharedLeds,
        script_active: Arc<AtomicBool>,
        events: mpsc::Sender<ScriptEvent>,
    ) -> Self {
        Self {
            abort: Arc::new(AtomicBool::new(false)),
            handle: None,
            script_active,
            leds,
            events,
        }
    }

    /// Stops the running script, if any, and waits for its thread.
    pub fn stop(&mut self) {
        self.abort.store(true, Ordering::SeqCst);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
        self.script_active.store(false, Ordering::SeqCst);
    }

    /// Replaces the running script. Replies flow through the event channel.
    pub fn start(&mut self, request_id: String, artifact: Vec<u8>) {
        self.stop();

        if let Err(message) = heap_check(artifact.len()) {
            let _ = self.events.send(ScriptEvent::Rejected {
                request_id,
                message,
                line: None,
                position: None,
            });
            return;
        }

        self.abort = Arc::new(AtomicBool::new(false));
        let abort = self.abort.clone();
        let leds = self.leds.clone();
        let active = self.script_active.clone();
        let events = self.events.clone();
        let thread_request_id = request_id.clone();

        let spawned = std::thread::Builder::new()
            .name("rhai".into())
            .stack_size(SCRIPT_STACK_BYTES)
            .spawn(move || run_script(thread_request_id, artifact, abort, leds, active, events));

        match spawned {
            Ok(handle) => self.handle = Some(handle),
            Err(e) => {
                let _ = self.events.send(ScriptEvent::Rejected {
                    request_id,
                    message: format!("failed to start the script thread: {e}"),
                    line: None,
                    position: None,
                });
            }
        }
    }
}

/// Rust aborts on allocation failure and abort reboots this board, so an
/// interpreter that would run out of memory must be refused up front.
/// Free-heap totals lie about contiguous space, so the stack (one
/// contiguous allocation) is checked against the largest free block.
pub fn free_heap() -> (usize, usize) {
    let free = unsafe { esp_idf_svc::sys::esp_get_free_heap_size() } as usize;
    let largest = unsafe {
        esp_idf_svc::sys::heap_caps_get_largest_free_block(esp_idf_svc::sys::MALLOC_CAP_8BIT)
    } as usize;
    (free, largest)
}

fn log_heap(stage: &str) {
    let (free, largest) = free_heap();
    info!("Heap [{stage}]: {free} free, largest block {largest}");
}

fn heap_check(artifact_bytes: usize) -> Result<(), String> {
    let (free, largest) = free_heap();
    let needed = SCRIPT_STACK_BYTES
        + ENGINE_HEAP_BYTES
        + artifact_bytes * ARTIFACT_BYTES_PER_WIRE_BYTE
        + RUN_MARGIN_BYTES;
    info!("Heap before script: {free} free, largest block {largest}, need ~{needed}");

    if largest < SCRIPT_STACK_BYTES {
        return Err(format!(
            "device out of memory: largest free block is {largest} bytes, the script stack needs {SCRIPT_STACK_BYTES}"
        ));
    }
    if free < needed {
        return Err(format!(
            "script too large for this device: needs about {needed} bytes of heap, {free} free"
        ));
    }
    Ok(())
}

fn block_for_index(block: i64) -> Block {
    match block {
        0 => Block::Center,
        1 => Block::BottomLeft,
        2 => Block::BottomRight,
        3 => Block::Right,
        _ => Block::Top,
    }
}

fn lightning_snapshot() -> LightningSnapshot {
    let time = LightningTime::from(Local::now().with_timezone(&Eastern).time());
    let colors = time.colors();
    LightningSnapshot {
        bolts: time.bolts,
        zaps: time.zaps,
        sparks: time.sparks,
        charges: time.charges,
        bolt_color: [colors.bolt.red, colors.bolt.green, colors.bolt.blue],
        zap_color: [colors.zap.red, colors.zap.green, colors.zap.blue],
        spark_color: [colors.spark.red, colors.spark.green, colors.spark.blue],
    }
}

fn run_script(
    request_id: String,
    artifact: Vec<u8>,
    abort: Arc<AtomicBool>,
    leds: SharedLeds,
    active: Arc<AtomicBool>,
    events: mpsc::Sender<ScriptEvent>,
) {
    let start = Instant::now();
    let deadline = start + MAX_RUN;
    // f32 bits in an AtomicU32: the ESP32 has no wider atomics.
    let brightness = Arc::new(AtomicU32::new(1.0_f32.to_bits()));

    let mut engine = script_env::new_engine();
    script_env::register_api(
        &mut engine,
        Handlers {
            set_block: Box::new({
                let leds = leds.clone();
                let brightness = brightness.clone();
                move |block, r, g, b| {
                    let scale = f32::from_bits(brightness.load(Ordering::Relaxed));
                    let dim = |c: u8| (c as f32 * scale).round().clamp(0.0, 255.0) as u8;
                    leds.lock().unwrap().set_color(
                        palette::rgb::Rgb::new(dim(r), dim(g), dim(b)),
                        block_for_index(block),
                    );
                }
            }),
            sleep: Box::new({
                let abort = abort.clone();
                move |ms| {
                    let until = Instant::now() + Duration::from_millis(ms.max(0) as u64);
                    let until = until.min(deadline);
                    loop {
                        if abort.load(Ordering::SeqCst) {
                            return;
                        }
                        let now = Instant::now();
                        if now >= until {
                            return;
                        }
                        std::thread::sleep(SLEEP_CHUNK.min(until - now));
                    }
                }
            }),
            millis: Box::new(move || start.elapsed().as_millis() as i64),
            // Hardware RNG: a register read, cheap enough per value.
            random_u32: Box::new(|| unsafe { esp_idf_svc::sys::esp_random() }),
            set_brightness: Box::new({
                let brightness = brightness.clone();
                move |v| brightness.store(v.to_bits(), Ordering::Relaxed)
            }),
            lightning_time: Box::new(lightning_snapshot),
        },
    );

    engine.on_progress({
        let abort = abort.clone();
        move |ops| {
            if abort.load(Ordering::SeqCst) {
                return Some(Dynamic::from("aborted"));
            }
            if Instant::now() >= deadline {
                return Some(Dynamic::from("deadline"));
            }
            // Busy scripts never yield on their own; give the IDLE task
            // (and its watchdog) a breath now and then.
            if ops % 8192 == 0 {
                std::thread::sleep(Duration::from_millis(1));
            }
            None
        }
    });

    log_heap("engine built");

    let _ = events.send(ScriptEvent::Started { request_id });
    active.store(true, Ordering::SeqCst);
    let outcome = script_env::run_artifact(&engine, &artifact);
    active.store(false, Ordering::SeqCst);

    match outcome {
        Ok(()) => {
            if !abort.load(Ordering::SeqCst) {
                let _ = events.send(ScriptEvent::Done);
            }
        }
        Err(e) => match *e {
            EvalAltResult::ErrorTerminated(token, _) => {
                // A deadline is a normal end of life; an abort is the
                // runner replacing us and needs no report.
                if token.to_string() == "deadline" {
                    let _ = events.send(ScriptEvent::Done);
                }
            }
            other => {
                let _ = events.send(ScriptEvent::Failed {
                    message: other.to_string(),
                });
            }
        },
    }
}

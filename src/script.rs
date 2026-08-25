//! Runs user Rhai scripts against the LED blocks. One script at a time:
//! `ScriptRunner::start` stops any running script (abort flag + join, so
//! at most one interpreter and one script stack ever exist), performs a
//! heap admission check, and spawns the interpreter on its own thread.
//! Outcomes flow back to the WebSocket thread over a channel.

use std::sync::atomic::{AtomicBool, Ordering};
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

/// The interpreter stack is one contiguous allocation. Rhai's eval
/// recursion is bounded by the expression-depth and call-level limits in
/// script-env, not by script length, so this does not grow with scripts.
const SCRIPT_STACK_BYTES: usize = 32 * 1024;
/// Rough heap cost of the curated engine, to be tightened against the
/// numbers `log_heap` prints on real hardware.
const ENGINE_HEAP_BYTES: usize = 100 * 1024;
/// Measured on-device cost of a parsed AST per byte of source.
const AST_BYTES_PER_SOURCE_BYTE: usize = 24;
const RUN_MARGIN_BYTES: usize = 8 * 1024;

/// Abort latency bound: blocking script calls wake at least this often
/// to check the abort flag.
const SLEEP_CHUNK: Duration = Duration::from_millis(10);

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
    pub fn start(&mut self, request_id: String, script: String) {
        self.stop();

        if let Err(message) = heap_check(script.len()) {
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
            .spawn(move || run_script(thread_request_id, script, abort, leds, active, events));

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
fn heap_check(script_bytes: usize) -> Result<(), String> {
    let free = unsafe { esp_idf_svc::sys::esp_get_free_heap_size() } as usize;
    let largest = unsafe {
        esp_idf_svc::sys::heap_caps_get_largest_free_block(esp_idf_svc::sys::MALLOC_CAP_8BIT)
    } as usize;
    let needed = SCRIPT_STACK_BYTES
        + ENGINE_HEAP_BYTES
        + script_bytes * AST_BYTES_PER_SOURCE_BYTE
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
    script: String,
    abort: Arc<AtomicBool>,
    leds: SharedLeds,
    active: Arc<AtomicBool>,
    events: mpsc::Sender<ScriptEvent>,
) {
    let start = Instant::now();

    let mut engine = script_env::new_engine();
    script_env::register_api(
        &mut engine,
        Handlers {
            set_block: Box::new({
                let leds = leds.clone();
                move |block, r, g, b| {
                    leds.lock()
                        .unwrap()
                        .set_color(palette::rgb::Rgb::new(r, g, b), block_for_index(block));
                }
            }),
            sleep: Box::new({
                let abort = abort.clone();
                move |ms| {
                    let until = Instant::now() + Duration::from_millis(ms.max(0) as u64);
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
            lightning_time: Box::new(lightning_snapshot),
        },
    );

    engine.on_progress({
        let abort = abort.clone();
        move |ops| {
            if abort.load(Ordering::SeqCst) {
                return Some(Dynamic::from("aborted"));
            }
            // Busy scripts never yield on their own; give the IDLE task
            // (and its watchdog) a breath now and then.
            if ops % 8192 == 0 {
                std::thread::sleep(Duration::from_millis(1));
            }
            None
        }
    });

    let ast = match script_env::compile(&engine, &script) {
        Ok(ast) => ast,
        Err(e) => {
            let _ = events.send(ScriptEvent::Rejected {
                request_id,
                message: e.0.to_string(),
                line: e.1.line(),
                position: e.1.position(),
            });
            return;
        }
    };
    // `run` would otherwise hold both the text and the AST live.
    drop(script);

    let _ = events.send(ScriptEvent::Started { request_id });
    active.store(true, Ordering::SeqCst);
    let outcome = script_env::run(&engine, &ast);
    active.store(false, Ordering::SeqCst);

    match outcome {
        Ok(()) => {
            if !abort.load(Ordering::SeqCst) {
                let _ = events.send(ScriptEvent::Done);
            }
        }
        Err(e) => match *e {
            // Termination through on_progress is the runner stopping us.
            EvalAltResult::ErrorTerminated(..) => {}
            other => {
                let _ = events.send(ScriptEvent::Failed {
                    message: other.to_string(),
                });
            }
        },
    }
}

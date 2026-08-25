//! The Rhai environment the Sign gives to user scripts: engine
//! construction, sandbox limits, and the script-facing API. This crate is
//! compiled into both the firmware and the WASM validator, so what the
//! server accepts and what the device runs can never disagree.
//!
//! The hardware never appears here — the firmware injects it through
//! [`Handlers`], and the validator injects [`Handlers::stubs`].

pub use rhai;

use std::sync::{Arc, OnceLock};

use rhai::packages::{
    ArithmeticPackage, BasicArrayPackage, BasicIteratorPackage, BasicMapPackage, BasicMathPackage,
    LanguageCorePackage, LogicPackage, Package,
};
use rhai::{
    Array, Dynamic, Engine, EvalAltResult, Module, ParseError, Scope, Shared, AST, FLOAT, INT,
};

/// Raw script size cap, enforced by the validator before compiling and by
/// the firmware before accepting a `set_script` frame. Sized against the
/// measured ~24 bytes of on-device AST heap per byte of source.
pub const MAX_SCRIPT_BYTES: usize = 8 * 1024;

/// How many LED blocks the sign has.
pub const NUM_BLOCKS: usize = 5;

// Block indices. The values are a wire contract with the firmware's
// `Block` enum ordinals — scripts see them as the constants pushed by
// [`base_scope`].
pub const BLOCK_CENTER: INT = 0;
pub const BLOCK_BOTTOM_LEFT: INT = 1;
pub const BLOCK_BOTTOM_RIGHT: INT = 2;
pub const BLOCK_RIGHT: INT = 3;
pub const BLOCK_TOP: INT = 4;

/// One reading of the Lightning Time clock, for `lightning_time()`.
#[derive(Clone, Copy, Debug)]
pub struct LightningSnapshot {
    pub bolts: u8,
    pub zaps: u8,
    pub sparks: u8,
    pub charges: u8,
    pub bolt_color: [u8; 3],
    pub zap_color: [u8; 3],
    pub spark_color: [u8; 3],
}

/// The seam between the shared language definition and the hardware.
pub struct Handlers {
    /// set_block(block, r, g, b) — arguments already validated and clamped.
    pub set_block: Box<dyn Fn(INT, u8, u8, u8) + Send + Sync>,
    /// sleep(ms) — implementations must chunk and honor the abort flag.
    pub sleep: Box<dyn Fn(INT) + Send + Sync>,
    /// Monotonic milliseconds since script start.
    pub millis: Box<dyn Fn() -> INT + Send + Sync>,
    /// A fresh uniformly-distributed u32 per call.
    pub random_u32: Box<dyn Fn() -> u32 + Send + Sync>,
    /// The current Lightning Time reading.
    pub lightning_time: Box<dyn Fn() -> LightningSnapshot + Send + Sync>,
}

impl Handlers {
    /// No-op handlers for the validator: nothing to drive, a virtual
    /// clock that sleeps instantly, and a deterministic RNG so validation
    /// never depends on entropy.
    pub fn stubs() -> Self {
        // A Mutex, not an AtomicI64 — the ESP32 has no 64-bit atomics.
        let clock = Arc::new(std::sync::Mutex::new(0i64));
        let rng_state = Arc::new(std::sync::atomic::AtomicU32::new(0x8acc_8acc));
        Handlers {
            set_block: Box::new(|_, _, _, _| {}),
            sleep: {
                let clock = clock.clone();
                Box::new(move |ms| {
                    *clock.lock().unwrap() += ms.max(0);
                })
            },
            millis: Box::new(move || *clock.lock().unwrap()),
            random_u32: Box::new(move || {
                // xorshift32; state updates are racy-free enough for stubs
                let mut x = rng_state.load(std::sync::atomic::Ordering::SeqCst);
                x ^= x << 13;
                x ^= x >> 17;
                x ^= x << 5;
                rng_state.store(x, std::sync::atomic::Ordering::SeqCst);
                x
            }),
            lightning_time: Box::new(|| LightningSnapshot {
                bolts: 8,
                zaps: 0,
                sparks: 0,
                charges: 0,
                bolt_color: [251, 219, 0],
                zap_color: [255, 0, 138],
                spark_color: [0, 255, 255],
            }),
        }
    }
}

fn arithmetic_module() -> Shared<Module> {
    static CELL: OnceLock<Shared<Module>> = OnceLock::new();
    CELL.get_or_init(|| ArithmeticPackage::new().as_shared_module())
        .clone()
}

fn logic_module() -> Shared<Module> {
    static CELL: OnceLock<Shared<Module>> = OnceLock::new();
    CELL.get_or_init(|| LogicPackage::new().as_shared_module())
        .clone()
}

/// Builds the sign's engine: raw base, arithmetic/logic shared by pointer
/// (built once per process), the curated per-engine packages, and the
/// sandbox limits. The per-engine packages are deliberately not cached —
/// a cached module is never freed, and the firmware wants its heap back
/// the moment no script is running.
pub fn new_engine() -> Engine {
    let mut engine = Engine::new_raw();

    // `Engine::new` enables the interner and `new_raw` does not; losing it
    // costs tens of KB of duplicated strings on real scripts.
    engine.set_max_strings_interned(1024);

    <ArithmeticPackage as Package>::init_engine(&mut engine);
    engine.register_global_module(arithmetic_module());
    <LogicPackage as Package>::init_engine(&mut engine);
    engine.register_global_module(logic_module());

    LanguageCorePackage::new().register_into_engine(&mut engine);
    BasicIteratorPackage::new().register_into_engine(&mut engine);
    BasicMathPackage::new().register_into_engine(&mut engine);
    BasicArrayPackage::new().register_into_engine(&mut engine);
    BasicMapPackage::new().register_into_engine(&mut engine);

    apply_limits(&mut engine);
    engine
}

/// The sandbox. Parse-time limits (expression depth) also make
/// `Engine::compile` reject pathological inputs during validation. There
/// is deliberately no operation cap: a sign animation legitimately runs
/// until it is replaced, and what actually bounds a run is the abort flag
/// the firmware checks in `on_progress` and inside `sleep`.
pub fn apply_limits(engine: &mut Engine) {
    engine.set_max_call_levels(16);
    engine.set_max_expr_depths(32, 16);
    engine.set_max_string_size(4 * 1024);
    engine.set_max_array_size(1024);
    // NB: 0 would mean "no limit" in rhai, not "no maps".
    engine.set_max_map_size(64);
    engine.disable_symbol("eval");
    engine.set_strict_variables(true);
}

/// The constants every script compiles and runs against.
pub fn base_scope() -> Scope<'static> {
    let mut scope = Scope::new();
    scope.push_constant("CENTER", BLOCK_CENTER);
    scope.push_constant("BOTTOM_LEFT", BLOCK_BOTTOM_LEFT);
    scope.push_constant("BOTTOM_RIGHT", BLOCK_BOTTOM_RIGHT);
    scope.push_constant("RIGHT", BLOCK_RIGHT);
    scope.push_constant("TOP", BLOCK_TOP);
    scope.push_constant("NUM_BLOCKS", NUM_BLOCKS as INT);
    scope
}

/// Compiles against [`base_scope`] so `strict_variables` can see the
/// block constants. Both the validator and the firmware go through here.
pub fn compile(engine: &Engine, script: &str) -> Result<AST, ParseError> {
    engine.compile_with_scope(&base_scope(), script)
}

/// Runs a compiled script against [`base_scope`].
pub fn run(engine: &Engine, ast: &AST) -> Result<(), Box<EvalAltResult>> {
    engine.run_ast_with_scope(&mut base_scope(), ast)
}

fn channel(v: INT) -> u8 {
    v.clamp(0, 255) as u8
}

fn rgb_from_array(a: &Array) -> Result<(u8, u8, u8), Box<EvalAltResult>> {
    if a.len() != 3 {
        return Err(format!("expected [r, g, b], got {} element(s)", a.len()).into());
    }
    let mut c = [0u8; 3];
    for (slot, value) in c.iter_mut().zip(a.iter()) {
        let n = value.as_int().map_err(|t| -> Box<EvalAltResult> {
            format!("expected [r, g, b] of ints, got {t}").into()
        })?;
        *slot = channel(n);
    }
    Ok((c[0], c[1], c[2]))
}

fn hsv_to_rgb(h: FLOAT, s: FLOAT, v: FLOAT) -> [u8; 3] {
    let h = h.rem_euclid(360.0);
    let s = s.clamp(0.0, 1.0);
    let v = v.clamp(0.0, 1.0);

    let c = v * s;
    let hp = h / 60.0;
    let x = c * (1.0 - (hp % 2.0 - 1.0).abs());
    let (r, g, b) = match hp as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = v - c;
    [
        ((r + m) * 255.0).round() as u8,
        ((g + m) * 255.0).round() as u8,
        ((b + m) * 255.0).round() as u8,
    ]
}

fn color_array(c: [u8; 3]) -> Dynamic {
    let array: Array = c.iter().map(|&v| Dynamic::from(v as INT)).collect();
    Dynamic::from(array)
}

/// Register the script-facing API.
pub fn register_api(engine: &mut Engine, handlers: Handlers) {
    let Handlers {
        set_block,
        sleep,
        millis,
        random_u32,
        lightning_time,
    } = handlers;

    let set_block = Arc::new(set_block);
    engine.register_fn("set_block", {
        let f = set_block.clone();
        move |block: INT, r: INT, g: INT, b: INT| -> Result<(), Box<EvalAltResult>> {
            if !(0..NUM_BLOCKS as INT).contains(&block) {
                return Err(format!(
                    "set_block: no block {block} (use CENTER/BOTTOM_LEFT/BOTTOM_RIGHT/RIGHT/TOP)"
                )
                .into());
            }
            f(block, channel(r), channel(g), channel(b));
            Ok(())
        }
    });
    engine.register_fn("set_block", {
        let f = set_block.clone();
        move |block: INT, rgb: Array| -> Result<(), Box<EvalAltResult>> {
            if !(0..NUM_BLOCKS as INT).contains(&block) {
                return Err(format!(
                    "set_block: no block {block} (use CENTER/BOTTOM_LEFT/BOTTOM_RIGHT/RIGHT/TOP)"
                )
                .into());
            }
            let (r, g, b) = rgb_from_array(&rgb)?;
            f(block, r, g, b);
            Ok(())
        }
    });
    engine.register_fn("set_all", {
        let f = set_block.clone();
        move |r: INT, g: INT, b: INT| {
            for block in 0..NUM_BLOCKS as INT {
                f(block, channel(r), channel(g), channel(b));
            }
        }
    });
    engine.register_fn("set_all", {
        let f = set_block.clone();
        move |rgb: Array| -> Result<(), Box<EvalAltResult>> {
            let (r, g, b) = rgb_from_array(&rgb)?;
            for block in 0..NUM_BLOCKS as INT {
                f(block, r, g, b);
            }
            Ok(())
        }
    });

    // sleep_until needs both, so they are shared rather than moved.
    let sleep = Arc::new(sleep);
    let millis = Arc::new(millis);
    engine.register_fn("sleep", {
        let sleep = sleep.clone();
        move |ms: INT| sleep(ms)
    });
    engine.register_fn("millis", {
        let millis = millis.clone();
        move || millis()
    });
    // sleep_until(t) instead of sleep(period): a pattern built from
    // relative sleeps accumulates every delay the work in between cost,
    // so its period drifts long. Against an absolute target the error
    // cannot accumulate.
    engine.register_fn("sleep_until", {
        let sleep = sleep.clone();
        let millis = millis.clone();
        move |target_ms: INT| {
            let remaining = target_ms - millis();
            if remaining > 0 {
                sleep(remaining);
            }
        }
    });

    let random_u32 = Arc::new(random_u32);
    let random_u64 = {
        let random_u32 = random_u32.clone();
        Arc::new(move || ((random_u32() as u64) << 32) | random_u32() as u64)
    };
    // [0.0, 1.0). 24 bits, which is all an f32 mantissa holds.
    engine.register_fn("rand_float", {
        let random_u32 = random_u32.clone();
        move || -> FLOAT { (random_u32() >> 8) as FLOAT / 16_777_216.0 }
    });
    // Inclusive on both ends. Lemire's multiply-shift rather than a
    // modulo, so the distribution isn't skewed toward the low end.
    engine.register_fn("rand_int", {
        let random_u64 = random_u64.clone();
        move |lo: INT, hi: INT| -> Result<INT, Box<EvalAltResult>> {
            if hi < lo {
                return Err(format!("rand_int: empty range {lo}..{hi}").into());
            }
            let span = (hi as i128 - lo as i128 + 1) as u128;
            let scaled = ((random_u64() as u128) * span) >> 64;
            Ok((lo as i128 + scaled as i128) as INT)
        }
    });
    engine.register_fn("rand_chance", {
        let random_u32 = random_u32.clone();
        move |p: FLOAT| -> bool { (random_u32() >> 8) as FLOAT / 16_777_216.0 < p }
    });

    engine.register_fn("hsv", |h: FLOAT, s: FLOAT, v: FLOAT| -> Array {
        hsv_to_rgb(h, s, v)
            .iter()
            .map(|&c| Dynamic::from(c as INT))
            .collect()
    });

    // Returns #{ bolts, zaps, sparks, charges, colors: #{ bolt, zap, spark } }
    engine.register_fn("lightning_time", move || -> rhai::Map {
        let snap = lightning_time();
        let mut colors = rhai::Map::new();
        colors.insert("bolt".into(), color_array(snap.bolt_color));
        colors.insert("zap".into(), color_array(snap.zap_color));
        colors.insert("spark".into(), color_array(snap.spark_color));

        let mut map = rhai::Map::new();
        map.insert("bolts".into(), Dynamic::from(snap.bolts as INT));
        map.insert("zaps".into(), Dynamic::from(snap.zaps as INT));
        map.insert("sparks".into(), Dynamic::from(snap.sparks as INT));
        map.insert("charges".into(), Dynamic::from(snap.charges as INT));
        map.insert("colors".into(), Dynamic::from(colors));
        map
    });
}

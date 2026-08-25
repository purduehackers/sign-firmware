use std::sync::{Arc, Mutex};

use rhai::INT;
use script_env::{compile, new_engine, register_api, run, Handlers};

/// Handlers that record every set_block call and advance a virtual clock.
fn recording_handlers() -> (Handlers, Arc<Mutex<Vec<(INT, u8, u8, u8)>>>) {
    let writes: Arc<Mutex<Vec<(INT, u8, u8, u8)>>> = Arc::new(Mutex::new(Vec::new()));
    let clock = Arc::new(std::sync::atomic::AtomicI64::new(0));

    let stubs = Handlers::stubs();
    let handlers = Handlers {
        set_block: {
            let writes = writes.clone();
            Box::new(move |block, r, g, b| writes.lock().unwrap().push((block, r, g, b)))
        },
        sleep: {
            let clock = clock.clone();
            Box::new(move |ms| {
                clock.fetch_add(ms.max(0), std::sync::atomic::Ordering::SeqCst);
            })
        },
        millis: Box::new(move || clock.load(std::sync::atomic::Ordering::SeqCst)),
        random_u32: stubs.random_u32,
        lightning_time: stubs.lightning_time,
    };
    (handlers, writes)
}

fn run_script(script: &str) -> Result<Vec<(INT, u8, u8, u8)>, String> {
    let mut engine = new_engine();
    let (handlers, writes) = recording_handlers();
    register_api(&mut engine, handlers);
    let ast = compile(&engine, script).map_err(|e| e.to_string())?;
    run(&engine, &ast).map_err(|e| e.to_string())?;
    let writes = writes.lock().unwrap().clone();
    Ok(writes)
}

#[test]
fn set_block_with_constants() {
    let writes = run_script("set_block(TOP, 10, 20, 30);").unwrap();
    assert_eq!(writes, vec![(4, 10, 20, 30)]);
}

#[test]
fn set_block_accepts_rgb_array() {
    let writes = run_script("set_block(CENTER, [1, 2, 3]);").unwrap();
    assert_eq!(writes, vec![(0, 1, 2, 3)]);
}

#[test]
fn set_all_writes_every_block() {
    let writes = run_script("set_all(5, 6, 7);").unwrap();
    assert_eq!(writes.len(), 5);
    assert!(writes.iter().all(|&(_, r, g, b)| (r, g, b) == (5, 6, 7)));
    let blocks: Vec<INT> = writes.iter().map(|&(block, ..)| block).collect();
    assert_eq!(blocks, vec![0, 1, 2, 3, 4]);
}

#[test]
fn channels_clamp_to_u8() {
    let writes = run_script("set_block(RIGHT, 999, -5, 256);").unwrap();
    assert_eq!(writes, vec![(3, 255, 0, 255)]);
}

#[test]
fn bad_block_index_is_a_runtime_error() {
    let err = run_script("set_block(7, 0, 0, 0);").unwrap_err();
    assert!(err.contains("no block 7"), "unexpected error: {err}");
}

#[test]
fn hsv_primaries() {
    let writes = run_script(
        "set_all(hsv(0.0, 1.0, 1.0));\n\
         set_all(hsv(120.0, 1.0, 1.0));\n\
         set_all(hsv(240.0, 1.0, 1.0));\n\
         set_all(hsv(0.0, 0.0, 1.0));",
    )
    .unwrap();
    assert_eq!(writes[0], (0, 255, 0, 0));
    assert_eq!(writes[5], (0, 0, 255, 0));
    assert_eq!(writes[10], (0, 0, 0, 255));
    assert_eq!(writes[15], (0, 255, 255, 255));
}

#[test]
fn sleep_until_uses_absolute_time() {
    let writes = run_script(
        "sleep(100);\n\
         sleep_until(250);\n\
         if millis() == 250 { set_all(1, 1, 1); }\n\
         sleep_until(200);\n\
         if millis() == 250 { set_all(2, 2, 2); }",
    )
    .unwrap();
    assert_eq!(writes.len(), 10, "both conditions should have fired");
}

#[test]
fn rand_int_stays_in_range() {
    let writes = run_script(
        "for i in 0..100 {\n\
             let v = rand_int(10, 12);\n\
             if v < 10 || v > 12 { set_all(255, 0, 0); }\n\
         }",
    )
    .unwrap();
    assert!(writes.is_empty(), "rand_int left its range");
}

#[test]
fn rand_int_rejects_empty_range() {
    let err = run_script("rand_int(5, 1);").unwrap_err();
    assert!(err.contains("empty range"), "unexpected error: {err}");
}

#[test]
fn lightning_time_map_shape() {
    // Stub snapshot: bolts=8, bolt_color=[251, 219, 0]
    let writes = run_script(
        "let lt = lightning_time();\n\
         if lt.bolts == 8 { set_block(TOP, lt.colors.bolt); }",
    )
    .unwrap();
    assert_eq!(writes, vec![(4, 251, 219, 0)]);
}

#[test]
fn undefined_variables_fail_at_compile_time() {
    let engine = new_engine();
    assert!(compile(&engine, "set_all(r, 0, 0);").is_err());
}

#[test]
fn eval_is_disabled() {
    let engine = new_engine();
    assert!(compile(&engine, "eval(\"1 + 1\");").is_err());
}

#[test]
fn deep_expressions_are_rejected_at_parse_time() {
    let engine = new_engine();
    let script = format!("let x = {}1{};", "(".repeat(200), ")".repeat(200));
    assert!(compile(&engine, &script).is_err());
}

#[test]
fn loops_and_conditionals_work() {
    // The imperative style from the plan: a script that owns its loop.
    let writes = run_script(
        "let t = 0;\n\
         for i in 0..3 {\n\
             let v = i * 10;\n\
             set_all(v, v, v);\n\
             sleep_until(t + 33);\n\
             t += 33;\n\
         }",
    )
    .unwrap();
    assert_eq!(writes.len(), 15);
}

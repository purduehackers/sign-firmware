//! The upload-time gate: api-v4 calls [`validate`] before a script is
//! pushed to the sign. The engine and limits come from `script-env`, the
//! same crate the firmware compiles, so anything accepted here parses
//! identically on the device.

use serde::Serialize;
use wasm_bindgen::prelude::*;

#[derive(Serialize)]
struct Validation {
    ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    line: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    col: Option<usize>,
}

fn check(script: &str) -> Validation {
    if script.len() > script_env::MAX_SCRIPT_BYTES {
        return Validation {
            ok: false,
            error: Some(format!(
                "script is {} bytes; the sign accepts at most {}",
                script.len(),
                script_env::MAX_SCRIPT_BYTES
            )),
            line: None,
            col: None,
        };
    }

    let mut engine = script_env::new_engine();
    script_env::register_api(&mut engine, script_env::Handlers::stubs());

    match script_env::compile(&engine, script) {
        Ok(_) => Validation {
            ok: true,
            error: None,
            line: None,
            col: None,
        },
        Err(err) => Validation {
            ok: false,
            error: Some(err.0.to_string()),
            line: err.1.line(),
            col: err.1.position(),
        },
    }
}

/// Validates a script and returns a JSON string:
/// `{"ok":true}` or `{"ok":false,"error":"...","line":N,"col":M}`
/// (line/col are absent when the error has no position, e.g. size cap).
#[wasm_bindgen]
pub fn validate(script: &str) -> String {
    serde_json::to_string(&check(script)).expect("validation result serializes")
}

#[cfg(test)]
mod tests {
    use super::check;

    #[test]
    fn accepts_a_real_script() {
        let result = check("let t = 0;\nloop {\n  set_all(hsv(280.0, 1.0, 0.5));\n  sleep_until(t + 33);\n  t += 33;\n}");
        assert!(result.ok, "{:?}", result.error);
    }

    #[test]
    fn rejects_a_syntax_error_with_position() {
        let result = check("let x = ;");
        assert!(!result.ok);
        assert_eq!(result.line, Some(1));
        assert!(result.col.is_some());
    }

    #[test]
    fn rejects_oversize_scripts() {
        let result = check(&"set_all(0, 0, 0);\n".repeat(1000));
        assert!(!result.ok);
        assert!(result.error.unwrap().contains("at most"));
    }
}

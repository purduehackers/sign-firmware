//! The upload-time gate: api-v4 calls [`validate`] before a script is
//! pushed to the sign. The engine and limits come from `script-env`, the
//! same crate the firmware compiles, so anything accepted here parses
//! identically on the device.

use base64::Engine as _;
use serde::Serialize;
use wasm_bindgen::prelude::*;

#[derive(Serialize)]
struct Validation {
    ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    artifact: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    line: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    col: Option<usize>,
}

fn failure(error: String, line: Option<usize>, col: Option<usize>) -> Validation {
    Validation {
        ok: false,
        artifact: None,
        error: Some(error),
        line,
        col,
    }
}

fn check(script: &str) -> Validation {
    if script.len() > script_env::MAX_SCRIPT_BYTES {
        return failure(
            format!(
                "script is {} bytes; the sign accepts at most {}",
                script.len(),
                script_env::MAX_SCRIPT_BYTES
            ),
            None,
            None,
        );
    }

    let mut engine = script_env::new_engine();
    script_env::register_api(&mut engine, script_env::Handlers::stubs());

    let ast = match script_env::compile(&engine, script) {
        Ok(ast) => ast,
        Err(err) => return failure(err.0.to_string(), err.1.line(), err.1.position()),
    };

    let artifact = match script_env::lower(&ast) {
        Ok(artifact) => artifact,
        Err(error) => return failure(format!("lowering failed: {error}"), None, None),
    };
    if artifact.program.len() > script_env::MAX_ARTIFACT_BYTES {
        return failure(
            format!(
                "lowered artifact is {} bytes; the sign accepts at most {}",
                artifact.program.len(),
                script_env::MAX_ARTIFACT_BYTES
            ),
            None,
            None,
        );
    }

    Validation {
        ok: true,
        artifact: Some(base64::engine::general_purpose::STANDARD.encode(&artifact.program)),
        error: None,
        line: None,
        col: None,
    }
}

/// Validates and lowers a script, returning a JSON string:
/// `{"ok":true,"artifact":"<base64 grain bytecode>"}` or
/// `{"ok":false,"error":"...","line":N,"col":M}`
/// (line/col are absent when the error has no position, e.g. size cap).
#[wasm_bindgen]
pub fn validate(script: &str) -> String {
    serde_json::to_string(&check(script)).expect("validation result serializes")
}

#[cfg(test)]
mod tests {
    use super::check;

    #[test]
    fn accepts_and_lowers_a_real_script() {
        let result = check("let t = 0;\nloop {\n  set_all(hsv(280.0, 1.0, 0.5));\n  sleep_until(t + 33);\n  t += 33;\n}");
        assert!(result.ok, "{:?}", result.error);
        assert!(!result.artifact.as_deref().unwrap_or_default().is_empty());
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

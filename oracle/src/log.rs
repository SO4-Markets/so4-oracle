use serde_json::json;
use worker::console_log;

pub fn log_json(level: &str, message: &str, context: serde_json::Value) {
    let log_entry = json!({
        "level": level,
        "timestamp": current_timestamp(),
        "message": message,
        "context": context,
    });

    let log_str = serde_json::to_string(&log_entry).unwrap_or_else(|_| {
        format!(
            r#"{{"level":"error","timestamp":{},"message":"log serialization failed"}}"#,
            current_timestamp()
        )
    });

    console_log!("{}", log_str);
}

#[cfg(target_arch = "wasm32")]
fn current_timestamp() -> String {
    let millis = js_sys::Date::now() as u64;
    format!("{}.{:03}", millis / 1000, millis % 1000)
}

#[cfg(not(target_arch = "wasm32"))]
fn current_timestamp() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let secs = duration.as_secs();
    let millis = duration.subsec_millis();
    format!("{}.{:03}", secs, millis)
}

/// Convenience wrapper for INFO-level structured logs.
pub fn info(message: &str, context: serde_json::Value) {
    log_json("INFO", message, context);
}

/// Convenience wrapper for ERROR-level structured logs.
pub fn error(message: &str, context: serde_json::Value) {
    log_json("ERROR", message, context);
}

/// Convenience wrapper for WARN-level structured logs.
pub fn warn(message: &str, context: serde_json::Value) {
    log_json("WARN", message, context);
}

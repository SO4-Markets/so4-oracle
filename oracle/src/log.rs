/// Structured logging — redirected to `tracing` (JSON to stdout via `tracing-subscriber`).

pub fn info(message: &str, context: serde_json::Value) {
    tracing::info!(msg = %message, json = %context, "info");
}

pub fn warn(message: &str, context: serde_json::Value) {
    tracing::warn!(msg = %message, json = %context, "warn");
}

pub fn error(message: &str, context: serde_json::Value) {
    tracing::error!(msg = %message, json = %context, "error");
}

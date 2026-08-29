use std::sync::Arc;

use axum::body::Body;
use axum::http::Request;
use tower::ServiceExt;
use wiremock::MockServer;

mod common;

use common::test_config;
use oracle::api::build_router;
use oracle::state::AppState;

#[tokio::test]
async fn test_request_id_and_completion_logs() {
    let mock_server = MockServer::start().await;
    let config = test_config(&mock_server.uri(), "http://127.0.0.1:9");
    let state = Arc::new(AppState::new(config));

    let app = build_router(Arc::clone(&state));

    // 1. Without x-request-id header -> generates one
    let req1 = Request::builder()
        .uri("/health")
        .body(Body::empty())
        .unwrap();
    let res1 = app.clone().oneshot(req1).await.unwrap();
    println!("Res1 headers: {:#?}", res1.headers());

    let generated_id = res1
        .headers()
        .get("x-request-id")
        .expect("should generate request id");
    assert!(!generated_id.is_empty(), "request id should not be empty");

    // 2. With x-request-id header -> accepts and echoes it
    let req2 = Request::builder()
        .uri("/ready")
        .header("x-request-id", "test-custom-id")
        .body(Body::empty())
        .unwrap();
    let res2 = app.clone().oneshot(req2).await.unwrap();

    let echoed_id = res2
        .headers()
        .get("x-request-id")
        .expect("should echo request id");
    assert_eq!(echoed_id, "test-custom-id", "request id should be echoed");

    // 3. Error response (404 unmatched route) -> still echoes/generates it
    let req3 = Request::builder()
        .uri("/does-not-exist")
        .header("x-request-id", "error-id-123")
        .body(Body::empty())
        .unwrap();
    let res3 = app.clone().oneshot(req3).await.unwrap();
    let error_id = res3
        .headers()
        .get("x-request-id")
        .expect("should echo request id on error");
    assert_eq!(
        error_id, "error-id-123",
        "request id should be echoed on error"
    );
}

#[tokio::test]
async fn test_unmatched_route_metrics_cardinality() {
    let mock_server = MockServer::start().await;
    let config = test_config(&mock_server.uri(), "http://127.0.0.1:9");
    let state = Arc::new(AppState::new(config));

    let app = build_router(Arc::clone(&state));

    // Send a bunch of requests to unmatched routes
    for i in 0..10 {
        let req = Request::builder()
            .uri(format!("/some-random-route-{}", i))
            .body(Body::empty())
            .unwrap();
        let _ = app.clone().oneshot(req).await.unwrap();
    }

    let metrics_out = state.metrics.to_prometheus();

    // We expect 10 requests to be logged under the "/unmatched" route label,
    // not under "/some-random-route-x".
    assert!(metrics_out.contains(
        "oracle_http_requests_total{route=\"/unmatched\",method=\"GET\",status_class=\"4xx\"} 10"
    ));
    assert!(!metrics_out.contains("some-random-route"));
}

#[tokio::test]
async fn test_promtool_validation() {
    let mock_server = MockServer::start().await;
    let config = test_config(&mock_server.uri(), "http://127.0.0.1:9");
    let state = Arc::new(AppState::new(config));

    // Populate some metrics
    state.metrics.record_price_cycle(100, 3, 1);
    state.metrics.record_http_request("/prices", "GET", 200, 15);
    state.metrics.record_http_auth_failure("/oracle/status");

    let metrics_out = state.metrics.to_prometheus();

    let mut child = match std::process::Command::new("promtool")
        .arg("check")
        .arg("metrics")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(_) if std::env::var("CI").is_ok() => {
            panic!(
                "promtool not found in CI — install it in the test job to validate /metrics output"
            );
        }
        Err(_) => {
            println!("promtool not found, skipping validation test");
            return;
        }
    };

    use std::io::Write;
    let mut stdin = child.stdin.take().expect("Failed to open stdin");
    std::thread::spawn(move || {
        stdin
            .write_all(metrics_out.as_bytes())
            .expect("Failed to write to stdin");
    });

    let output = child.wait_with_output().expect("Failed to read stdout");
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        panic!(
            "promtool check metrics failed!\nSTDOUT:\n{}\nSTDERR:\n{}",
            stdout, stderr
        );
    }
}

/// #790 — `trace_layer` runs *after* `SetRequestIdLayer` now, so a request's
/// span carries the real request id instead of an empty string. Drives a
/// request with an explicit `x-request-id` through the real router with a
/// capturing subscriber and asserts that id shows up in the span's structured
/// output. Before the layer reorder this string is absent (the span field is
/// `""`).
#[tokio::test]
async fn trace_span_carries_the_request_id_not_an_empty_string() {
    use std::io::Write;
    use std::sync::Mutex;

    #[derive(Clone)]
    struct CaptureWriter(Arc<Mutex<Vec<u8>>>);
    impl Write for CaptureWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for CaptureWriter {
        type Writer = CaptureWriter;
        fn make_writer(&'a self) -> Self::Writer {
            self.clone()
        }
    }

    let buf = Arc::new(Mutex::new(Vec::<u8>::new()));
    let subscriber = tracing_subscriber::fmt()
        .json()
        .with_env_filter(tracing_subscriber::EnvFilter::new("info"))
        .with_current_span(true)
        .with_span_list(true)
        .with_writer(CaptureWriter(Arc::clone(&buf)))
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);

    let mock_server = MockServer::start().await;
    let config = test_config(&mock_server.uri(), "http://127.0.0.1:9");
    let state = Arc::new(AppState::new(config));
    let app = build_router(state);

    // /oracle/status emits an `info!("request completed")` event inside the
    // request span (health/ready log at debug and would be filtered out here).
    let req = Request::builder()
        .uri("/oracle/status")
        .header("x-request-id", "trace-req-id-790")
        .body(Body::empty())
        .unwrap();
    let _ = app.oneshot(req).await.unwrap();

    let logs = String::from_utf8(buf.lock().unwrap().clone()).unwrap();
    assert!(
        logs.contains("trace-req-id-790"),
        "the trace span should carry the request id; got logs:\n{logs}"
    );
    assert!(
        !logs.contains("\"request_id\":\"\""),
        "the trace span's request_id must not be empty:\n{logs}"
    );
}

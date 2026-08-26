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
            panic!("promtool not found in CI — install it in the test job to validate /metrics output");
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

/// The request ID has to reach the SPAN, not just the response header.
///
/// `test_request_id_and_completion_logs` above asserts the header, which is produced by
/// `PropagateRequestIdLayer` and worked even while every span carried `request_id = ""` — so the
/// header assertion cannot see this bug at all (#790, and the README claim in #813).
///
/// Captures the span's own field rather than parsing formatted output, so it does not depend on the
/// log format staying JSON.
#[tokio::test]
async fn request_id_reaches_the_trace_span_not_only_the_response_header() {
    use std::sync::{Arc as StdArc, Mutex};
    use tracing_subscriber::layer::{Context, SubscriberExt};
    use tracing_subscriber::registry::LookupSpan;
    use tracing_subscriber::Layer;

    #[derive(Default)]
    struct Captured(StdArc<Mutex<Vec<String>>>);

    struct CaptureRequestId(StdArc<Mutex<Vec<String>>>);

    struct Visitor(StdArc<Mutex<Vec<String>>>);
    impl tracing::field::Visit for Visitor {
        // The span records `request_id = %request_id`, i.e. a Display value, which reaches a
        // visitor through record_debug rather than record_str. Both are implemented so the test
        // does not silently stop seeing the field if that changes.
        fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
            if "request_id" == field.name() {
                let rendered = format!("{value:?}");
                self.0
                    .lock()
                    .unwrap()
                    .push(rendered.trim_matches('"').to_string());
            }
        }
        fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
            if "request_id" == field.name() {
                self.0.lock().unwrap().push(value.to_string());
            }
        }
    }

    impl<S: tracing::Subscriber + for<'a> LookupSpan<'a>> Layer<S> for CaptureRequestId {
        fn on_new_span(
            &self,
            attrs: &tracing::span::Attributes<'_>,
            _id: &tracing::span::Id,
            _ctx: Context<'_, S>,
        ) {
            attrs.record(&mut Visitor(StdArc::clone(&self.0)));
        }
    }

    let seen = Captured::default().0;
    let subscriber = tracing_subscriber::registry().with(CaptureRequestId(StdArc::clone(&seen)));

    let mock_server = MockServer::start().await;
    let config = test_config(&mock_server.uri(), "http://127.0.0.1:9");
    let app = build_router(Arc::new(AppState::new(config)));

    let response = tracing::subscriber::with_default(subscriber, || {
        futures::executor::block_on(async {
            app.oneshot(
                Request::builder()
                    .uri("/health")
                    .header("x-request-id", "req-from-the-caller")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap()
        })
    });

    let header = response
        .headers()
        .get("x-request-id")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();

    let ids = seen.lock().unwrap().clone();
    // Guards the guard: no span recorded means the assertion below would pass for free.
    assert!(!ids.is_empty(), "no request span was recorded at all");
    assert!(
        ids.iter().any(|id| !id.is_empty()),
        "every span carried an empty request_id — the trace layer ran before SetRequestIdLayer"
    );
    assert!(
        ids.contains(&header),
        "the span's request_id {ids:?} does not match the response header {header:?}"
    );
}

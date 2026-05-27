use axum::{routing::get, Json, Router};
use serde::Serialize;
use std::env;
use tower_http::cors::{AllowOrigin, CorsLayer};

#[derive(Serialize)]
struct Health {
    status: &'static str,
}

async fn health() -> Json<Health> {
    Json(Health { status: "ok" })
}

fn cors_layer() -> CorsLayer {
    let origins_env = env::var("CORS_ALLOWED_ORIGINS").unwrap_or_default();
    if origins_env.is_empty() || origins_env == "*" {
        CorsLayer::permissive()
    } else {
        let origins: Vec<_> = origins_env
            .split(',')
            .filter_map(|o| o.trim().parse().ok())
            .collect();
        CorsLayer::new().allow_origin(AllowOrigin::list(origins))
    }
}

#[tokio::main]
async fn main() {
    let app = Router::new()
        .route("/health", get(health))
        .layer(cors_layer());
    let listener = tokio::net::TcpListener::bind("0.0.0.0:3000").await.unwrap();
    println!("listening on {}", listener.local_addr().unwrap());
    axum::serve(listener, app).await.unwrap();
}

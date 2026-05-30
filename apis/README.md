# APIs Server (`apis/`)

Axum-based REST API server for SO4 Markets oracle data.

## Endpoints

| Method | Path | Description |
|---|---|---|
| `GET` | `/health` | Health check — returns `{"status":"ok"}` |
| `GET` | `/prices/:token` | Latest min/max price for a token (case-insensitive) |
| `GET` | `/prices/:token/history` | OHLCV candles — query params: `interval` (1m/5m/1h), `from`, `to` |
| `GET` | `/markets` | All active markets with pool stats |
| `GET` | `/markets/:market_id` | Single market detail with top positions |
| `GET` | `/positions/:account` | Open positions for a Stellar account (with PnL) |
| `GET` | `/oracle/status` | Oracle health — 200 if fresh, 503 if stale (>5 min) |
| `POST` | `/oracle/status` | Update oracle status (called by the oracle worker) |
| `GET` | `/openapi.json` | Auto-generated OpenAPI 3.1 spec |
| `GET` | `/docs` | Swagger UI (loads from CDN) |
| `GET` | `/admin/hello` | Admin-only endpoint (requires `Authorization: Bearer <API_KEY>`) |

> The full OpenAPI spec at `/openapi.json` is the source of truth for the API contract.

## Running

```bash
cargo run -p apis
# → listening on 0.0.0.0:3000
```

## Environment Variables

| Variable | Description |
|---|---|
| `PORT` | Listen port (default `3000`) |
| `API_KEY` | Bearer token for `/admin` routes |
| `CORS_ALLOWED_ORIGINS` | Comma-separated allowed origins (dev: `*`) |
| `APP_ENV` | Set to `production` to enforce strict CORS |
| `LOG_FORMAT` | `json` or `pretty` (default: `pretty`) |
| `RUST_LOG` | Log level filter (default: `apis=info,warn`) |

## Testing

```bash
cargo test -p apis
```

Uses mock `Reader` implementations — no external dependencies required.

## Architecture

- **Cache:** In-memory TTL cache (`tokio::sync::RwLock<HashMap>`)
- **History:** In-memory ring buffer of price ticks, aggregated into OHLCV candles at query time
- **CORS:** Configurable via `CORS_ALLOWED_ORIGINS`; permissive in dev, strict in production
- **OpenAPI:** Auto-generated from `#[utoipa::path]` annotations via `utoipa`

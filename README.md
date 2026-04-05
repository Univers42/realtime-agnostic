# Realtime Agnostic

A **database-agnostic, horizontally scalable, Rust-native realtime event routing engine**.

Supports PostgreSQL (via LISTEN/NOTIFY) and MongoDB (via Change Streams) out of the box, with a pluggable trait system for adding any database backend.

## Architecture

```
┌─────────────┐     ┌──────────────┐     ┌───────────────┐
│  DB Producer │────▶│  Event Bus   │────▶│  Event Router │
│ (PG / Mongo) │     │ (in-process) │     │ (registry +   │
└─────────────┘     └──────────────┘     │  filter eval) │
                                          └──────┬────────┘
                                                 │
                                          ┌──────▼────────┐
                                          │  Fan-Out Pool  │
                                          │  (N workers)   │
                                          └──────┬────────┘
                                                 │
                                    ┌────────────┼────────────┐
                                    ▼            ▼            ▼
                              ┌──────────┐ ┌──────────┐ ┌──────────┐
                              │ WS Conn  │ │ WS Conn  │ │ WS Conn  │
                              └──────────┘ └──────────┘ └──────────┘
```

## Crates

| Crate | Description |
|---|---|
| `realtime-core` | Core types, traits, protocol, filter expressions |
| `realtime-engine` | Subscription registry, bitmap filter index, event router, sequence generator |
| `realtime-bus-inprocess` | In-process event bus (tokio broadcast channel) |
| `realtime-auth` | Pluggable auth: JWT (HS256/RS256) + NoAuth providers |
| `realtime-gateway` | WebSocket gateway, connection manager, fan-out, REST API |
| `realtime-db-postgres` | PostgreSQL CDC via LISTEN/NOTIFY |
| `realtime-db-mongodb` | MongoDB CDC via Change Streams |
| `realtime-server` | Main binary assembling all components |
| `realtime-client` | Client SDK with auto-reconnect and dedup |

## Quick Start

### Prerequisites

- Rust 1.75+ (tested on 1.89)
- Docker & Docker Compose (for sandbox demo)

### Build

```bash
cargo build --workspace
```

### Test

```bash
cargo test --workspace
```

All 78 tests pass with zero warnings — 54 unit tests + 24 integration tests covering:
- Health endpoint, REST publish/batch, WebSocket connect/auth/subscribe/receive
- Unsubscribe, multiple subscribers, prefix patterns, batch subscribe
- Filter evaluation (field-level and payload JSON extraction)
- Connection lifecycle, sequence generator, serialization
- JWT auth, NoAuth, throughput (100+ events), concurrent connections (10 clients)

### Run the Server

```bash
# With environment variables
RUST_LOG=info \
AUTH_MODE=noauth \
LISTEN_ADDR=0.0.0.0:4000 \
cargo run --bin realtime-server
```

### Sandbox Demo (Docker Compose)

Launches PostgreSQL, MongoDB (replica set), and the realtime server with a web UI:

```bash
docker compose up --build
```

Then open http://localhost:4000 for the interactive demo frontend.

## Protocol

### WebSocket Messages (Client → Server)

```json
{ "type": "AUTH", "token": "jwt-or-any-token" }
{ "type": "SUBSCRIBE", "sub_id": "my-sub", "topic": "orders/*" }
{ "type": "SUBSCRIBE_BATCH", "subscriptions": [{ "sub_id": "s1", "topic": "t1" }, ...] }
{ "type": "UNSUBSCRIBE", "sub_id": "my-sub" }
{ "type": "PING" }
```

### WebSocket Messages (Server → Client)

```json
{ "type": "AUTH_OK", "conn_id": 1 }
{ "type": "SUBSCRIBED", "sub_id": "my-sub" }
{ "type": "EVENT", "event": { "topic": "...", "event_type": "...", "payload": {...}, "seq": 1 } }
{ "type": "PONG" }
{ "type": "ERROR", "message": "..." }
```

### REST API

| Method | Path | Description |
|---|---|---|
| `POST` | `/v1/publish` | Publish a single event |
| `POST` | `/v1/publish/batch` | Publish multiple events |
| `GET` | `/v1/health` | Health check |

## Topic Patterns

- **Exact**: `orders/created` — matches only `orders/created`
- **Prefix**: `orders/*` — matches any topic starting with `orders/`
- **Glob**: `db/*/updated` — matches `db/users/updated`, `db/orders/updated`, etc.

## Filter Expressions

Subscriptions can include filters that evaluate against event fields:

```json
{
  "type": "SUBSCRIBE",
  "sub_id": "filtered",
  "topic": "orders/*",
  "filter": { "eq": ["event_type", "created"] }
}
```

Supported fields: `event_type`, `topic`, `source.kind`, `source.id`, `source.metadata.*`, `payload.*` (JSON path extraction).

Operators: `eq`, `ne`, `in`, `and`, `or`, `not`.

## Adding a New Database Backend

1. Implement the `DatabaseProducer` trait from `realtime-core`:

```rust
#[async_trait]
impl DatabaseProducer for MyDbProducer {
    async fn start(&self, publisher: Arc<dyn EventBusPublisher>) -> Result<(), RealtimeError>;
    async fn stop(&self) -> Result<(), RealtimeError>;
    async fn health_check(&self) -> Result<(), RealtimeError>;
}
```

2. Parse database change events into `EventEnvelope` and publish to the bus.

## License

MIT

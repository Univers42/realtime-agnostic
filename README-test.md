# SyncSpace — Realtime-Agnostic Test Harness

A real-time collaborative workspace (Trello boards + Discord channels)
purpose-built to verify that the `realtime-agnostic` module works correctly
with **PostgreSQL**, **MongoDB**, and any future adapter.

## Quick start

```bash
# Start databases
docker-compose up -d

# Install
npm install

# Schema + seed
psql postgresql://syncspace:syncspace@localhost:5432/syncspace \
  -f apps/api/src/db/schema.sql
npm run seed

# Run
npm run dev
```

Open http://localhost:5173. Log in as any seeded user:
- alice@syncspace.dev / password123
- bob@syncspace.dev  / password123
- carol@syncspace.dev / password123

## Test real-time

Open two tabs, log in as different users.
The **debug panel** (bottom-right, dev mode) shows every event in real-time
and lets you hot-swap between PostgreSQL and MongoDB adapters.

See `PROMPT.md` for the full test matrix, architecture, and extension guide.

## Adapter switch

```bash
# Use PostgreSQL for the event log
RT_DB_ADAPTER=postgres npm run dev -w apps/api

# Use MongoDB for the event log
RT_DB_ADAPTER=mongo npm run dev -w apps/api

# Or switch at runtime via the debug panel (no restart needed)
```

## Adding a new database adapter

1. Implement `DatabaseAdapter` from `@syncspace/realtime-agnostic`
2. Pass it to `new RealtimeManager({ db: yourAdapter, transport })`
3. Done — the entire app works without any other changes

See `PROMPT.md` → "Extending the adapter" for a full SQLite example.
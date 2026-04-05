# SyncSpace — Full-Stack Prompt for Any LLM Agent

## What this is

SyncSpace is a real-time collaborative workspace. Think Trello boards + Discord
channels, running in the same app. It is the ideal stress-test for a
`realtime-agnostic` module because **every user interaction must be visible
to every other connected user within 100ms**, with no polling.

The app is intentionally built to fail spectacularly if real-time is broken:
- Drag a card → no one else sees it move
- Type a message → it never appears in other tabs
- Join a board → other users don't know you're there
- Disconnect and reconnect → you miss events (tests replay)

The entire sandbox lives in `sandbox/`. Do not create files outside it.

---

## Application overview

```
sandbox/
├── docker-compose.yml              # PostgreSQL 16 + MongoDB 7
├── apps/
│   ├── api/                        # Express + WebSocket server (Node/TypeScript)
│   │   └── src/
│   │       ├── index.ts            # ← wires RealtimeManager with chosen adapter
│   │       ├── ws/handler.ts       # WebSocket lifecycle: auth, subscribe, replay
│   │       ├── routes/
│   │       │   ├── auth.ts         # POST /api/auth/login, /register
│   │       │   ├── boards.ts       # CRUD boards + lists
│   │       │   ├── cards.ts        # CRUD cards + move (publishes RT events)
│   │       │   ├── channels.ts     # Chat messages (MongoDB) + typing indicator
│   │       │   └── debug.ts        # POST /api/debug/switch-adapter (dev only)
│   │       ├── db/
│   │       │   ├── postgres.ts     # pg.Pool singleton
│   │       │   ├── mongo.ts        # MongoClient + Db singleton
│   │       │   └── schema.sql      # Full Postgres schema + indexes
│   │       ├── middleware/auth.ts  # JWT verify middleware
│   │       └── scripts/seed.ts    # npm run seed
│   └── client/                     # React + Vite (TypeScript)
│       └── src/
│           ├── hooks/useRealtime.ts      # ← single WS connection + event dispatch
│           ├── stores/
│           │   ├── boardStore.ts         # Zustand: lists, cards, cursors, presence
│           │   ├── chatStore.ts          # Zustand: messages, typing indicators
│           │   └── authStore.ts         # Zustand: token, user
│           └── components/
│               ├── board/
│               │   ├── KanbanBoard.tsx  # DnD board, mouse move → cursor broadcast
│               │   ├── KanbanList.tsx   # Droppable column
│               │   ├── KanbanCard.tsx   # Draggable card
│               │   ├── LiveCursors.tsx  # Coloured cursor overlays
│               │   ├── PresenceBar.tsx  # Online user avatars
│               │   └── AddListButton.tsx
│               ├── chat/
│               │   └── ChatPanel.tsx    # Discord-like channel chat
│               └── debug/
│                   └── RealtimeDebugPanel.tsx  # ← THE testing UI
└── packages/
    └── realtime-agnostic/          # ← THE MODULE UNDER TEST
        ├── index.ts
        └── src/
            ├── types.ts            # All interfaces: DatabaseAdapter, TransportAdapter, payloads
            ├── RealtimeManager.ts  # publish(), replay(), join(), leave()
            └── adapters/
                ├── PostgresAdapter.ts   # Implements DatabaseAdapter with pg
                ├── MongoAdapter.ts      # Implements DatabaseAdapter with MongoDB driver
                └── WebSocketTransport.ts # Implements TransportAdapter with ws
```

---

## The module under test: `realtime-agnostic`

```typescript
// The only import the API server needs:
import { RealtimeManager, PostgresAdapter, MongoAdapter } from "@syncspace/realtime-agnostic";

// Wire it up once in apps/api/src/index.ts:
const rt = new RealtimeManager({
  db: new PostgresAdapter({ query: (sql, p) => pool.query(sql, p) }),
  transport: wsTransport,
});

// Publish from any route handler:
await rt.publish("board:abc123", "card.moved", payload, userId);

// Switch to MongoDB — ZERO other code changes:
const rt2 = new RealtimeManager({
  db: new MongoAdapter({ eventsCollection, presenceCollection }),
  transport: wsTransport,
});
```

### DatabaseAdapter interface (what you implement per database)

```typescript
interface DatabaseAdapter {
  persist(event: RealtimeEvent): Promise<void>;
  getHistory(channel: string, since: number, limit?: number): Promise<RealtimeEvent[]>;
  setPresence(channel: string, userId: string, online: boolean): Promise<void>;
  getPresence(channel: string): Promise<string[]>;
}
```

To add MySQL support: implement this interface with `mysql2/promise`.
To add SQLite: implement it with `better-sqlite3`.
The rest of the app never changes.

---

## Real-time channels and events

| Channel pattern             | Events                                                      |
|-----------------------------|-------------------------------------------------------------|
| `board:{boardId}`           | card.created, card.updated, card.moved, card.deleted        |
|                             | list.created, list.updated, list.deleted                    |
|                             | user.joined_board, user.left_board                          |
| `board:{boardId}:cursors`   | cursor.moved  *(ephemeral — never persisted)*               |
| `channel:{id}:chat`         | message.created, message.updated, message.deleted           |
|                             | user.typing, user.stopped_typing  *(ephemeral)*             |
| `user:{userId}`             | notification.created                                        |

Ephemeral events (cursor.moved, user.typing) are broadcast via transport but
NOT written to the event log. This is enforced in RealtimeManager.publish().

---

## WebSocket protocol (client ↔ server)

```
Client → Server                         Server → Client
──────────────────────────────────────────────────────────────────
{ type: "auth", token }                 { type: "auth_ok", userId }
                                        { type: "auth_error", message }

{ type: "subscribe",                    { type: "subscribed",
  channel,                                channel,
  lastTimestamp? }   ──replay──►          onlineUsers: string[] }
                                        { type: "event", ...RealtimeEvent }

{ type: "unsubscribe", channel }

{ type: "pong" }                        { type: "ping" }  (every 15s)
```

Reconnect flow:
1. Client reconnects, authenticates
2. Client sends `{ type: "subscribe", channel, lastTimestamp: <last seen> }`
3. Server calls `rt.replay(channel, lastTimestamp, clientId, handler)`
4. Handler streams missed events directly to the client WS

---

## Data split: PostgreSQL vs MongoDB

| Data                          | Database   | Why                                      |
|-------------------------------|------------|------------------------------------------|
| Users, workspaces, boards     | PostgreSQL | Relational, strong consistency, ACID     |
| Lists, cards                  | PostgreSQL | Ordered, positional, FK constraints      |
| Board membership, channels    | PostgreSQL | Joins with users                         |
| realtime_events (event log)   | PostgreSQL | OR MongoDB — this is the adapter test    |
| realtime_presence             | PostgreSQL | OR MongoDB — this is the adapter test    |
| Chat messages                 | MongoDB    | Schema-flexible, append-heavy, no joins  |
| Reactions, attachments        | MongoDB    | Nested documents, frequent schema change |

---

## How to run

```bash
# 1. Start databases
cd sandbox
docker-compose up -d

# 2. Install deps (from monorepo root)
npm install

# 3. Configure API
cp apps/api/.env.example apps/api/.env
# Edit RT_DB_ADAPTER=postgres  (or mongo)

# 4. Run schema migration
psql postgresql://syncspace:syncspace@localhost:5432/syncspace \
  -f apps/api/src/db/schema.sql

# 5. Seed demo data
npm run seed

# 6. Start everything
npm run dev
# API:    http://localhost:4000
# Client: http://localhost:5173
```

---

## Verifying real-time is working (test matrix)

Open TWO browser tabs, log in as different users (alice / bob / carol).

### Test 1 — Card CRUD
- Tab A: Create a card in any list
- Tab B: Card should appear instantly, no refresh

### Test 2 — Drag and drop
- Tab A: Drag a card to a different list
- Tab B: Card should move instantly in the same visual position

### Test 3 — Live cursors
- Tab A: Move mouse around the board
- Tab B: A colored cursor with Tab A's name should follow the movement

### Test 4 — Presence
- Tab A: Join a board
- Tab B: Tab A's avatar should appear in the PresenceBar instantly

### Test 5 — Chat
- Tab A: Open #general channel, type a message
- Tab B: Message should appear in real-time
- Tab A: Start typing (do NOT send)
- Tab B: Typing indicator should appear within 1 second, disappear after 4s

### Test 6 — Reconnect replay
- Tab A: Disconnect network (DevTools → Network → Offline)
- Tab B: Create 3 cards, move 1
- Tab A: Reconnect network
- Tab A: All 3 cards + the move should appear (replayed from event log)

### Test 7 — Adapter switch (the core test)
- Open the RealtimeDebugPanel (bottom-right, dev mode)
- Click "PostgreSQL" — events shown with blue badge
- Do some actions: create card, move card, send message
- Click "MongoDB" — events now shown with green badge
- Repeat the same actions
- EXPECTED: behavior identical in both modes
- VERIFY: events appear in the debug panel for both adapters

---

## What to build next (missing implementations)

The following files are referenced but not yet implemented.
Build them in this order:

### Priority 1 — Required to run

1. **`apps/api/src/db/postgres.ts`**
   ```typescript
   import { Pool } from "pg";
   export const pool = new Pool({ connectionString: process.env.DATABASE_URL });
   ```

2. **`apps/api/src/db/mongo.ts`**
   ```typescript
   import { MongoClient } from "mongodb";
   const client = new MongoClient(process.env.MONGO_URL!);
   await client.connect();
   export const mongo = client.db("syncspace");
   ```

3. **`apps/api/src/middleware/auth.ts`**
   - Verify JWT from `Authorization: Bearer <token>` header
   - Attach `{ userId, userName, avatar }` to `req.user`

4. **`apps/api/src/routes/auth.ts`**
   - `POST /api/auth/register` → bcrypt hash, insert user, return JWT
   - `POST /api/auth/login` → verify password, return JWT

5. **`apps/api/src/routes/boards.ts`**
   - `GET /api/boards` → list boards for current user
   - `POST /api/boards` → create board, publish `board.created`
   - `GET /api/boards/:boardId` → full board with lists + cards
   - `POST /api/boards/:boardId/lists` → create list, publish `list.created`
   - `POST /api/boards/:boardId/cursor` → publish `cursor.moved` (ephemeral)

### Priority 2 — UI (React components)

6. **`apps/client/src/components/board/KanbanList.tsx`**
   - `useDroppable` from @dnd-kit/core
   - Renders a sorted list of `<KanbanCard>` from `boardStore.lists[listId].cardIds`

7. **`apps/client/src/components/board/KanbanCard.tsx`**
   - `useDraggable` from @dnd-kit/core
   - Shows title, assignee avatars, label dots, due date badge

8. **`apps/client/src/components/board/AddListButton.tsx`**
   - Inline input that calls `POST /api/boards/:boardId/lists`

9. **`apps/client/src/App.tsx`**
   - React Router routes: `/login`, `/boards`, `/boards/:boardId`
   - Mounts `<RealtimeDebugPanel />` when `import.meta.env.DEV`

10. **`apps/client/src/pages/BoardPage.tsx`**
    - Splits layout: left sidebar (channel list), center (KanbanBoard), optional right (ChatPanel)
    - Fetches board data on mount, calls `boardStore.hydrate()`

### Priority 3 — Nice to have

11. **MySQL adapter** — implement `DatabaseAdapter` using `mysql2/promise`
    - Same 4 methods: persist, getHistory, setPresence, getPresence
    - Table schema: same columns as the Postgres schema, MySQL syntax

12. **Card detail modal** — click a card to open full editor
    - Markdown description editor
    - Assignee picker (shows online users first)
    - Label picker, due date picker
    - Comment thread (stored in MongoDB)
    - All changes publish `card.updated` in real-time

13. **Notifications** — when a card is assigned to you
    - Server publishes to `user:{userId}` channel
    - Client shows toast via `syncspace:notification` custom event

---

## Extending the adapter (adding a new database)

To add SQLite (for testing without Docker):

```typescript
// packages/realtime-agnostic/src/adapters/SqliteAdapter.ts
import Database from "better-sqlite3";
import type { DatabaseAdapter, Channel, RealtimeEvent } from "../types";

export class SqliteAdapter implements DatabaseAdapter {
  private db: Database.Database;

  constructor(dbPath: string) {
    this.db = new Database(dbPath);
    this.db.exec(`
      CREATE TABLE IF NOT EXISTS realtime_events (
        id TEXT PRIMARY KEY,
        channel TEXT NOT NULL,
        event TEXT NOT NULL,
        payload TEXT NOT NULL,
        actor_id TEXT NOT NULL,
        timestamp INTEGER NOT NULL
      );
      CREATE INDEX IF NOT EXISTS idx_ch_ts ON realtime_events(channel, timestamp);
      CREATE TABLE IF NOT EXISTS realtime_presence (
        channel TEXT NOT NULL,
        user_id TEXT NOT NULL,
        online INTEGER NOT NULL DEFAULT 0,
        last_seen INTEGER NOT NULL,
        PRIMARY KEY (channel, user_id)
      );
    `);
  }

  async persist(event: RealtimeEvent): Promise<void> {
    this.db.prepare(`
      INSERT OR IGNORE INTO realtime_events VALUES (?,?,?,?,?,?)
    `).run(event.id, event.channel, event.event, JSON.stringify(event.payload), event.actorId, event.timestamp);
  }

  async getHistory(channel: Channel, since: number, limit = 500): Promise<RealtimeEvent[]> {
    const rows = this.db.prepare(`
      SELECT * FROM realtime_events
      WHERE channel = ? AND timestamp > ?
      ORDER BY timestamp ASC LIMIT ?
    `).all(channel, since, limit) as any[];
    return rows.map(r => ({ ...r, payload: JSON.parse(r.payload), actorId: r.actor_id }));
  }

  async setPresence(channel: Channel, userId: string, online: boolean): Promise<void> {
    this.db.prepare(`
      INSERT OR REPLACE INTO realtime_presence VALUES (?,?,?,?)
    `).run(channel, userId, online ? 1 : 0, Date.now());
  }

  async getPresence(channel: Channel): Promise<string[]> {
    const cutoff = Date.now() - 5 * 60 * 1000;
    const rows = this.db.prepare(`
      SELECT user_id FROM realtime_presence
      WHERE channel = ? AND online = 1 AND last_seen > ?
    `).all(channel, cutoff) as { user_id: string }[];
    return rows.map(r => r.user_id);
  }
}
```

Then in `apps/api/src/index.ts`:
```typescript
const rt = new RealtimeManager({
  db: new SqliteAdapter("./dev.db"),
  transport: wsTransport,
});
```

---

## Architecture diagram

```
Browser Tab A                     Node.js API Server                 Browser Tab B
──────────────                    ──────────────────                 ──────────────
useRealtime hook                                                     useRealtime hook
  │                                                                       │
  │ WS connect + auth                                        WS connect + auth │
  │──────────────────►  WebSocketTransport                ◄──────────────────│
  │                            │                                            │
  │ { type:"subscribe",        │                                            │
  │   channel:"board:abc" }    │                                            │
  │──────────────────►  ws/handler.ts                                       │
  │                     transport.addClientToChannel()                      │
  │                     rt.join(channel, userId)                            │
  │                     rt.replay(channel, lastTs, ...)                     │
  │                            │                                            │
  │ drag card ──► PATCH /api/cards/:id/move                                 │
  │                     ↓                                                   │
  │               pool.query(UPDATE cards …)  ◄── PostgreSQL                │
  │                     ↓                                                   │
  │               rt.publish("board:abc",                                   │
  │                 "card.moved", payload)                                  │
  │                     ↓                                                   │
  │               adapter.persist(event) ◄── PostgresAdapter OR MongoAdapter│
  │                     ↓                                                   │
  │               transport.broadcast("board:abc", event)                   │
  │                     │                                                   │
  │◄──────────────────  │  ──────────────────────────────────────────────►  │
  { type:"event",       │       { type:"event", event:"card.moved", … }     │
    event:"card.moved"} │                                                   │
  boardStore.moveCard() │                                               boardStore.moveCard()
  card moves in UI ✓    │                                               card moves in UI ✓
```

---

## Key design decisions explained

**Why is the event log in the DB adapter and not in a message broker?**
To keep the module self-contained and testable without Redis/Kafka.
The `DatabaseAdapter.persist()` / `getHistory()` pair acts as a simple
durable event log that enables replay on reconnect. For production scale,
you would replace the `WebSocketTransport` with a Redis pub/sub transport
adapter — the `RealtimeManager` API stays identical.

**Why split messages between Postgres and MongoDB?**
Cards and lists have strict ordering constraints and foreign key relationships
— Postgres is the right tool. Chat messages are append-only, schema-flexible
(reactions, thread replies, rich embeds), and never joined — MongoDB is
the right tool. This also forces the realtime-agnostic module to prove it
works when two different databases power different parts of the same app.

**Why are cursors ephemeral?**
Cursor positions update at 20fps. Persisting them would write ~1200 rows/min
per active user. The `RealtimeManager` skips `persist()` for events in the
`EPHEMERAL_EVENTS` set. Cursors are only meaningful in the moment — there is
no value in replaying "the mouse was at position 0.3, 0.7 five minutes ago".

**Why does the debug panel hot-swap adapters?**
It's the fastest way to verify your module's agnosticism without restarting
the server. Make some actions, switch adapter, make the same actions, compare
the event logs. If behavior is identical, your module is truly agnostic.
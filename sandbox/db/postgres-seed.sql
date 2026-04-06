-- =============================================================================
-- SyncSpace: Seed Data
-- =============================================================================

-- ─── Users ──────────────────────────────────────────────────────────────
INSERT INTO users (id, username, email, avatar_url, color) VALUES
  ('user-alice',  'alice',  'alice@syncspace.dev',  NULL, '#ef4444'),
  ('user-bob',    'bob',    'bob@syncspace.dev',    NULL, '#3b82f6'),
  ('user-carol',  'carol',  'carol@syncspace.dev',  NULL, '#22c55e')
ON CONFLICT DO NOTHING;

-- ─── Workspace ──────────────────────────────────────────────────────────
INSERT INTO workspaces (id, name, owner_id) VALUES
  ('ws-main', 'SyncSpace Demo', 'user-alice')
ON CONFLICT DO NOTHING;

-- ─── Board: Product Roadmap ─────────────────────────────────────────────
INSERT INTO boards (id, workspace_id, name, description, bg_color) VALUES
  ('board-roadmap', 'ws-main', 'Product Roadmap', 'Q2 2026 feature planning', '#1e1b4b')
ON CONFLICT DO NOTHING;

INSERT INTO board_members (board_id, user_id, role) VALUES
  ('board-roadmap', 'user-alice', 'admin'),
  ('board-roadmap', 'user-bob',   'member'),
  ('board-roadmap', 'user-carol', 'member')
ON CONFLICT DO NOTHING;

-- ─── Lists ──────────────────────────────────────────────────────────────
INSERT INTO lists (id, board_id, title, position) VALUES
  ('list-backlog',     'board-roadmap', 'Backlog',      0),
  ('list-in-progress', 'board-roadmap', 'In Progress',  1),
  ('list-review',      'board-roadmap', 'In Review',    2),
  ('list-done',        'board-roadmap', 'Done',         3)
ON CONFLICT DO NOTHING;

-- ─── Cards ──────────────────────────────────────────────────────────────
INSERT INTO cards (id, list_id, board_id, title, description, position, assignee_id, label_color, created_by) VALUES
  ('card-1', 'list-backlog', 'board-roadmap', 'WebSocket reconnect with replay',
   'Implement automatic reconnection with event replay from last timestamp',
   0, 'user-alice', '#ef4444', 'user-alice'),

  ('card-2', 'list-backlog', 'board-roadmap', 'Add MySQL adapter',
   'Implement DatabaseAdapter for MySQL using mysql2',
   1, NULL, '#f97316', 'user-bob'),

  ('card-3', 'list-backlog', 'board-roadmap', 'Rate limiting per subscription',
   'Add configurable rate limits to prevent event flooding',
   2, 'user-carol', '#eab308', 'user-carol'),

  ('card-4', 'list-in-progress', 'board-roadmap', 'Live cursor overlays',
   'Show other users'' cursor positions on the board in real-time',
   0, 'user-bob', '#3b82f6', 'user-alice'),

  ('card-5', 'list-in-progress', 'board-roadmap', 'Chat message reactions',
   'Allow emoji reactions on chat messages stored in MongoDB',
   1, 'user-carol', '#8b5cf6', 'user-bob'),

  ('card-6', 'list-review', 'board-roadmap', 'Presence indicators',
   'Show online/offline status of board members',
   0, 'user-alice', '#22c55e', 'user-carol'),

  ('card-7', 'list-done', 'board-roadmap', 'PostgreSQL CDC integration',
   'LISTEN/NOTIFY based change data capture working end-to-end',
   0, 'user-alice', '#22c55e', 'user-alice'),

  ('card-8', 'list-done', 'board-roadmap', 'MongoDB change streams',
   'Change streams integration with event publishing',
   1, 'user-bob', '#22c55e', 'user-bob')
ON CONFLICT DO NOTHING;

-- ─── Chat Channels ──────────────────────────────────────────────────────
INSERT INTO channels (id, board_id, name) VALUES
  ('ch-general', 'board-roadmap', 'general'),
  ('ch-random',  'board-roadmap', 'random')
ON CONFLICT DO NOTHING;

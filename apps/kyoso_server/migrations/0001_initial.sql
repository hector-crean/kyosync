-- Initial kyoso_server schema.
--
-- One row per document/room. Snapshots and ops belong to a single room,
-- as do per-peer ack high-water marks used for log compaction.

CREATE TABLE rooms (
    id              TEXT        PRIMARY KEY,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    -- Next global_seq to assign on the next op append. Bumped atomically
    -- inside the append CTE so concurrent appends never collide on the
    -- (room_id, global_seq) primary key.
    next_seq        BIGINT      NOT NULL DEFAULT 1,
    -- Highest seq covered by any persisted snapshot for this room.
    snapshot_seq    BIGINT      NOT NULL DEFAULT 0,
    -- Ops with global_seq <= compacted_below have been deleted from the
    -- ops table (and are recoverable only through the snapshot at or
    -- above compacted_below).
    compacted_below BIGINT      NOT NULL DEFAULT 0
);

-- The op log. (room_id, global_seq) is unique and dense — global_seq
-- starts at 1 and increments by 1 per op within a room.
CREATE TABLE ops (
    room_id     TEXT        NOT NULL REFERENCES rooms(id) ON DELETE CASCADE,
    global_seq  BIGINT      NOT NULL,
    op_blob     BYTEA       NOT NULL,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (room_id, global_seq)
);

-- Periodic checkpoints. Late joiners receive the most recent snapshot
-- (if any) plus ops since `at_seq`. Multiple snapshots per room are
-- allowed; old ones can be pruned by the GC scheduler.
CREATE TABLE snapshots (
    room_id    TEXT        NOT NULL REFERENCES rooms(id) ON DELETE CASCADE,
    at_seq     BIGINT      NOT NULL,
    blob       BYTEA       NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (room_id, at_seq)
);

-- Per-peer high-water mark. The minimum of last_seen_seq across all
-- currently-connected peers for a room is the safe-to-compact threshold.
-- Rows are cleared on peer disconnect so stale clients don't block GC.
CREATE TABLE peer_acks (
    room_id       TEXT        NOT NULL REFERENCES rooms(id) ON DELETE CASCADE,
    peer_id       BIGINT      NOT NULL,
    last_seen_seq BIGINT      NOT NULL,
    updated_at    TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (room_id, peer_id)
);

CREATE INDEX idx_ops_room_seq        ON ops(room_id, global_seq);
CREATE INDEX idx_snapshots_room_seq  ON snapshots(room_id, at_seq DESC);

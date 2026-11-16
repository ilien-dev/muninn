-- Muninn schema v1. Transcribed from design/ENGINE.md §2.
-- SQLite is the operational truth; Markdown under .muninn/records/ is the portable one.

CREATE TABLE IF NOT EXISTS meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

-- 2.1 record: the typed ledger. Literal storage, never rewritten, never deleted.
CREATE TABLE IF NOT EXISTS record (
    id              INTEGER PRIMARY KEY,
    kind            TEXT    NOT NULL CHECK (kind IN ('invariant','decision','deadend','correction','claim','episode')),
    subject         TEXT    NOT NULL,
    relation        TEXT    NOT NULL,
    object          TEXT    NOT NULL,
    body            TEXT    NOT NULL CHECK (length(body) <= 2000),
    origin          TEXT    NOT NULL CHECK (origin IN ('user_said','review_accepted','commit_linked','tool_observed','agent_inferred','imported')),
    trust           INTEGER NOT NULL CHECK (trust BETWEEN 0 AND 3),
    anchor_path     TEXT,
    anchor_hash     TEXT,
    session_id      TEXT    NOT NULL,
    transcript_ref  TEXT,
    dedup_hash      TEXT    NOT NULL,
    created_at      INTEGER NOT NULL,
    invalid         INTEGER NOT NULL DEFAULT 0 CHECK (invalid IN (0,1)),
    invalidated_by  INTEGER REFERENCES record(id),
    invalid_reason  TEXT    CHECK (invalid_reason IS NULL OR invalid_reason IN ('superseded','revoked','anchor_changed','reverted','user','cap')),
    -- v3: when a row was retired (epoch ms) and by which mechanism (`said`, `quantity`, `ack`,
    -- `key`, `swap:<commit>`, `gone:<word>`, `revert:<commit>`, `restated`, …;
    -- `agent_unrestored` marks a row that an agent's prompt retired and that was not given
    -- back because something was said or done to the store since:
    -- `muninn-capture/src/agent/undo.rs`). Both are set by
    -- the triggers below when the retiring statement leaves them out, and cleared when the
    -- row is put back. A store migrated from v2 has NULL in both for its older retirements.
    invalidated_at  INTEGER,
    invalidated_via TEXT
);
CREATE INDEX IF NOT EXISTS record_active_key ON record(subject, relation) WHERE invalid = 0;
-- the same key with the kind, for the catalogue's conflict test: a record whose key no other
-- active record of its kind shares is answered from the index, without reading the others' rows
CREATE INDEX IF NOT EXISTS record_active_kind_key ON record(subject, relation, kind) WHERE invalid = 0;
CREATE INDEX IF NOT EXISTS record_anchor     ON record(anchor_path) WHERE anchor_path IS NOT NULL;
CREATE INDEX IF NOT EXISTS record_kind       ON record(kind) WHERE invalid = 0;
-- newest-first over the served rows: the catalogue and any recency query read the top of
-- this index instead of sorting the whole active set. On a store at the schema's cap that
-- sort was 3.5 ms of a 10 ms hook; through the index it is 0.08 ms.
CREATE INDEX IF NOT EXISTS record_recent     ON record(created_at DESC, id DESC) WHERE invalid = 0;
-- who retired whom, for the session catalogue's `replaces #n`
CREATE INDEX IF NOT EXISTS record_heir       ON record(invalidated_by) WHERE invalidated_by IS NOT NULL;
CREATE UNIQUE INDEX IF NOT EXISTS record_dedup ON record(dedup_hash);
-- the retired rows of a turn, for the read path's stale-twin test (`recall::recall`): an
-- episode whose turn's statement was replaced is not served
CREATE INDEX IF NOT EXISTS record_retired_ref ON record(transcript_ref) WHERE invalid = 1;
-- The active rows of a turn. When a prompt is found to be an agent's, what its turn wrote is
-- read by turn, active rows through this index and retired ones through the one above
-- (`muninn-capture/src/agent.rs`). Partial on purpose: an index over every row would also
-- serve the read path's query above and change its plan. Write path only. `record_ref` was
-- that wider index, in one build of this schema.
DROP INDEX IF EXISTS record_ref;
CREATE INDEX IF NOT EXISTS record_active_ref ON record(transcript_ref) WHERE invalid = 0;

-- F1's gate, in the schema rather than in every query (ENGINE.md §5). This view is the
-- ONLY source a serving path may read from: what is selected here is what may reach the
-- agent. `invalid`, `invalidated_by` and `invalid_reason` are left out on purpose — a
-- serving path has no use for them, and their absence turns a forgotten `WHERE invalid = 0`
-- into a query that fails to prepare instead of one that silently serves a retired record.
-- The paths that must see retired rows (`muninn why --all`, `export --all`, the Markdown
-- mirror, lineage, and the whole write path) read `record` directly and say so.
CREATE VIEW IF NOT EXISTS served_record AS
    SELECT id, kind, subject, relation, object, body, origin, trust,
           anchor_path, anchor_hash, session_id, transcript_ref, dedup_hash, created_at
    FROM record WHERE invalid = 0;

-- 2.6 external-content FTS5: text is stored once, in record.
CREATE VIRTUAL TABLE IF NOT EXISTS record_fts USING fts5(
    subject, object, body,
    content='record', content_rowid='id',
    -- `porter` stems English suffixes so a question about the "connection pooler" reaches
    -- a record that said "pooling"; it wraps `unicode61`, which still folds the diacritics.
    tokenize='porter unicode61 remove_diacritics 2'
);
-- meta.fts_rows mirrors the FTS row count so the health gate checks coherence in O(1).
INSERT OR IGNORE INTO meta(key, value) VALUES ('fts_rows', '0');
CREATE TRIGGER IF NOT EXISTS record_ai AFTER INSERT ON record WHEN new.invalid = 0 BEGIN
    INSERT INTO record_fts(rowid, subject, object, body) VALUES (new.id, new.subject, new.object, new.body);
    UPDATE meta SET value = CAST(CAST(value AS INTEGER) + 1 AS TEXT) WHERE key = 'fts_rows';
END;
CREATE TRIGGER IF NOT EXISTS record_ad AFTER DELETE ON record WHEN old.invalid = 0 BEGIN
    INSERT INTO record_fts(record_fts, rowid, subject, object, body) VALUES ('delete', old.id, old.subject, old.object, old.body);
    UPDATE meta SET value = CAST(CAST(value AS INTEGER) - 1 AS TEXT) WHERE key = 'fts_rows';
END;
-- Invalidating removes the row from the index; the row itself stays.
CREATE TRIGGER IF NOT EXISTS record_au_invalidate AFTER UPDATE OF invalid ON record
WHEN old.invalid = 0 AND new.invalid = 1 BEGIN
    INSERT INTO record_fts(record_fts, rowid, subject, object, body) VALUES ('delete', old.id, old.subject, old.object, old.body);
    UPDATE meta SET value = CAST(CAST(value AS INTEGER) - 1 AS TEXT) WHERE key = 'fts_rows';
END;
CREATE TRIGGER IF NOT EXISTS record_au_revalidate AFTER UPDATE OF invalid ON record
WHEN old.invalid = 1 AND new.invalid = 0 BEGIN
    INSERT INTO record_fts(rowid, subject, object, body) VALUES (new.id, new.subject, new.object, new.body);
    UPDATE meta SET value = CAST(CAST(value AS INTEGER) + 1 AS TEXT) WHERE key = 'fts_rows';
END;

-- v3: every retirement is dated and carries its cause, whichever statement made it. A
-- statement that names its own cause keeps it; one that does not gets its reason.
CREATE TRIGGER IF NOT EXISTS record_au_retired_when AFTER UPDATE OF invalid ON record
WHEN old.invalid = 0 AND new.invalid = 1 BEGIN
    UPDATE record SET
        invalidated_at  = coalesce(new.invalidated_at, CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER)),
        invalidated_via = coalesce(new.invalidated_via, new.invalid_reason)
    WHERE id = new.id;
END;
CREATE TRIGGER IF NOT EXISTS record_au_revalidated_clear AFTER UPDATE OF invalid ON record
WHEN old.invalid = 1 AND new.invalid = 0 BEGIN
    UPDATE record SET invalidated_at = NULL, invalidated_via = NULL WHERE id = new.id;
END;

-- v3: the earlier turns a typed record was stated in. A restatement moves the record's own
-- `transcript_ref` to its latest turn; the turns it leaves are kept here, so that when the
-- record is replaced the episodes of every turn that stated it go with it. Write path only.
CREATE TABLE IF NOT EXISTS record_turn (
    record_id      INTEGER NOT NULL REFERENCES record(id),
    transcript_ref TEXT    NOT NULL,
    -- v4: the session and date the record had in that turn, so a restatement can be undone
    -- when its turn turns out to be an agent's (`muninn-capture/src/agent.rs`)
    session_id     TEXT,
    created_at     INTEGER,
    PRIMARY KEY (record_id, transcript_ref)
);
CREATE INDEX IF NOT EXISTS record_turn_ref ON record_turn(transcript_ref);

-- v4: who wrote a prompt. A coordinating agent's tool call hands a worker its task, and the
-- worker's transcript logs that text in the user's role. `agent_task` holds the hash of every
-- long string a non-file, non-shell tool call carried, per session; `turn_prompt` the hash of
-- every prompt typed in the user's role, per turn. A prompt whose hash another session handed
-- over was written by an agent (`muninn-capture/src/agent.rs`). Hashes only. Write path only.
CREATE TABLE IF NOT EXISTS agent_task (
    hash       TEXT NOT NULL,
    session_id TEXT NOT NULL,
    PRIMARY KEY (hash, session_id)
);
CREATE TABLE IF NOT EXISTS turn_prompt (
    hash           TEXT NOT NULL,
    session_id     TEXT NOT NULL,
    transcript_ref TEXT NOT NULL,
    PRIMARY KEY (hash, transcript_ref)
);
CREATE INDEX IF NOT EXISTS turn_prompt_hash ON turn_prompt(hash);

-- v4: a journal of every restatement (`ingest::restate`), so that one found later to be an
-- agent's copy is undone exactly: the restated record's state before it, the state it left,
-- and the records that restatement itself retired. Write path only.
CREATE TABLE IF NOT EXISTS restatement (
    id             INTEGER PRIMARY KEY,
    record_id      INTEGER NOT NULL,
    turn_ref       TEXT    NOT NULL,   -- the restating turn
    new_session    TEXT,
    new_created    INTEGER NOT NULL,
    prior_invalid  INTEGER NOT NULL,
    prior_reason   TEXT,
    prior_by       INTEGER,
    prior_via      TEXT,
    prior_at       INTEGER,
    prior_ref      TEXT,
    prior_session  TEXT,
    prior_created  INTEGER NOT NULL,
    added_turn     INTEGER NOT NULL DEFAULT 0,  -- it wrote record_turn(record_id, prior_ref)
    -- 0 it stands; 1 it was inverted; 2 pending: its turn is known to be an agent's, and the
    -- record was no longer as it left it, so it waits for what changed the record to be undone
    undone         INTEGER NOT NULL DEFAULT 0,
    -- the newest record on the store when the restatement was made. With the row's own id it
    -- dates the restatement exactly against records and other restatements, with no clock:
    -- a record with a greater id, or a row with a greater id, came after it. NULL in a row
    -- an earlier build of v4 wrote.
    after_record   INTEGER
);
CREATE INDEX IF NOT EXISTS restatement_turn ON restatement(turn_ref);
-- the pending rows of a record, and the later rows that recorded an earlier one's retirement
CREATE INDEX IF NOT EXISTS restatement_record ON restatement(record_id);
CREATE TABLE IF NOT EXISTS restatement_retired (
    restatement_id INTEGER NOT NULL REFERENCES restatement(id),
    record_id      INTEGER NOT NULL,
    PRIMARY KEY (restatement_id, record_id)
);

-- v4: what retired a row marked `agent_unrestored`: the statement an agent's prompt typed, or
-- the journal row of its restatement. The link is kept here and not on the row, so it does
-- not travel with an export: an imported marked row has no link and is never given back.
-- A row can carry the mark as it stands (`held_by` 0) and, apart from that, in the prior state
-- a journal row would return it to (`held_by` that row's id): each has its own link.
-- `held_by` -1 is not a link: it notes that the person revoked the record while a replacement
-- held it retired (`filter::revoke`), with the newest record and journal row of that moment.
-- Write path only.
CREATE TABLE IF NOT EXISTS agent_retired (
    record_id INTEGER NOT NULL,
    held_by   INTEGER NOT NULL DEFAULT 0,
    statement INTEGER,
    journal   INTEGER,
    PRIMARY KEY (record_id, held_by)
);

-- v4: one row for each pass, outside ingest, that retires or revokes records or could have
-- (`db::note_event`): maintain's commit and revert passes and repairs, the anchor validator,
-- `muninn revoke`, import, the variant pass of the embedder. It holds the newest record and
-- journal row on the store when it was written, so it is before or after a retirement by id.
-- A record an agent's prompt retired is not given back once such a pass has run: the pass
-- only looks at active records and never met it. Append-only. Write path only.
CREATE TABLE IF NOT EXISTS store_event (
    id            INTEGER PRIMARY KEY,
    what          TEXT    NOT NULL,
    after_record  INTEGER NOT NULL,
    after_journal INTEGER NOT NULL
);

-- Term statistics for IDF-based term selection in the read path [H6].
CREATE VIRTUAL TABLE IF NOT EXISTS record_vocab USING fts5vocab('record_fts', 'col');

-- Embedding sidecar (plan, Phase 3 §10): one vector per active record, per model.
-- Filled on the asynchronous write path only; read hooks never touch it.
CREATE TABLE IF NOT EXISTS record_vec (
    record_id  INTEGER NOT NULL REFERENCES record(id),
    dim        INTEGER NOT NULL,
    vec        BLOB    NOT NULL,
    model_id   TEXT    NOT NULL,
    created_at INTEGER NOT NULL,
    PRIMARY KEY (record_id, model_id)
);

-- Symbol graph (plan, Phase 5 §1-bis): definitions and references per file, rebuilt
-- per file by content hash on the write path; the read path only does indexed lookups.
CREATE TABLE IF NOT EXISTS symbol (
    id             INTEGER PRIMARY KEY,
    qualified_name TEXT    NOT NULL,
    short_name     TEXT    NOT NULL,
    kind           TEXT    NOT NULL,
    path           TEXT    NOT NULL,
    line           INTEGER NOT NULL,
    file_hash      TEXT    NOT NULL,
    partial        INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS symbol_short ON symbol(short_name);
CREATE INDEX IF NOT EXISTS symbol_path  ON symbol(path);
CREATE TABLE IF NOT EXISTS symbol_ref (
    path      TEXT    NOT NULL,
    line      INTEGER NOT NULL,
    from_name TEXT,
    to_name   TEXT    NOT NULL,
    ref_kind  TEXT    NOT NULL
);
CREATE INDEX IF NOT EXISTS symbol_ref_to   ON symbol_ref(to_name);
CREATE INDEX IF NOT EXISTS symbol_ref_path ON symbol_ref(path);
CREATE TABLE IF NOT EXISTS symbol_file (
    path      TEXT PRIMARY KEY,
    file_hash TEXT NOT NULL,
    lang      TEXT NOT NULL,
    defs      INTEGER NOT NULL,
    refs      INTEGER NOT NULL,
    partial   INTEGER NOT NULL DEFAULT 0,
    indexed_at INTEGER NOT NULL
);

-- 2.2 cue: permanent trigger conditions (F3).
CREATE TABLE IF NOT EXISTS cue (
    record_id   INTEGER NOT NULL REFERENCES record(id),
    kind        TEXT    NOT NULL CHECK (kind IN ('dir','symbol','event','after','cooldown','keyword','glob')),
    key         TEXT    NOT NULL,
    grp         INTEGER NOT NULL DEFAULT 0
);
-- covering: `evaluate` wants (record_id, grp) for a (kind, key), and a dir cue on a busy
-- directory matches thousands of rows, so reading them out of the table costs a page each.
CREATE INDEX IF NOT EXISTS cue_lookup ON cue(kind, key, record_id, grp);
CREATE INDEX IF NOT EXISTS cue_record ON cue(record_id);

-- 2.3 rule: project rules and their enforcement state (F2).
CREATE TABLE IF NOT EXISTS rule (
    id             INTEGER PRIMARY KEY,
    source_file    TEXT    NOT NULL,
    source_line    INTEGER NOT NULL,
    source_hash    TEXT    NOT NULL,
    text           TEXT    NOT NULL,
    class          TEXT    NOT NULL CHECK (class IN ('enforceable_permission','enforceable_hook','enforceable_sandbox','interpretive_only')),
    pattern_id     TEXT,
    emitted        TEXT,
    rationale_ref  INTEGER REFERENCES record(id),
    compiled_at    INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS rule_source ON rule(source_file, source_hash);

-- 2.4 fire_ledger: every decision to deliver or to stay silent leaves a row.
CREATE TABLE IF NOT EXISTS fire_ledger (
    id               INTEGER PRIMARY KEY,
    session_id       TEXT    NOT NULL,
    compaction_epoch INTEGER NOT NULL DEFAULT 0,
    record_id        INTEGER REFERENCES record(id),
    fired_at         INTEGER NOT NULL,
    tokens           INTEGER NOT NULL DEFAULT 0,
    reason           TEXT    NOT NULL
);
-- What a session was delivered in its epoch (`delivery::delivered_ids`): only the rows that
-- count, declared with the exact words of `delivery::SHOWN_ROWS_SQL` so the planner uses it. A
-- long session's rows are mostly budget cuts, which are never read. It replaces `fire_session`; a
-- store read before its first write open still has that one, and the same query answers there.
DROP INDEX IF EXISTS fire_session;
DROP INDEX IF EXISTS fire_session_shown;
CREATE INDEX IF NOT EXISTS fire_session_seen ON fire_ledger(session_id, compaction_epoch, record_id)
    WHERE reason NOT LIKE '%gated:%' AND reason NOT LIKE '%cue:event:subagent%' AND reason NOT LIKE '%catalog';

-- 2.5 heartbeat: folded in from .muninn/log/heartbeat.jsonl by the write path.
-- Read hooks never write the database; they append to the log file instead.
CREATE TABLE IF NOT EXISTS heartbeat (
    id          INTEGER PRIMARY KEY,
    hook        TEXT    NOT NULL,
    session_id  TEXT    NOT NULL,
    started_at  INTEGER NOT NULL,
    ok          INTEGER,
    error       TEXT,
    ms          REAL
);
CREATE INDEX IF NOT EXISTS heartbeat_session ON heartbeat(session_id, started_at);

CREATE TABLE IF NOT EXISTS health (
    checked_at INTEGER NOT NULL,
    session_id TEXT    NOT NULL,
    summary    TEXT    NOT NULL,
    detail     TEXT    NOT NULL
);

-- Per-session accumulated turn context (F3): files touched, symbols referenced.
CREATE TABLE IF NOT EXISTS turn_context (
    session_id TEXT    NOT NULL,
    kind       TEXT    NOT NULL CHECK (kind IN ('file','symbol')),
    key        TEXT    NOT NULL,
    seen_at    INTEGER NOT NULL,
    PRIMARY KEY (session_id, kind, key)
);

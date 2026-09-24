-- auth.durable_sessions: a generic, kind-discriminated table backing
-- unitprep_core::durable_session_store::DurableSessionStore -- the
-- write-through wrapper around InMemorySessionStore that lets a
-- session survive a process restart. See that module's own doc comment
-- for the full design (why a wrapper rather than a from-scratch
-- Postgres store, why bincode over serde_json, why timestamps cross the
-- SQL boundary as epoch-second floats instead of via chrono).
--
-- First (and today, only) consumer: WebAuthn registration/authentication
-- ceremonies (src/auth/registration_ceremony.rs,
-- authentication_ceremony.rs), previously purely in-memory and lost on
-- any restart mid-ceremony. The three tool sessions (unit-group/dedup/
-- template-tagger) are NOT moved onto this table -- that's separate,
-- not-yet-started follow-up work; they stay on plain
-- InMemorySessionStore exactly as before.
--
-- One shared table, not one per session kind: every kind this store
-- will ever hold shares the exact same shape -- an id, the same small
-- SessionMetadata envelope every HasSessionMetadata implementor already
-- carries, and an opaque serialized payload -- so a second
-- nearly-identical table per kind would just be duplication for no
-- benefit. Same reasoning as client_ops.vendor_format's own
-- single-registry design (see CLAUDE.md's data-over-hardcoding
-- principle, and that migration's precedent).
--
-- (kind, id), not id alone, is the primary key: ids are generated
-- independently per session type (SessionMetadata::new, called by each
-- type's own constructor) with no coordination between types, so
-- nothing today guarantees two different kinds' ids can never collide.
-- `kind` is the store's own instance-level discriminator (see
-- DurableSessionStore::kind) -- every row a given DurableSessionStore
-- instance ever reads or writes carries that same value.
--
-- `payload` is bincode, not JSON -- see the Rust module's doc comment
-- for the concrete reason (a raw `Vec<u8>` field, e.g. webauthn_state,
-- would otherwise serialize as a JSON array of decimal numbers, several
-- times larger for no benefit since nothing outside this process ever
-- reads this column).
--
-- owner_id/created_at/last_accessed/cancelled mirror SessionMetadata's
-- fields as real, queryable columns rather than burying them inside
-- payload -- the periodic expiry sweep
-- (DurableSessionStore::start_cleanup_task) needs to find and delete
-- stale rows by last_accessed without deserializing every payload
-- first, and a rehydrated session needs its real created_at/owner_id
-- back, not a fresh one.
CREATE TABLE auth.durable_sessions (
    id TEXT NOT NULL,
    kind TEXT NOT NULL,

    -- Always Some(user_id) for today's only consumer (both ceremony
    -- types), but nullable to match SessionMetadata.owner_id's own
    -- Option<Uuid> -- some future durable session kind may legitimately
    -- have no owner, the same way today's tool sessions don't.
    -- ON DELETE CASCADE: a durable session tied to a user that no
    -- longer exists is meaningless, not just orphaned -- there is no
    -- reasonable way to ever rehydrate or act on it again.
    owner_id UUID REFERENCES auth.users(id) ON DELETE CASCADE,

    created_at TIMESTAMPTZ NOT NULL,
    last_accessed TIMESTAMPTZ NOT NULL,
    cancelled BOOLEAN NOT NULL DEFAULT false,

    payload BYTEA NOT NULL,

    PRIMARY KEY (kind, id)
);

-- The periodic expiry sweep's own query: one kind's stale rows, found
-- without a table scan.
CREATE INDEX durable_sessions_kind_last_accessed_idx
    ON auth.durable_sessions (kind, last_accessed);

ALTER TABLE auth.durable_sessions ENABLE ROW LEVEL SECURITY;

-- Unconditional on every operation, deliberately -- unlike most
-- app-facing tables, this one is never reached by a user-facing query;
-- the only code that ever touches it is DurableSessionStore itself
-- (src/main.rs wires it in for the two WebAuthn ceremony stores). And
-- unlike client_ops.tool_runs's own "unconditional because the caller
-- already passed a stricter in-app gate" reasoning, here there often
-- isn't even an authenticated app.current_user_id to gate on in the
-- first place: a registration ceremony's first write happens before the
-- caller has any session, an authentication ceremony begins before
-- login succeeds, and a cold-start rehydration read can happen with no
-- request-scoped GUC set at all. Gating these policies on
-- app.current_user_id the way client-facing tables are gated would make
-- the very ceremonies this table exists to persist unwritable/
-- unreadable at the exact moments they need to work.
CREATE POLICY durable_sessions_select_unconditional ON auth.durable_sessions
    FOR SELECT USING (true);

CREATE POLICY durable_sessions_insert_unconditional ON auth.durable_sessions
    FOR INSERT WITH CHECK (true);

CREATE POLICY durable_sessions_update_unconditional ON auth.durable_sessions
    FOR UPDATE USING (true) WITH CHECK (true);

CREATE POLICY durable_sessions_delete_unconditional ON auth.durable_sessions
    FOR DELETE USING (true);

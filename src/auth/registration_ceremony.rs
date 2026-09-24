use uuid::Uuid;

use unitprep_core::session::{HasSessionMetadata, SessionMetadata};

/// Ephemeral state for one in-progress WebAuthn registration ceremony --
/// the gap between POST /auth/register/begin (issues a challenge) and
/// POST /auth/register/finish (verifies the browser's response against
/// it). Stored the same way unit-group/dedup sessions are (see
/// unitprep_core::in_memory_session_store), not in a database table --
/// this state is meaningless once the ceremony completes or expires,
/// and unlike a real session it must never survive a process restart or
/// be reachable by anything other than the exact ceremony that created
/// it.
///
/// UPDATE 2026-09-24: it now DOES survive a process restart --
/// `main.rs` wires this store to `unitprep_core::durable_session_store::
/// DurableSessionStore` instead of a plain `InMemorySessionStore`, so a
/// deploy landing mid-registration no longer strands the browser with an
/// inexplicable "ceremony expired" error. The other half of that
/// sentence -- unreachable by anything other than the exact ceremony
/// that created it -- still holds: a persisted row is looked up by this
/// exact id, same as the in-memory map was, and Postgres's own RLS
/// policies on `auth.durable_sessions` are unconditional rather than
/// permissive to any authenticated caller (see that migration's own doc
/// comment) precisely because nothing outside `DurableSessionStore`
/// itself is meant to read it.
///
/// `Serialize`/`Deserialize` are derived so this type can round-trip
/// through that store -- `webauthn_state`/`invite_token_hash` are plain
/// `Vec<u8>`/`Option<Vec<u8>>`, which serde already knows how to
/// (de)serialize with no special handling.
#[derive(serde::Serialize, serde::Deserialize)]
pub struct RegistrationCeremony {
    pub metadata: SessionMetadata,
    pub user_id: Uuid,

    /// webauthn-rs's own serialized PasskeyRegistration state, opaque to
    /// everything except AuthBackend::finish_registration -- see
    /// RegistrationChallenge in auth/mod.rs.
    pub webauthn_state: Vec<u8>,

    /// Non-secret id for correlating this ceremony's two halves in the
    /// logs. Deliberately NOT `metadata.id`: that value is the ceremony
    /// cookie's contents, so logging it would put a live bearer value into
    /// ops output that is routinely shipped somewhere less protected than
    /// the database. This one is generated alongside it, never leaves the
    /// server, and is safe to log -- which is the whole point, since two
    /// concurrent ceremonies for the same user are otherwise
    /// indistinguishable in the log.
    pub correlation_id: Uuid,

    /// `Some` when this ceremony was authorized by an **invite token**
    /// rather than by an existing session, holding that token's hash so
    /// `finish` can consume the invite in the same transaction that writes
    /// the credential.
    ///
    /// This replaced an `is_bootstrap: bool` and is strictly better for
    /// two reasons. It carries what `finish` actually needs instead of
    /// only asserting that something exists, and `Option` makes the
    /// invariant structural: there is no way to represent "invite
    /// enrolment" without also having the token to consume.
    ///
    /// Decided at `begin` and carried here rather than re-derived at
    /// `finish`: whether a session cookie happens to be present on the
    /// *second* request is a different question from which path this
    /// ceremony was authorized under, and only the invite case should end
    /// with a newly issued session (an already-authenticated caller keeps
    /// the session they arrived with).
    ///
    /// It holds the **hash**, never the raw token. The raw value is a
    /// bearer credential and has no business living in server-side state
    /// any longer than the request that carried it.
    pub invite_token_hash: Option<Vec<u8>>,
}

impl RegistrationCeremony {
    pub fn new(
        id: String,
        user_id: Uuid,
        webauthn_state: Vec<u8>,
        invite_token_hash: Option<Vec<u8>>,
    ) -> Self {
        Self {
            metadata: SessionMetadata::new(id, Some(user_id)),
            user_id,
            correlation_id: Uuid::new_v4(),
            webauthn_state,
            invite_token_hash,
        }
    }
}

impl HasSessionMetadata for RegistrationCeremony {
    fn metadata(&self) -> &SessionMetadata {
        &self.metadata
    }

    fn metadata_mut(&mut self) -> &mut SessionMetadata {
        &mut self.metadata
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The reason `correlation_id` exists at all: the log-safe id must not
    /// be the cookie's value. A later "simplification" that logged
    /// `metadata.id` instead, or assigned it here, would put a live bearer
    /// value into ops output -- and would look like a tidy-up in review.
    #[test]
    fn the_correlation_id_is_not_the_ceremony_id() {
        let ceremony = RegistrationCeremony::new(
            "ceremony-cookie-value".to_string(),
            Uuid::new_v4(),
            Vec::new(),
            None,
        );

        assert_ne!(
            ceremony.correlation_id.to_string(),
            ceremony.metadata.id,
            "the logged correlation id must never be the ceremony cookie's value"
        );
    }

    /// Two ceremonies for the SAME user must be tellable apart, which is
    /// the property that makes the id worth logging -- two browser tabs
    /// enrolling at once are indistinguishable by `user_id` alone.
    #[test]
    fn two_ceremonies_for_one_user_get_different_correlation_ids() {
        let user_id = Uuid::new_v4();

        let first = RegistrationCeremony::new("a".to_string(), user_id, Vec::new(), None);
        let second = RegistrationCeremony::new("b".to_string(), user_id, Vec::new(), None);

        assert_ne!(first.correlation_id, second.correlation_id);
    }

    /// Integration test for the actual feature this change is about:
    /// a WebAuthn ceremony surviving a process restart (see `main.rs`,
    /// which wires `registration_ceremonies` to
    /// `unitprep_core::durable_session_store::DurableSessionStore`
    /// instead of a plain `InMemorySessionStore` -- see that module's
    /// own doc comment for the full design). The wrapper's own unit
    /// tests (`durable_session_store_tests.rs`, in the `core` crate)
    /// only ever exercise the in-memory hit path against an unreachable
    /// Postgres pool -- by design, they cannot prove rehydration
    /// actually works against a real database. This project's own
    /// standing principle is to verify empirically, not just by reading
    /// code, and its own history backs that up: `client_ops::tool_runs`'
    /// regression test doc comment describes a migration that was
    /// committed but never actually applied to the dev DB, caught only
    /// by a real-DB integration test exactly like this one -- same
    /// pattern, same reason.
    ///
    /// `metadata.owner_id` is deliberately left `None` here rather than
    /// a real `auth.users` id. `RegistrationCeremony::new` always sets
    /// it to `Some(user_id)` in production (every real call site already
    /// has a `user_id` read back from that table -- see
    /// `api::auth_register`), but reproducing that here would mean
    /// either creating a throwaway row in `auth.users` (whose own RLS
    /// policies -- `users_select_own_or_admin`,
    /// `users_insert_admin_only` -- block reading or inserting one at
    /// all without an authenticated `app.current_user_id` already set,
    /// and which has no DELETE policy whatsoever, so a throwaway row
    /// could never be cleaned back up afterward) or depending on a
    /// specific seeded account happening to still exist. `owner_id`'s
    /// column value is opaque to durability itself -- a `UUID` is a
    /// `UUID` whether or not it happens to be real -- so this test skips
    /// that complication entirely, at the cost of not exercising the
    /// `owner_id` foreign key constraint itself (ordinary Postgres
    /// behavior, not anything this change introduces). The `user_id`
    /// field on `RegistrationCeremony` (distinct from
    /// `metadata.owner_id`) is still round-tripped with a real value,
    /// since it lives inside the opaque bincode payload rather than a
    /// queryable, constrained column.
    ///
    /// Run explicitly with `cargo test -- --ignored durability`.
    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "needs a real, reachable Postgres with migrations applied -- see doc comment"]
    async fn a_registration_ceremony_survives_a_simulated_process_restart_durability() {
        use std::time::Duration;

        use unitprep_core::durable_session_store::DurableSessionStore;
        use unitprep_core::session_store::SessionStore;

        let _ = dotenvy::from_filename(".env.local");

        let db =
            crate::db::connect().expect("DATABASE_URL must be a well-formed connection string");

        // A unique kind per test run, not the real
        // "webauthn_registration_ceremony" main.rs actually uses --
        // keeps this test's row fully isolated from anything a
        // concurrently-running real server instance might be persisting
        // under that kind, and makes this test safely repeatable/
        // parallelizable with itself.
        let kind = format!("test_registration_ceremony_{}", Uuid::new_v4());
        let ceremony_id = format!("test-ceremony-{}", Uuid::new_v4());

        let mut ceremony = RegistrationCeremony::new(
            ceremony_id.clone(),
            Uuid::new_v4(),
            b"opaque webauthn-rs ceremony state bytes".to_vec(),
            Some(b"fake invite token hash".to_vec()),
        );
        // See this test's own doc comment for why owner_id is cleared.
        ceremony.metadata.owner_id = None;

        let original_user_id = ceremony.user_id;
        let original_correlation_id = ceremony.correlation_id;
        let original_webauthn_state = ceremony.webauthn_state.clone();
        let original_invite_token_hash = ceremony.invite_token_hash.clone();

        let store_before_restart = DurableSessionStore::<RegistrationCeremony>::with_timeout(
            db.clone(),
            kind.clone(),
            Duration::from_secs(5 * 60),
        );

        store_before_restart.save(ceremony);

        // save()'s Postgres write is fire-and-forget (see
        // DurableSessionStore::persist's own doc comment for why) --
        // poll briefly for the row to actually land before simulating a
        // restart, rather than either a single fixed sleep (flaky under
        // a slow connection) or asserting immediately (would always
        // fail, racing the spawned write).
        let mut persisted = false;

        for _ in 0..20 {
            let count: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM auth.durable_sessions WHERE kind = $1 AND id = $2",
            )
            .bind(&kind)
            .bind(&ceremony_id)
            .fetch_one(&db)
            .await
            .expect("querying auth.durable_sessions must not fail");

            if count == 1 {
                persisted = true;
                break;
            }

            tokio::time::sleep(Duration::from_millis(100)).await;
        }

        assert!(
            persisted,
            "save() must write a row to auth.durable_sessions within ~2 seconds"
        );

        // Simulate a process restart: drop the store entirely (its
        // in-memory layer, and every handle into it, goes with it) and
        // build a brand new one against the SAME underlying Postgres
        // connection pool -- exactly what actually happens across a real
        // deploy, just without actually killing the OS process.
        drop(store_before_restart);

        let store_after_restart = DurableSessionStore::<RegistrationCeremony>::with_timeout(
            db.clone(),
            kind.clone(),
            Duration::from_secs(5 * 60),
        );

        let handle = store_after_restart.get_handle(&ceremony_id).expect(
            "get_handle must rehydrate the ceremony from Postgres after a simulated restart",
        );

        {
            let rehydrated = handle.read();

            assert_eq!(rehydrated.metadata.id, ceremony_id);
            assert_eq!(rehydrated.metadata.owner_id, None);
            assert!(!rehydrated.metadata.cancelled);
            assert_eq!(rehydrated.user_id, original_user_id);
            assert_eq!(rehydrated.correlation_id, original_correlation_id);
            assert_eq!(rehydrated.webauthn_state, original_webauthn_state);
            assert_eq!(rehydrated.invite_token_hash, original_invite_token_hash);
        }

        // Clean up -- leave no row behind for the next run.
        store_after_restart.delete(&ceremony_id);

        // delete()'s Postgres write is ALSO fire-and-forget -- give it a
        // moment before the test process exits so the row is actually
        // gone rather than merely scheduled to be.
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
}

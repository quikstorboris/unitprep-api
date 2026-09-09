/// Where an integration settings page reads a credential's *current
/// value* from, when nothing has been saved to that integration's own
/// settings table yet -- so the page shows what's actually configured
/// (masked, revealable) instead of a blank form the first time an admin
/// opens it.
///
/// [`ProcessEnvSource`] (process environment variables) is the only
/// implementation today, and is very likely the only one this app ever
/// needs: on Fly.io specifically, `fly secrets set` works by injecting
/// the value into the running machine's process environment -- there is
/// no separate "read a secret back over the network" API to call
/// instead, so `std::env::var` already *is* "ask Fly.io for the current
/// value" for any app hosted there. This trait exists as the one seam to
/// change, in the unlikely event a future host (or a dedicated secrets
/// manager sitting in front of whatever host) exposes a real read-back
/// API distinct from the running process's own environment -- write a
/// new implementation and swap the one line in `main.rs` that constructs
/// `AppState::env_source`, rather than hunting down every
/// `std::env::var` call across the integration settings handlers.
pub trait EnvSource: Send + Sync {
    fn get(&self, key: &str) -> Option<String>;
}

pub struct ProcessEnvSource;

impl EnvSource for ProcessEnvSource {
    fn get(&self, key: &str) -> Option<String> {
        std::env::var(key).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    #[test]
    #[serial(integrations_env_source_test_var)]
    fn reads_a_set_process_env_var() {
        std::env::set_var("INTEGRATIONS_ENV_SOURCE_TEST_VAR", "value");
        assert_eq!(
            ProcessEnvSource.get("INTEGRATIONS_ENV_SOURCE_TEST_VAR"),
            Some("value".to_string())
        );
        std::env::remove_var("INTEGRATIONS_ENV_SOURCE_TEST_VAR");
    }

    #[test]
    #[serial(integrations_env_source_test_var)]
    fn returns_none_for_an_unset_var() {
        std::env::remove_var("INTEGRATIONS_ENV_SOURCE_TEST_VAR");
        assert_eq!(ProcessEnvSource.get("INTEGRATIONS_ENV_SOURCE_TEST_VAR"), None);
    }
}

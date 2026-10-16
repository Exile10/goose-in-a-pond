//! What an installed catalogue extension starts with. Key values live in the secret store: a
//! saved row gives up its copy of a key once the store holds that key, and each start reads the
//! store, so a deleted key stops working everywhere at once. A row never loses the only copy.

use std::collections::HashMap;

use crate::mcp::domain::marketplace::MarketplaceExtension;
use crate::security::ports::secret::SecretRepository;

/// `env` without any secret `ext` declares: what may be saved in the extension's row.
pub fn without_declared_secrets(
    env: &HashMap<String, String>,
    ext: &MarketplaceExtension,
) -> HashMap<String, String> {
    env.iter()
        .filter(|(key, _)| !ext.required_secrets.iter().any(|sr| &sr.key == *key))
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect()
}

/// The blocking secrets `stored` lacks, by name; a blank value counts as missing.
pub fn missing_blocking_secrets(
    ext: &MarketplaceExtension,
    stored: &HashMap<String, String>,
) -> Vec<String> {
    ext.blocking_secrets()
        .filter(|sr| stored.get(&sr.key).is_none_or(|v| v.trim().is_empty()))
        .map(|sr| sr.key.clone())
        .collect()
}

/// A start prepared from an extension's saved row and the secret store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedEnv {
    /// The environment to start with, or the blocking secrets it would lack.
    pub start: Result<HashMap<String, String>, Vec<String>>,
    /// The row's environment less every declared secret the store holds.
    pub row_env: HashMap<String, String>,
    /// Whether the row gave up a key, so it must be saved again without it.
    pub row_changed: bool,
}

fn non_blank(value: Option<&String>) -> Option<&String> {
    value.filter(|v| !v.trim().is_empty())
}

/// Prepare a start. The row keeps a declared secret only while `stored` (the store's values for
/// the extension's declared secrets) lacks it; the start takes the store's value, else the row's.
pub fn prepare_extension_env(
    row_env: &HashMap<String, String>,
    ext: &MarketplaceExtension,
    stored: &HashMap<String, String>,
) -> PreparedEnv {
    let in_store = |key: &str| non_blank(stored.get(key)).is_some();
    let kept: HashMap<String, String> = row_env
        .iter()
        .filter(|(key, _)| {
            !ext.required_secrets
                .iter()
                .any(|sr| &sr.key == *key && in_store(key))
        })
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect();
    let row_changed = kept.len() != row_env.len();

    let mut known = stored.clone();
    for sr in &ext.required_secrets {
        if let Some(value) = non_blank(kept.get(&sr.key)) {
            known.entry(sr.key.clone()).or_insert_with(|| value.clone());
        }
    }
    let missing = missing_blocking_secrets(ext, &known);
    let start = if missing.is_empty() {
        let mut env = without_declared_secrets(&kept, ext);
        for sr in ext.env_secrets() {
            if let Some(value) = non_blank(known.get(&sr.key)) {
                env.insert(sr.key.clone(), value.clone());
            }
        }
        Ok(env)
    } else {
        Err(missing)
    };
    PreparedEnv {
        start,
        row_env: kept,
        row_changed,
    }
}

/// [`prepare_extension_env`] for a saved row at start-up, against the store itself. A key only
/// the row holds is first copied into the store, and leaves the row only once that succeeded;
/// with no store, or one that cannot be read, the row is left whole and starts from its own copy.
pub async fn settle_saved_env(
    row_env: &HashMap<String, String>,
    ext: &MarketplaceExtension,
    store: Option<&dyn SecretRepository>,
) -> PreparedEnv {
    let mut stored = HashMap::new();
    if let Some(store) = store {
        for sr in &ext.required_secrets {
            match store.get(&sr.key).await {
                Ok(value) if non_blank(value.as_ref()).is_some() => {
                    stored.insert(sr.key.clone(), value.unwrap_or_default());
                }
                Ok(_) => {
                    let Some(row_value) = non_blank(row_env.get(&sr.key)) else {
                        continue;
                    };
                    match store.set(&sr.key, row_value).await {
                        Ok(()) => {
                            tracing::info!(
                                extension = %ext.id,
                                key = %sr.key,
                                "moved a key from the saved extension row into the secret store"
                            );
                            stored.insert(sr.key.clone(), row_value.clone());
                        }
                        Err(e) => tracing::warn!(
                            extension = %ext.id,
                            key = %sr.key,
                            error = %e,
                            "could not move a key into the secret store; the saved row keeps it"
                        ),
                    }
                }
                Err(e) => tracing::warn!(
                    extension = %ext.id,
                    key = %sr.key,
                    error = %e,
                    "the secret store could not be read; the saved row keeps its copy"
                ),
            }
        }
    }
    prepare_extension_env(row_env, ext, &stored)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A required pasted key, an optional one, a host-only key and a sign-in.
    fn ext() -> MarketplaceExtension {
        serde_json::from_value(serde_json::json!({
            "id": "github",
            "name": "GitHub",
            "description": "",
            "kind": "stdio",
            "command": "npx",
            "category": "development",
            "author": "test",
            "required_secrets": [
                {"key": "TOKEN", "display_name": "Token", "description": "", "required": true,
                 "kind": "api_key"},
                {"key": "OPTIONAL", "display_name": "Optional", "description": "",
                 "required": false, "kind": "generic"},
                {"key": "SIGNING", "display_name": "Signing", "description": "", "required": false,
                 "kind": "api_key", "host_only": true},
                {"key": "SIGN_IN", "display_name": "Sign in", "description": "", "required": true,
                 "kind": "oauth_flow"}
            ]
        }))
        .unwrap()
    }

    fn map(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn a_saved_row_keeps_no_declared_secret() {
        let row = map(&[
            ("TOKEN", "old-value"),
            ("OPTIONAL", "o"),
            ("SIGNING", "s"),
            ("SIGN_IN", "t"),
            ("GIAP_SERVER_URL", "http://127.0.0.1:4000"),
        ]);
        assert_eq!(
            without_declared_secrets(&row, &ext()),
            map(&[("GIAP_SERVER_URL", "http://127.0.0.1:4000")])
        );
    }

    #[test]
    fn a_start_takes_the_stores_values_not_the_rows() {
        let row = map(&[("TOKEN", "old-value"), ("GIAP_SERVER_URL", "u")]);
        let stored = map(&[("TOKEN", "new-value"), ("SIGNING", "s"), ("OPTIONAL", "o")]);
        let prepared = prepare_extension_env(&row, &ext(), &stored);
        assert_eq!(
            prepared.start,
            Ok(map(&[
                ("TOKEN", "new-value"),
                ("OPTIONAL", "o"),
                ("GIAP_SERVER_URL", "u")
            ])),
            "a host-only secret never enters the environment"
        );
        assert_eq!(prepared.row_env, map(&[("GIAP_SERVER_URL", "u")]));
        assert!(prepared.row_changed, "the row's copy of the key must go");
    }

    /// The store does not hold it, so the row's copy is the only one: kept, and started with.
    #[test]
    fn a_key_only_the_row_holds_stays_in_the_row() {
        let row = map(&[("TOKEN", "old-value"), ("SIGNING", "s")]);
        let prepared = prepare_extension_env(&row, &ext(), &HashMap::new());
        assert_eq!(prepared.start, Ok(map(&[("TOKEN", "old-value")])));
        assert_eq!(prepared.row_env, row);
        assert!(!prepared.row_changed);
    }

    #[test]
    fn with_no_key_anywhere_the_start_is_refused_by_name() {
        let prepared = prepare_extension_env(&HashMap::new(), &ext(), &HashMap::new());
        assert_eq!(prepared.start, Err(vec!["TOKEN".to_string()]));
    }

    #[test]
    fn a_blank_stored_key_is_missing_and_an_optional_one_never_blocks() {
        let stored = map(&[("TOKEN", "  ")]);
        assert_eq!(
            missing_blocking_secrets(&ext(), &stored),
            vec!["TOKEN".to_string()]
        );
        let stored = map(&[("TOKEN", "a-value")]);
        assert!(
            missing_blocking_secrets(&ext(), &stored).is_empty(),
            "neither the optional key nor the sign-in blocks"
        );
    }

    #[test]
    fn a_clean_row_is_left_unchanged() {
        let row = map(&[("GIAP_SERVER_URL", "u")]);
        let prepared = prepare_extension_env(&row, &ext(), &map(&[("TOKEN", "a-value")]));
        assert!(!prepared.row_changed);
        assert!(prepared.start.is_ok());
    }

    /// A store that holds `values`, and refuses every write when `writes_fail`.
    #[derive(Default)]
    struct Store {
        values: std::sync::Mutex<HashMap<String, String>>,
        writes_fail: bool,
        reads_fail: bool,
    }

    #[async_trait::async_trait]
    impl SecretRepository for Store {
        async fn get(&self, key: &str) -> anyhow::Result<Option<String>> {
            if self.reads_fail {
                anyhow::bail!("locked");
            }
            Ok(self.values.lock().unwrap().get(key).cloned())
        }
        async fn set(&self, key: &str, value: &str) -> anyhow::Result<()> {
            if self.writes_fail {
                anyhow::bail!("disk full");
            }
            self.values
                .lock()
                .unwrap()
                .insert(key.to_string(), value.to_string());
            Ok(())
        }
        async fn delete(&self, key: &str) -> anyhow::Result<()> {
            self.values.lock().unwrap().remove(key);
            Ok(())
        }
        async fn list_keys(&self) -> anyhow::Result<Vec<String>> {
            Ok(self.values.lock().unwrap().keys().cloned().collect())
        }
        async fn has(&self, key: &str) -> anyhow::Result<bool> {
            Ok(self.values.lock().unwrap().contains_key(key))
        }
    }

    #[tokio::test]
    async fn a_key_only_the_row_holds_moves_into_the_store_then_leaves_the_row() {
        let store = Store::default();
        let row = map(&[("TOKEN", "row-value"), ("GIAP_SERVER_URL", "u")]);
        let settled = settle_saved_env(&row, &ext(), Some(&store)).await;
        assert_eq!(
            store
                .values
                .lock()
                .unwrap()
                .get("TOKEN")
                .map(String::as_str),
            Some("row-value")
        );
        assert_eq!(settled.row_env, map(&[("GIAP_SERVER_URL", "u")]));
        assert!(settled.row_changed);
        assert_eq!(
            settled.start,
            Ok(map(&[("TOKEN", "row-value"), ("GIAP_SERVER_URL", "u")]))
        );
    }

    #[tokio::test]
    async fn a_key_the_store_cannot_take_stays_in_the_row() {
        let store = Store {
            writes_fail: true,
            ..Store::default()
        };
        let row = map(&[("TOKEN", "row-value")]);
        let settled = settle_saved_env(&row, &ext(), Some(&store)).await;
        assert_eq!(settled.row_env, row, "the only copy was dropped");
        assert!(!settled.row_changed);
        assert_eq!(settled.start, Ok(map(&[("TOKEN", "row-value")])));
    }

    #[tokio::test]
    async fn with_no_store_or_an_unreadable_one_the_row_is_left_whole() {
        let row = map(&[("TOKEN", "row-value")]);
        for store in [
            None,
            Some(Store {
                reads_fail: true,
                ..Store::default()
            }),
        ] {
            let settled = settle_saved_env(
                &row,
                &ext(),
                store.as_ref().map(|s| s as &dyn SecretRepository),
            )
            .await;
            assert_eq!(settled.row_env, row);
            assert!(!settled.row_changed);
            assert_eq!(settled.start, Ok(map(&[("TOKEN", "row-value")])));
        }
    }

    #[tokio::test]
    async fn a_key_the_store_holds_leaves_the_row_and_the_store_wins() {
        let store = Store::default();
        store
            .values
            .lock()
            .unwrap()
            .insert("TOKEN".into(), "stored".into());
        let settled = settle_saved_env(&map(&[("TOKEN", "stale")]), &ext(), Some(&store)).await;
        assert!(settled.row_env.is_empty());
        assert_eq!(settled.start, Ok(map(&[("TOKEN", "stored")])));
        assert_eq!(
            store
                .values
                .lock()
                .unwrap()
                .get("TOKEN")
                .map(String::as_str),
            Some("stored"),
            "the store's value is never overwritten by the row's"
        );
    }
}

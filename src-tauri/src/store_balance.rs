use super::Store;
use crate::error::{AppError, AppResult};
use crate::model::{AppConfig, BalanceQuery, BalanceResult, ProviderKey};
use crate::secret::SecretStore;
use zeroize::{Zeroize, Zeroizing};

pub(crate) const ACCESS_PREFIX: &str = "__balance_access_";

pub(super) fn referenced_key_secrets(keys: &[ProviderKey]) -> std::collections::HashSet<String> {
    keys.iter()
        .flat_map(|key| {
            std::iter::once(key.id.clone())
                .chain(
                    key.balance_query
                        .as_ref()
                        .and_then(|query| query.api_key_ref.clone()),
                )
                .chain(
                    key.balance_query
                        .as_ref()
                        .and_then(|query| query.access_token_ref.clone()),
                )
        })
        .collect()
}

pub(super) fn is_orphan(reference: &str, live: &std::collections::HashSet<String>) -> bool {
    !live.contains(reference)
        && (!crate::proxy::lan_guard::is_internal_secret_id(reference)
            || reference.starts_with(ACCESS_PREFIX))
}

pub(super) fn protect_key(key: &mut ProviderKey, secrets: &mut SecretStore) -> AppResult<bool> {
    let Some(query) = key.balance_query.as_mut() else {
        return Ok(false);
    };
    let Some(token) = query.access_token.as_ref() else {
        return Ok(false);
    };
    let reference = if token.is_empty() {
        None
    } else if let Some(reference) = query.access_token_ref.as_ref().filter(|reference| {
        secrets
            .get(reference)
            .ok()
            .flatten()
            .is_some_and(|saved| saved.as_str() == token)
    }) {
        Some(reference.clone())
    } else {
        let reference = format!("{ACCESS_PREFIX}{}", uuid::Uuid::new_v4());
        secrets.set(&reference, token)?;
        Some(reference)
    };
    if let Some(token) = query.access_token.as_mut() {
        token.zeroize();
    }
    query.access_token = None;
    query.access_token_ref = reference;
    Ok(true)
}

pub(super) fn same_query(left: &ProviderKey, right: &ProviderKey) -> bool {
    left.base_url == right.base_url
        && left.protocol == right.protocol
        && left.balance_query == right.balance_query
}

pub(crate) struct Credentials {
    pub(crate) config: BalanceQuery,
    pub(crate) secret: Zeroizing<String>,
}

impl Store {
    pub(crate) fn migrate_balance_credentials(&self) -> AppResult<bool> {
        if self.secrets.read().is_locked()
            || !self.config.read().keys.iter().any(|key| {
                key.balance_query
                    .as_ref()
                    .is_some_and(|query| query.access_token.is_some())
            })
        {
            return Ok(false);
        }
        self.mutate_and_persist(|config| {
            let mut secrets = self.secrets.write();
            let mut changed = false;
            for key in &mut config.keys {
                changed |= protect_key(key, &mut secrets)?;
            }
            Ok(changed)
        })
    }

    pub(crate) fn unlock_secrets(&self, password: &str) -> AppResult<()> {
        self.secrets.write().unlock(password)?;
        self.migrate_balance_credentials()?;
        Ok(())
    }

    pub(crate) fn save_credential(&self, reference: &str, secret: &str) -> AppResult<()> {
        self.change_credential(reference, |secrets| secrets.set(reference, secret))
    }

    pub(crate) fn mark_secret_present(&self, key_id: &str) -> AppResult<()> {
        self.mutate_and_persist_when(|config| {
            let Some(key) = config.keys.iter_mut().find(|key| key.id == key_id) else {
                return ((), false);
            };
            let changed = !key.has_secret;
            key.has_secret = true;
            ((), changed)
        })
    }

    pub(crate) fn remove_credential(&self, reference: &str) -> AppResult<()> {
        self.change_credential(reference, |secrets| secrets.remove(reference))
    }

    fn change_credential(
        &self,
        reference: &str,
        change: impl FnOnce(&mut SecretStore) -> AppResult<()>,
    ) -> AppResult<()> {
        let mut config = self.config.write();
        change(&mut self.secrets.write())?;
        for key in &mut config.keys {
            if key.balance_query.as_ref().is_some_and(|query| {
                query.api_key_ref.as_deref().unwrap_or(&key.id) == reference
                    || query.access_token_ref.as_deref() == Some(reference)
            }) {
                key.cached_balance = None;
            }
        }
        Ok(())
    }

    pub(crate) fn balance_credentials(&self, key: &ProviderKey) -> AppResult<Credentials> {
        let mut config = key
            .balance_query
            .clone()
            .ok_or_else(|| AppError::Invalid("该 Key 未配置余额查询".into()))?;
        let secrets = self.secrets.read();
        if secrets.is_locked() {
            return Err(AppError::Invalid("密钥库已锁定，请先用主口令解锁".into()));
        }
        let secret = secrets
            .get(config.api_key_ref.as_deref().unwrap_or(&key.id))?
            .ok_or_else(|| AppError::Invalid("未配置密钥".into()))?;
        if let Some(reference) = config.access_token_ref.as_deref() {
            config.access_token = Some(
                secrets
                    .get(reference)?
                    .ok_or_else(|| AppError::Invalid("未配置余额 Access Token，请重新录入".into()))?
                    .to_string(),
            );
        }
        Ok(Credentials { config, secret })
    }

    pub(crate) fn commit_balance_result(
        &self,
        original: &ProviderKey,
        credentials: &Credentials,
        result: &BalanceResult,
    ) -> AppResult<Option<bool>> {
        let commit = |config: &mut AppConfig| {
            let Some(current) = config.keys.iter_mut().find(|key| key.id == original.id) else {
                return (None, false);
            };
            if !same_query(current, original) {
                return (None, false);
            }
            let secrets = self.secrets.read();
            let reference = credentials
                .config
                .api_key_ref
                .as_deref()
                .unwrap_or(&original.id);
            if !secrets
                .get(reference)
                .ok()
                .flatten()
                .is_some_and(|secret| secret.as_str() == credentials.secret.as_str())
            {
                return (None, false);
            }
            if let Some(reference) = credentials.config.access_token_ref.as_deref() {
                if !secrets.get(reference).ok().flatten().is_some_and(|token| {
                    Some(token.as_str()) == credentials.config.access_token.as_deref()
                }) {
                    return (None, false);
                }
            }
            let mut changed = false;
            if let (Some(template), Some(query)) = (
                &result.resolved_url_template,
                current.balance_query.as_mut(),
            ) {
                if query.url != *template {
                    query.url = template.clone();
                    changed = true;
                }
            }
            current.cached_balance = Some(result.clone());
            (Some(changed), changed)
        };
        if result.resolved_url_template.is_some() {
            self.mutate_and_persist_when(commit)
        } else {
            Ok(commit(&mut self.config.write()).0)
        }
    }

    #[cfg(test)]
    pub fn update_balance_cache(&self, key_id: &str, result: BalanceResult) -> AppResult<()> {
        if let Some(key) = self
            .config
            .write()
            .keys
            .iter_mut()
            .find(|key| key.id == key_id)
        {
            key.cached_balance = Some(result);
        }
        Ok(())
    }

    #[cfg(test)]
    pub fn set_balance_query_url(&self, key_id: &str, template: &str) -> AppResult<bool> {
        self.mutate_and_persist_when(|config| {
            let Some(key) = config.keys.iter_mut().find(|key| key.id == key_id) else {
                return (false, false);
            };
            let Some(query) = key.balance_query.as_mut() else {
                return (false, false);
            };
            if query.url == template {
                return (false, false);
            }
            query.url = template.to_owned();
            key.cached_balance = None;
            (true, true)
        })
    }

    pub fn get_balance_cache(&self, key_id: &str) -> Option<BalanceResult> {
        let config = self.config.read();
        let key = config.keys.iter().find(|key| key.id == key_id)?;
        let cached = key.cached_balance.as_ref()?;
        let interval = key
            .balance_query
            .as_ref()
            .map(|query| query.auto_interval_min)
            .unwrap_or(0);
        let lifetime = if interval > 0 {
            i64::from(interval) * 60 * 9 / 10
        } else {
            5 * 60
        };
        let age = (chrono::Utc::now().timestamp_millis() - cached.queried_at) / 1000;
        (age >= 0 && age < lifetime).then(|| cached.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::service::tests::temp_store;

    fn configured(store: &Store) -> ProviderKey {
        let key = store
            .upsert_key(ProviderKey {
                id: "balance-owner".into(),
                balance_query: Some(BalanceQuery {
                    access_token: Some("panel-secret".into()),
                    ..Default::default()
                }),
                ..Default::default()
            })
            .unwrap();
        store.save_credential(&key.id, "api-secret").unwrap();
        key
    }

    fn result() -> BalanceResult {
        BalanceResult {
            ok: true,
            remaining: Some(0.0),
            queried_at: chrono::Utc::now().timestamp_millis(),
            ..BalanceResult::failed("")
        }
    }

    #[test]
    fn panel_tokens_are_encrypted_and_can_be_replaced_or_cleared() {
        let (store, directory) = temp_store("balance_secrets");
        let mut key = configured(&store);
        let reference = key
            .balance_query
            .as_ref()
            .unwrap()
            .access_token_ref
            .clone()
            .unwrap();
        assert!(key.balance_query.as_ref().unwrap().access_token.is_none());
        assert_eq!(
            store
                .secrets
                .read()
                .get(&reference)
                .unwrap()
                .unwrap()
                .as_str(),
            "panel-secret"
        );
        assert!(!std::fs::read_to_string(directory.join("config.json"))
            .unwrap()
            .contains("panel-secret"));
        assert!(!std::fs::read_to_string(directory.join("secrets.enc"))
            .unwrap()
            .contains("panel-secret"));
        key.balance_query.as_mut().unwrap().access_token = Some("replacement".into());
        key = store.upsert_key(key).unwrap();
        assert_ne!(
            key.balance_query
                .as_ref()
                .unwrap()
                .access_token_ref
                .as_deref(),
            Some(reference.as_str())
        );
        assert_eq!(
            store
                .balance_credentials(&key)
                .unwrap()
                .config
                .access_token
                .as_deref(),
            Some("replacement")
        );
        key.balance_query.as_mut().unwrap().access_token = Some(String::new());
        key = store.upsert_key(key).unwrap();
        assert!(key
            .balance_query
            .as_ref()
            .unwrap()
            .access_token_ref
            .is_none());
    }

    #[test]
    fn cache_survives_metadata_edits_but_not_query_or_secret_edits() {
        let (store, _) = temp_store("balance_invalidation");
        let mut key = configured(&store);
        store.update_balance_cache(&key.id, result()).unwrap();
        key.name = "renamed".into();
        key = store.upsert_key(key).unwrap();
        assert!(store.get_balance_cache(&key.id).is_some());
        key.base_url = "https://changed.example".into();
        key = store.upsert_key(key).unwrap();
        assert!(store.get_balance_cache(&key.id).is_none());
        store.update_balance_cache(&key.id, result()).unwrap();
        store.save_credential(&key.id, "new-api-secret").unwrap();
        assert!(store.get_balance_cache(&key.id).is_none());
    }

    #[test]
    fn shared_credentials_invalidate_all_dependent_balances() {
        let (store, _) = temp_store("balance_shared_secret");
        let key = configured(&store);
        let mut dependent = key.clone();
        dependent.id = "balance-dependent".into();
        dependent.balance_query.as_mut().unwrap().api_key_ref = Some(key.id.clone());
        store.upsert_key(dependent.clone()).unwrap();
        for candidate in [&key, &dependent] {
            store.update_balance_cache(&candidate.id, result()).unwrap();
        }
        store.save_credential(&key.id, "changed").unwrap();
        for candidate in [&key, &dependent] {
            assert!(store.get_balance_cache(&candidate.id).is_none());
        }
    }

    #[test]
    fn old_queries_cannot_restore_cache_or_overwrite_a_new_url() {
        let (store, _) = temp_store("balance_stale");
        let key = configured(&store);
        let credentials = store.balance_credentials(&key).unwrap();
        let mut stale = result();
        stale.resolved_url_template = Some("{{origin}}/old-probe".into());
        let mut updated = key.clone();
        updated.balance_query.as_mut().unwrap().url = "{{origin}}/new-url".into();
        store.upsert_key(updated).unwrap();
        assert_eq!(
            store
                .commit_balance_result(&key, &credentials, &stale)
                .unwrap(),
            None
        );
        assert!(store.get_balance_cache(&key.id).is_none());
        assert_eq!(
            store.get_key(&key.id).unwrap().balance_query.unwrap().url,
            "{{origin}}/new-url"
        );
        let current = store.get_key(&key.id).unwrap();
        let credentials = store.balance_credentials(&current).unwrap();
        store
            .save_credential(&key.id, "changed-during-request")
            .unwrap();
        assert_eq!(
            store
                .commit_balance_result(&current, &credentials, &stale)
                .unwrap(),
            None
        );
    }

    #[test]
    fn unreferenced_panel_tokens_can_be_pruned_without_removing_the_current_token() {
        let (store, _) = temp_store("balance_token_cleanup");
        let mut key = configured(&store);
        let previous = key
            .balance_query
            .as_ref()
            .unwrap()
            .access_token_ref
            .clone()
            .unwrap();
        key.balance_query.as_mut().unwrap().access_token = Some("replacement".into());
        let current = store
            .upsert_key(key)
            .unwrap()
            .balance_query
            .unwrap()
            .access_token_ref
            .unwrap();
        assert_eq!(store.count_orphan_secrets(), 1);
        assert_eq!(store.prune_orphan_secrets(), 1);
        assert!(store.secrets.read().get(&previous).unwrap().is_none());
        assert_eq!(
            store
                .secrets
                .read()
                .get(&current)
                .unwrap()
                .unwrap()
                .as_str(),
            "replacement"
        );
    }

    #[test]
    fn failed_configuration_write_preserves_the_original_encrypted_token() {
        let (store, directory) = temp_store("balance_failed_write");
        let mut key = configured(&store);
        let previous = key.balance_query.as_ref().unwrap().access_token_ref.clone();
        std::fs::remove_file(directory.join("config.json")).unwrap();
        std::fs::create_dir(directory.join("config.json")).unwrap();
        key.balance_query.as_mut().unwrap().access_token = Some("replacement".into());
        assert!(store.upsert_key(key).is_err());
        let actual = store.get_key("balance-owner").unwrap();
        assert_eq!(
            actual.balance_query.as_ref().unwrap().access_token_ref,
            previous
        );
        assert_eq!(
            store
                .balance_credentials(&actual)
                .unwrap()
                .config
                .access_token
                .as_deref(),
            Some("panel-secret")
        );
    }

    #[test]
    fn removing_a_shared_api_credential_clears_dependent_balances() {
        let (store, _) = temp_store("balance_remove_credential");
        let mut key = configured(&store);
        key.balance_query.as_mut().unwrap().api_key_ref = Some("shared-credential".into());
        let key = store.upsert_key(key).unwrap();
        store
            .save_credential("shared-credential", "shared-value")
            .unwrap();
        store.update_balance_cache(&key.id, result()).unwrap();
        store.remove_credential("shared-credential").unwrap();
        assert!(store.get_balance_cache(&key.id).is_none());
    }
}

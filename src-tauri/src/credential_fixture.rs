use super::SecretStore;
use crate::{error::AppResult, store::Store};
use std::path::PathBuf;

const PASSWORD: &str = "Fixture-Cedar-492!";

impl SecretStore {
    pub(crate) fn load_credential_fixture(path: PathBuf) -> AppResult<Self> {
        let mut vault = Self::load(path)?;
        vault.prepare_credential_fixture()?;
        Ok(vault)
    }

    pub(crate) fn prepare_credential_fixture(&mut self) -> AppResult<()> {
        if self.master_state().required {
            if self.is_locked() {
                self.unlock(PASSWORD)?;
            } else if !self.is_master_mode() {
                self.enable_master_password(PASSWORD)?;
            }
        }
        Ok(())
    }

    pub(crate) fn set_fixture_password(&mut self, password: &str) -> AppResult<usize> {
        if self.is_master_mode() {
            self.change_master_password(PASSWORD, password)
        } else {
            self.enable_master_password(password)
        }
    }
}

impl Store {
    pub(crate) fn credential_fixture(config: PathBuf, secrets: PathBuf) -> AppResult<Self> {
        let store = Self::new_at(config, secrets)?;
        store.secrets.write().prepare_credential_fixture()?;
        Ok(store)
    }
}

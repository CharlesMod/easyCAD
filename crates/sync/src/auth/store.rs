//! Where the refresh token lives: the OS keychain (macOS Keychain, Windows
//! Credential Manager, the Secret Service on Linux), or memory in tests.
//! The methods block, so async code calls them through `spawn_blocking`.

use std::sync::Mutex;

use crate::error::AuthError;

pub trait TokenStore: Send + Sync {
    fn load(&self) -> Result<Option<String>, AuthError>;
    fn save(&self, refresh_token: &str) -> Result<(), AuthError>;
    /// Deleting a token that is not there is not an error.
    fn delete(&self) -> Result<(), AuthError>;
}

/// The OS keychain entry `service`/`account`.
pub struct KeychainStore {
    service: String,
    account: String,
}

impl KeychainStore {
    pub fn new(service: &str, account: &str) -> KeychainStore {
        KeychainStore { service: service.into(), account: account.into() }
    }

    /// The entry easyCAD uses: service `easycad`, account `google-drive`.
    pub fn default_entry() -> KeychainStore {
        KeychainStore::new("easycad", "google-drive")
    }

    fn entry(&self) -> Result<keyring::Entry, AuthError> {
        keyring::Entry::new(&self.service, &self.account).map_err(|e| AuthError::Keychain(e.to_string()))
    }
}

impl TokenStore for KeychainStore {
    fn load(&self) -> Result<Option<String>, AuthError> {
        match self.entry()?.get_password() {
            Ok(t) => Ok(Some(t)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(AuthError::Keychain(e.to_string())),
        }
    }
    fn save(&self, refresh_token: &str) -> Result<(), AuthError> {
        self.entry()?.set_password(refresh_token).map_err(|e| AuthError::Keychain(e.to_string()))
    }
    fn delete(&self) -> Result<(), AuthError> {
        match self.entry()?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(AuthError::Keychain(e.to_string())),
        }
    }
}

/// An in-memory store for tests.
#[derive(Default)]
pub struct MemoryStore {
    token: Mutex<Option<String>>,
}

impl MemoryStore {
    pub fn with_token(t: &str) -> MemoryStore {
        MemoryStore { token: Mutex::new(Some(t.into())) }
    }
    pub fn get(&self) -> Option<String> {
        self.token.lock().unwrap().clone()
    }
}

impl TokenStore for MemoryStore {
    fn load(&self) -> Result<Option<String>, AuthError> {
        Ok(self.get())
    }
    fn save(&self, refresh_token: &str) -> Result<(), AuthError> {
        *self.token.lock().unwrap() = Some(refresh_token.into());
        Ok(())
    }
    fn delete(&self) -> Result<(), AuthError> {
        *self.token.lock().unwrap() = None;
        Ok(())
    }
}

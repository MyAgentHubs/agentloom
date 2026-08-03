#![allow(dead_code)]

use std::collections::HashMap;
use std::sync::Mutex;

const SERVICE: &str = "agentloom";
pub const SEARCH_BRAVE_KEY_ID: &str = "search.brave";
pub const SEARCH_EXA_KEY_ID: &str = "search.exa";

pub trait KeyStore {
    fn set(&self, id: &str, key: &str) -> Result<(), String>;
    fn get(&self, id: &str) -> Result<Option<String>, String>;
    fn delete(&self, id: &str) -> Result<(), String>;
}

pub struct KeyringStore;

impl KeyringStore {
    fn entry(id: &str) -> Result<keyring::Entry, String> {
        let account = format!("agent:{id}");
        keyring::Entry::new(SERVICE, &account).map_err(|e| e.to_string())
    }
}

impl KeyStore for KeyringStore {
    fn set(&self, id: &str, key: &str) -> Result<(), String> {
        Self::entry(id)?
            .set_password(key)
            .map_err(|e| e.to_string())
    }

    fn get(&self, id: &str) -> Result<Option<String>, String> {
        match Self::entry(id)?.get_password() {
            Ok(key) => Ok(Some(key)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(e.to_string()),
        }
    }

    fn delete(&self, id: &str) -> Result<(), String> {
        match Self::entry(id)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(e.to_string()),
        }
    }
}

/// Import the legacy DeepSeek env key once when the profile has no stored key.
pub fn import_legacy_deepseek_key(
    store: &dyn KeyStore,
    id: &str,
    has_key: bool,
    env_val: Option<String>,
) -> Result<Option<String>, String> {
    if has_key {
        return Ok(None);
    }

    let Some(v) = env_val else {
        return Ok(None);
    };
    if v.is_empty() {
        return Ok(None);
    }

    store.set(id, &v)?;
    Ok(Some(v))
}

fn search_key_id(backend: &str) -> &'static str {
    match backend {
        "exa" => SEARCH_EXA_KEY_ID,
        _ => SEARCH_BRAVE_KEY_ID,
    }
}

pub fn set_search_key_with_store(
    store: &dyn KeyStore,
    backend: &str,
    key: &str,
) -> Result<(), String> {
    store.set(search_key_id(backend), key)
}

/// Backend-only plaintext read for spawning agents; IPC must expose configured state only.
pub fn get_search_key_with_store(
    store: &dyn KeyStore,
    backend: &str,
) -> Result<Option<String>, String> {
    store.get(search_key_id(backend))
}

pub fn search_key_configured_with_store(
    store: &dyn KeyStore,
    backend: &str,
) -> Result<bool, String> {
    Ok(get_search_key_with_store(store, backend)?
        .map(|key| !key.trim().is_empty())
        .unwrap_or(false))
}

#[derive(Default)]
pub struct FakeKeyStore {
    keys: Mutex<HashMap<String, String>>,
}

impl KeyStore for FakeKeyStore {
    fn set(&self, id: &str, key: &str) -> Result<(), String> {
        let mut keys = self.keys.lock().map_err(|e| e.to_string())?;
        keys.insert(id.to_string(), key.to_string());
        Ok(())
    }

    fn get(&self, id: &str) -> Result<Option<String>, String> {
        let keys = self.keys.lock().map_err(|e| e.to_string())?;
        Ok(keys.get(id).cloned())
    }

    fn delete(&self, id: &str) -> Result<(), String> {
        let mut keys = self.keys.lock().map_err(|e| e.to_string())?;
        keys.remove(id);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{import_legacy_deepseek_key, FakeKeyStore, KeyStore, KeyringStore};

    #[test]
    fn search_key_set_get_roundtrip_and_configured() {
        use super::{
            get_search_key_with_store, search_key_configured_with_store, set_search_key_with_store,
        };

        let store = FakeKeyStore::default();

        assert!(!search_key_configured_with_store(&store, "brave").unwrap());
        set_search_key_with_store(&store, "brave", "brave-k").unwrap();
        assert_eq!(
            get_search_key_with_store(&store, "brave").unwrap(),
            Some("brave-k".to_string())
        );
        assert!(search_key_configured_with_store(&store, "brave").unwrap());
    }

    #[test]
    fn per_backend_keys_do_not_overwrite() {
        use super::{
            get_search_key_with_store, search_key_configured_with_store, set_search_key_with_store,
            FakeKeyStore,
        };
        let store = FakeKeyStore::default();
        set_search_key_with_store(&store, "brave", "bk").unwrap();
        set_search_key_with_store(&store, "exa", "ek").unwrap();
        assert_eq!(
            get_search_key_with_store(&store, "brave").unwrap(),
            Some("bk".into())
        );
        assert_eq!(
            get_search_key_with_store(&store, "exa").unwrap(),
            Some("ek".into())
        );
        assert!(search_key_configured_with_store(&store, "exa").unwrap());
        // 空 key 不算「已配置」（spec：exa 空 key UI 不显示已配置）
        set_search_key_with_store(&store, "exa", "   ").unwrap();
        assert!(!search_key_configured_with_store(&store, "exa").unwrap());
    }

    #[test]
    fn fake_set_get_roundtrip() {
        let store = FakeKeyStore::default();

        store.set("agent-1", "secret-value").unwrap();

        assert_eq!(
            store.get("agent-1").unwrap(),
            Some("secret-value".to_string())
        );
    }

    #[test]
    fn fake_get_missing_none() {
        let store = FakeKeyStore::default();

        assert_eq!(store.get("missing").unwrap(), None);
    }

    #[test]
    fn fake_delete_then_missing() {
        let store = FakeKeyStore::default();

        store.set("agent-1", "secret-value").unwrap();
        store.delete("agent-1").unwrap();

        assert_eq!(store.get("agent-1").unwrap(), None);
        assert!(store.delete("missing").is_ok());
    }

    #[test]
    fn import_when_no_key_and_env() {
        let store = FakeKeyStore::default();

        let imported =
            import_legacy_deepseek_key(&store, "deepseek", false, Some("sk-x".to_string()))
                .unwrap();

        assert_eq!(imported, Some("sk-x".to_string()));
        assert_eq!(store.get("deepseek").unwrap(), Some("sk-x".to_string()));
    }

    #[test]
    fn import_skip_when_has_key() {
        let store = FakeKeyStore::default();

        let imported =
            import_legacy_deepseek_key(&store, "deepseek", true, Some("sk-x".to_string())).unwrap();

        assert_eq!(imported, None);
        assert_eq!(store.get("deepseek").unwrap(), None);
    }

    #[test]
    fn import_skip_when_no_env() {
        let store = FakeKeyStore::default();

        let imported = import_legacy_deepseek_key(&store, "deepseek", false, None).unwrap();

        assert_eq!(imported, None);
        assert_eq!(store.get("deepseek").unwrap(), None);
    }

    #[test]
    fn import_skip_when_env_empty() {
        let store = FakeKeyStore::default();

        let imported =
            import_legacy_deepseek_key(&store, "deepseek", false, Some(String::new())).unwrap();

        assert_eq!(imported, None);
        assert_eq!(store.get("deepseek").unwrap(), None);
    }

    #[test]
    #[ignore]
    fn keyring_real_roundtrip() {
        let store = KeyringStore;
        let id = "keyring-real-roundtrip";
        let key = "secret-value";

        store.delete(id).unwrap();
        store.set(id, key).unwrap();

        assert_eq!(store.get(id).unwrap(), Some(key.to_string()));

        store.delete(id).unwrap();
        assert_eq!(store.get(id).unwrap(), None);
    }
}

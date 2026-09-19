#![cfg(test)]

use super::*;

struct WriteOnlyKeyStore;

impl KeyStore for WriteOnlyKeyStore {
    fn set(&self, _id: &str, _key: &str) -> Result<(), String> {
        Ok(())
    }

    fn get(&self, _id: &str) -> Result<Option<String>, String> {
        Ok(None)
    }

    fn delete(&self, _id: &str) -> Result<(), String> {
        Ok(())
    }
}

#[test]
fn delete_agent_also_clears_key() {
    let conn = crate::test_support::mem_db();
    let store = FakeKeyStore::default();
    let profile = agent_profile("borrow-agent", true, false);
    db::upsert_agent(&conn, &profile).unwrap();
    store.set(&profile.id, "k").unwrap();

    delete_agent_with_store(&conn, &store, &profile.id).unwrap();

    assert!(db::get_agent(&conn, &profile.id).unwrap().is_none());
    assert_eq!(store.get(&profile.id).unwrap(), None);
}

#[test]
fn resolve_agent_name_snapshot_finds_profile_name() {
    // Finding A：flush 前查一次 profile 名字，reload 后不该回退到裸 agent_id。
    let conn = crate::test_support::mem_db();
    let profile = agent_profile("deepseek", false, false);
    db::upsert_agent(&conn, &profile).unwrap();

    let snapshot = resolve_agent_name_snapshot(&conn, "deepseek");

    assert_eq!(snapshot.as_deref(), Some(profile.name.as_str()));
}

#[test]
fn resolve_agent_name_snapshot_missing_agent_returns_none() {
    // 查不到就 None，不炸——对齐 agent 已被删除等边界情况。
    let conn = crate::test_support::mem_db();

    let snapshot = resolve_agent_name_snapshot(&conn, "does-not-exist");

    assert_eq!(snapshot, None);
}

#[test]
fn delete_native_via_store_rejected() {
    let conn = crate::test_support::mem_db();
    let store = FakeKeyStore::default();
    let mut profile = agent_profile("native-agent", true, false);
    profile.access = "native".into();
    db::upsert_agent(&conn, &profile).unwrap();
    store.set(&profile.id, "k").unwrap();

    assert!(delete_agent_with_store(&conn, &store, &profile.id).is_err());

    assert!(db::get_agent(&conn, &profile.id).unwrap().is_some());
    assert_eq!(store.get(&profile.id).unwrap(), Some("k".to_string()));
}

#[test]
fn upsert_native_agent_profile_edit_allowed() {
    let conn = crate::test_support::mem_db();
    let mut profile = agent_profile("native-agent", true, false);
    profile.access = "native".into();
    profile.provider = "claude".into();
    profile.primary_model = Some("sonnet".into());
    profile.reasoning_default = "medium".into();
    db::upsert_agent(&conn, &profile).unwrap();

    let mut edited = profile.clone();
    edited.name = "Edited Native".into();
    edited.primary_model = Some("opus".into());
    edited.reasoning_default = "high".into();
    edited.has_key = false;

    assert!(upsert_agent_guarded(&conn, &edited).is_ok());
    let stored = db::get_agent(&conn, &profile.id).unwrap().unwrap();
    assert_eq!(stored.name, "Edited Native");
    assert_eq!(stored.access, "native");
    assert_eq!(stored.primary_model.as_deref(), Some("opus"));
    assert_eq!(stored.reasoning_default, "high");
    assert!(!stored.has_key);
}

#[test]
fn upsert_native_agent_cannot_switch_to_borrow() {
    let conn = crate::test_support::mem_db();
    let mut profile = agent_profile("native-agent", true, false);
    profile.access = "native".into();
    db::upsert_agent(&conn, &profile).unwrap();

    let mut edited = profile.clone();
    edited.access = "borrow".into();
    edited.has_key = false;

    let err = upsert_agent_guarded(&conn, &edited).unwrap_err();

    assert_eq!(err, "AL_ERR:agent.nativeAccessImmutable");
    let stored = db::get_agent(&conn, &profile.id).unwrap().unwrap();
    assert_eq!(stored.access, profile.access);
    assert_eq!(stored.has_key, profile.has_key);
}

#[test]
fn upsert_borrow_agent_edit_allowed() {
    let conn = crate::test_support::mem_db();
    let profile = agent_profile("borrow-agent", true, false);
    db::upsert_agent(&conn, &profile).unwrap();

    let mut edited = profile.clone();
    edited.name = "Edited Borrow".into();

    assert!(upsert_agent_guarded(&conn, &edited).is_ok());
    let stored = db::get_agent(&conn, &profile.id).unwrap().unwrap();
    assert_eq!(stored.name, "Edited Borrow");
}

#[test]
fn upsert_new_agent_allowed() {
    let conn = crate::test_support::mem_db();
    let mut profile = agent_profile("new-native-agent", true, false);
    profile.access = "native".into();

    assert!(upsert_agent_guarded(&conn, &profile).is_ok());
    let stored = db::get_agent(&conn, &profile.id).unwrap().unwrap();
    assert_eq!(stored.access, "native");
}

#[test]
fn set_agent_key_sets_has_key() {
    let conn = crate::test_support::mem_db();
    let store = FakeKeyStore::default();
    let profile = agent_profile("borrow-agent", false, false);
    db::upsert_agent(&conn, &profile).unwrap();

    set_agent_key_with_store(&conn, &store, &profile.id, "k").unwrap();

    assert_eq!(store.get(&profile.id).unwrap(), Some("k".to_string()));
    assert!(db::get_agent(&conn, &profile.id).unwrap().unwrap().has_key);
}

#[test]
fn agent_key_set_success_without_readback_fails_and_keeps_has_key_false() {
    let conn = crate::test_support::mem_db();
    let profile = agent_profile("borrow-agent", false, false);
    db::upsert_agent(&conn, &profile).unwrap();

    let err = set_agent_key_with_store(&conn, &WriteOnlyKeyStore, &profile.id, "k").unwrap_err();

    assert!(err.starts_with("AL_ERR:agent.keychainSaveFailed"), "{err}");
    assert!(!db::get_agent(&conn, &profile.id).unwrap().unwrap().has_key);
}

#[test]
fn agent_key_harness_claimed_key_missing_from_store_has_actionable_error() {
    let conn = crate::test_support::mem_db();
    db::create_session(
        &conn,
        "s-harness-missing-key",
        "x",
        "local-default",
        "local",
    )
    .unwrap();
    let mut profile = agent_profile("deepseek", true, false);
    profile.access = "harness".into();
    profile.provider = "deepseek".into();
    db::upsert_agent(&conn, &profile).unwrap();

    let err = match build_send_plan(
        &conn,
        "s-harness-missing-key",
        "test-run",
        &profile.id,
        "hi",
        None,
        &[],
        &FakeKeyStore::default(),
        Locale::En,
    ) {
        Ok(_) => {
            panic!("configured harness agent must fail before spawn when its key is missing")
        }
        Err(err) => err,
    };

    assert!(
        err.starts_with("AL_ERR:agent.keychainKeyUnavailable:"),
        "{err}"
    );
    assert!(
        err.contains("Open Settings and save this agent's API key again"),
        "{err}"
    );
}

#[test]
fn agent_key_harness_without_configured_key_keeps_optional_key_behavior() {
    let mut profile = agent_profile("keyless-harness", false, false);
    profile.access = "harness".into();

    assert!(validate_harness_agent_key(&profile, None, Locale::En).is_ok());
}

#[test]
fn set_agent_key_native_rejected() {
    let conn = crate::test_support::mem_db();
    let store = FakeKeyStore::default();
    let mut profile = agent_profile("native-agent", false, false);
    profile.access = "native".into();
    db::upsert_agent(&conn, &profile).unwrap();

    let err = set_agent_key_with_store(&conn, &store, &profile.id, "k").unwrap_err();

    assert_eq!(err, "AL_ERR:agent.nativeKeyUnsupported");
    let stored = db::get_agent(&conn, &profile.id).unwrap().unwrap();
    assert!(!stored.has_key);
    assert_eq!(store.get(&profile.id).unwrap(), None);
}

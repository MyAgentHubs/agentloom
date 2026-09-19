#![cfg(test)]

use super::*;

#[test]
fn lead_step_budget_card_is_bilingual_with_unchanged_zh() {
    assert_eq!(
        lead_step_budget_action(Locale::Zh),
        lead_action::LeadAction::AskUser {
            rationale: "本会话 lead_step 已达到预算上限".into(),
            question: "我已经连续做了很多轮判断。要继续自动推进，还是先停下确认下一步？".into(),
            options: vec!["继续".into(), "先停下".into()],
            recommended: Some("先停下".into()),
        }
    );
    assert_eq!(
            lead_step_budget_action(Locale::En),
            lead_action::LeadAction::AskUser {
                rationale: "This session has reached the lead_step budget limit".into(),
                question:
                    "I've made many consecutive decisions. Continue automatically, or stop and confirm the next step?"
                        .into(),
                options: vec!["Continue".into(), "Stop for now".into()],
                recommended: Some("Stop for now".into()),
            }
        );
}

struct LockFreeProbeStore<'a> {
    db: &'a Db,
    calls: std::cell::Cell<u32>,
    observed_lock_free: std::cell::Cell<bool>,
}

impl<'a> KeyStore for LockFreeProbeStore<'a> {
    fn get(&self, _id: &str) -> Result<Option<String>, String> {
        self.calls.set(self.calls.get() + 1);
        self.observed_lock_free.set(self.db.0.try_lock().is_ok());
        Ok(Some("probe-key".to_string()))
    }

    fn set(&self, _id: &str, _key: &str) -> Result<(), String> {
        Ok(())
    }

    fn delete(&self, _id: &str) -> Result<(), String> {
        Ok(())
    }
}

#[test]
fn resolve_harness_search_creds_calls_keychain_with_db_lock_released() {
    let conn = crate::test_support::mem_db();
    let db = Db(crate::perf_probe::TimedMutex::new(conn));
    let mut profile = agent_profile("harness-agent", false, false);
    profile.access = "harness".into();
    let probe = LockFreeProbeStore {
        db: &db,
        calls: std::cell::Cell::new(0),
        observed_lock_free: std::cell::Cell::new(false),
    };

    let creds = resolve_harness_search_creds(&db, &profile, &probe).unwrap();

    assert_eq!(probe.calls.get(), 1);
    assert!(
        probe.observed_lock_free.get(),
        "search key 钥匙串 IPC 必须在 db.0.lock() 释放之后发生"
    );
    assert_eq!(creds.key.as_deref(), Some("probe-key"));
    assert_eq!(creds.backend.as_deref(), Some("brave"));
}

#[test]
fn resolve_harness_search_creds_skips_keychain_for_non_harness_profile() {
    let conn = crate::test_support::mem_db();
    let db = Db(crate::perf_probe::TimedMutex::new(conn));
    for access in ["native", "borrow"] {
        let mut profile = agent_profile("some-agent", false, false);
        profile.access = access.into();
        let probe = LockFreeProbeStore {
            db: &db,
            calls: std::cell::Cell::new(0),
            observed_lock_free: std::cell::Cell::new(false),
        };

        let creds = resolve_harness_search_creds(&db, &profile, &probe).unwrap();

        assert_eq!(
            probe.calls.get(),
            0,
            "access={access} 不应触发任何钥匙串 IPC"
        );
        assert_eq!(creds.key, None);
        assert_eq!(creds.backend, None);
    }
}

#[test]
fn should_seed_goal_p1_1_resume_without_message_never_seeds() {
    // P1-①（opus 对抗审）核心钉子：message=None（try_resume_pending 续跑路径）即使
    // 会话没有既存 goal，也绝不该 seed——不然占位兜底文案会被永久写进 goal memory
    // block。变异测试：把 `should_seed_goal` 里的 `has_message` 条件删掉/永真化，
    // 这条测试立刻变红。
    assert!(!should_seed_goal(false, false));
}

#[test]
fn should_seed_goal_first_real_message_seeds_when_no_existing_goal() {
    // 真实首轮消息路径（message=Some）行为不变：没有既存 goal 时正常 seed。
    assert!(should_seed_goal(false, true));
}

#[test]
fn should_seed_goal_never_reseeds_when_goal_already_exists() {
    // 已有 goal 时无论 message 有没有都不 clobber。
    assert!(!should_seed_goal(true, true));
    assert!(!should_seed_goal(true, false));
}

#[test]
fn lead_engine_for_profile_native_claude_ignores_cap_lead() {
    // native claude 从不看 cap_lead（无论有值还是 NULL）。
    let mut profile = lead_capable_profile("native-claude-cap");
    assert_eq!(
        lead_engine_for_profile(&profile),
        Ok(LeadEngine::NativeClaude)
    );
    profile.cap_lead = None;
    assert_eq!(
        lead_engine_for_profile(&profile),
        Ok(LeadEngine::NativeClaude)
    );
}

#[test]
fn lead_engine_for_profile_borrow_ignores_cap_lead() {
    // L1b 拍板：「能不能当 lead」是代码级属性，不读 cap_lead——存量 borrow agent 的
    // cap_lead 全 NULL 也必须放行，有值同样放行（不因为标了才行）。
    let mut profile = borrow_lead_capable_profile("borrow-lead-nullcap");
    profile.cap_lead = None;
    assert_eq!(
        lead_engine_for_profile(&profile),
        Ok(LeadEngine::BorrowClaude)
    );
    profile.cap_lead = Some("borrow_claude".to_string());
    assert_eq!(
        lead_engine_for_profile(&profile),
        Ok(LeadEngine::BorrowClaude)
    );
}

#[test]
fn lead_engine_for_profile_codex_native_errs_not_supported() {
    // 哪怕用户手填了 cap_lead，L1 spawn 接不住 codex——门禁不能放行。
    let mut profile = native_codex_profile("codex-native-cap");
    profile.cap_lead = Some("someone_filled_this_in".to_string());
    let err = lead_engine_for_profile(&profile).unwrap_err();
    assert!(
        err.starts_with("AL_ERR:lead.engineNotSupported:"),
        "expected lead.engineNotSupported envelope, got: {err}"
    );
}

#[test]
fn lead_engine_for_profile_harness_ignores_cap_lead() {
    // L3 A1：harness（myagent 引擎）现在可以当队长——与 borrow 同样不看 cap_lead，
    // provider 是「哪个 LLM 供应商」（deepseek/glm/...），与「能不能当 lead」无关。
    let mut profile = harness_lead_capable_profile("harness-lead-nullcap");
    profile.cap_lead = None;
    assert_eq!(lead_engine_for_profile(&profile), Ok(LeadEngine::Harness));
    profile.cap_lead = Some("whatever".to_string());
    assert_eq!(lead_engine_for_profile(&profile), Ok(LeadEngine::Harness));
}

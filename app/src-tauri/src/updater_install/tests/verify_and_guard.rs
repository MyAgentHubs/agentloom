#![cfg(test)]

use super::*;

// -------------------------------------------------------------
// default_verify (the real codesign/spctl/stapler verdict)
//
// `run_check` (a private fn in staging_orchestrate.rs) is not re-exported
// from the `updater_install` module — the host file only does
// `pub(crate) use staging_orchestrate::{default_verify, stage_bytes};`,
// never `use staging_orchestrate::*;` — so `run_check` is unreachable from
// this file's `use super::*` chain, and cannot be unit-tested directly
// against `/usr/bin/true` / `/usr/bin/false`. No production visibility was
// widened to accommodate a test. The `default_verify` test below instead
// goes through its only public entry point and asserts `Err` for an empty
// `.app` that is guaranteed to fail verification — if `run_check`'s result
// were swallowed (`?` turned into `let _ =`), `default_verify` would return
// `Ok(())` for this same empty bundle too, so this still turns red for that
// class of mutation.
// -------------------------------------------------------------

#[test]
fn default_verify_rejects_empty_fake_app_bundle() {
    let tmp = tempfile::tempdir().unwrap();
    let fake_app = tmp.path().join("Fake.app");
    fs::create_dir_all(&fake_app).unwrap();

    let result = default_verify(&fake_app);

    assert!(
        result.is_err(),
        "codesign --verify 对一个空 .app 目录必须失败，而不是被静默吞掉"
    );
}

// -------------------------------------------------------------
// StagingGuard::drop (panic-time cleanup safety net)
// -------------------------------------------------------------

#[test]
fn staging_guard_cleans_up_when_verify_panics() {
    let tmp = tempfile::tempdir().unwrap();
    let bundle = make_installed_bundle(tmp.path(), "0.2.9");
    let archive = minimal_valid_archive("0.3.0");

    let panicking_verify = |_p: &Path| -> Result<(), String> {
        panic!("injected panic inside verify() for StagingGuard::drop coverage")
    };

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        stage_bytes(&bundle, &archive, "0.3.0", &panicking_verify)
    }));

    assert!(
        result.is_err(),
        "verify() 里的 panic 必须真的沿调用栈 unwind 出来"
    );
    assert!(
        staging_leftovers(tmp.path()).is_empty(),
        "verify() panic 之后，StagingGuard::drop 必须清理掉暂存目录"
    );
}

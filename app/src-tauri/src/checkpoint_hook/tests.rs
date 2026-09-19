#![cfg(test)]

use super::*;
use std::ffi::OsString;
use std::process::Command;

struct HomeGuard {
    old_home: Option<OsString>,
    _lock: std::sync::MutexGuard<'static, ()>,
}

impl HomeGuard {
    fn set(path: &Path) -> Self {
        let lock = crate::worktree::test_home_lock();
        let old_home = std::env::var_os("HOME");
        std::env::set_var("HOME", path);
        Self {
            old_home,
            _lock: lock,
        }
    }
}

impl Drop for HomeGuard {
    fn drop(&mut self) {
        match &self.old_home {
            Some(value) => std::env::set_var("HOME", value),
            None => std::env::remove_var("HOME"),
        }
    }
}

fn archived_preimage(
    entry: &crate::checkpoint::CheckpointEntry,
    session_id: &str,
    run_id: &str,
) -> String {
    fs::read_to_string(
        crate::worktree::logs_dir()
            .parent()
            .unwrap()
            .join("checkpoints")
            .join(session_id)
            .join(run_id)
            .join("blobs")
            .join(entry.blob_sha.as_ref().unwrap()),
    )
    .unwrap()
}

mod checkpoint_lifecycle;
mod real_clients;
mod request_handling;
mod server_resilience;
mod stop_hook;

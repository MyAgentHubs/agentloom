use std::time::Duration;

#[cfg(test)]
use super::{CheckOutcome, Machine, UpdaterState};
#[cfg(test)]
use std::path::{Path, PathBuf};
#[cfg(test)]
use std::sync::atomic::AtomicBool;
#[cfg(test)]
use std::sync::Mutex;

/// Placeholder used before a production key is generated in `tauri.conf.json`;
/// when the configured `pubkey` is empty or equals this value, the state machine
/// enters `Disabled{Unsigned}` and never invokes the plugin.
const PLACEHOLDER_PUBKEY: &str = "REPLACE_WITH_REAL_PUBKEY";

const CHECK_TIMEOUT: Duration = Duration::from_secs(15);
/// A check future may synchronously block its Tokio worker during a poll, for example while
/// validating TLS certificates. A timer in that same task cannot be polled while this happens.
/// Run the check in a separate Tokio task and time out its JoinHandle so the state machine can
/// leave `Checking`. `timeout(deadline, run_check())` in one task cannot cover this case.
/// This deadline exceeds the plugin/reqwest timeout and only fires as a last resort.
const CHECK_HARD_DEADLINE: Duration = Duration::from_secs(45);
const _: () = assert!(CHECK_HARD_DEADLINE.as_millis() > CHECK_TIMEOUT.as_millis());
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(10 * 60);
const DOWNLOAD_WATCHDOG_IDLE: Duration = Duration::from_secs(60);
const DOWNLOAD_WATCHDOG_POLL: Duration = Duration::from_secs(5);
const SCHEDULER_INITIAL_DELAY: Duration = Duration::from_secs(30);
const SCHEDULER_INTERVAL: Duration = Duration::from_secs(6 * 60 * 60);

const SKIPPED_VERSION_SETTING: &str = "updater.skipped_version";

mod download_relaunch;
mod setup_check;
mod startup_recovery;

#[cfg(test)]
use download_relaunch::*;
use setup_check::*;
use startup_recovery::*;

pub(super) use download_relaunch::{
    discard_update, download_and_install, download_with_watchdog, relaunch, reopen, swap_back,
    DownloadOutcome,
};
pub(super) use setup_check::{
    awaiting_reopen_snapshot, check, emit_state, get_state, installed_target_awaiting_reopen,
    mark_healthy, marker_dir, resolve_bundle_path, start, UpdaterHandle,
};
pub(super) use startup_recovery::{recover_on_startup, skip_version};

#[cfg(test)]
pub(super) fn handle_in_state(state: UpdaterState) -> UpdaterHandle {
    UpdaterHandle {
        runtime: Mutex::new(Runtime {
            machine: Machine::in_state(state),
            pending: None,
            healthy_confirmed: false,
            pending_cleanup: None,
            pending_check_task: None,
        }),
        recovery_done: AtomicBool::new(true),
    }
}

#[cfg(test)]
mod t3_fix_wiring_tests;

#[cfg(test)]
mod recovery_tests;

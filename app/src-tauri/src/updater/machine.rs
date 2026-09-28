use super::*;
use crate::updater::diag_log::updater_diag;

/// Pure state machine with no `tauri` dependency. It can be constructed, driven, and asserted
/// in ordinary `#[test]` functions without starting a Tauri app, network, or file system.
#[derive(Debug, Clone)]
pub struct Machine {
    pub(super) snapshot: UpdaterSnapshot,
    /// In-memory mirror of the `app_settings` key `updater.skipped_version`. The Tauri shell
    /// loads its initial value from the database in `start()` and persists it in `skip()`.
    skipped_version: Option<String>,
    /// Target version currently moving through the full
    /// Available → Downloading → Staging → Ready pipeline. `Machine` does not know about
    /// `tauri_plugin_updater::Update`; this field records the current version without external
    /// state.
    pending_version: Option<String>,
    /// Download generation, incremented after every successful `begin_download`. The
    /// `on_progress`, `begin_staging`, `on_staged`, and `on_download_error` methods accept a
    /// generation and act only when it matches and the current state is still at the expected
    /// stage. This prevents a late download callback, such as a final poll after a watchdog
    /// timeout, from reviving an obsolete state.
    download_gen: u64,
    /// Before `begin_swap()` moves from `Ready{version, staged_path, ..}` to `Swapping`, it
    /// stores these two fields. `Swapping` is a fieldless unit state, but a failed swap must be
    /// able to restore the original `Ready` state with `last_error` instead of becoming an
    /// unretryable `Error`. Only `begin_swap` sets this value; `begin_recovery_swap`, which
    /// starts from `RecoveryOffered`, clears it so a forward-swap snapshot cannot be reused for
    /// a failed reverse swap.
    swap_ready_snapshot: Option<(String, String)>,
    /// Complete display data saved when `begin_recovery_swap()` moves from `RecoveryOffered`
    /// to `Swapping`. It restores a retryable `RecoveryOffered{last_error}` after a failed
    /// reverse swap.
    recovery_ready_snapshot: Option<(String, String, String)>,
    /// Original state saved when a manual check begins from `Ready`. The check plugin knows
    /// only the currently installed version, so `Ok(None)` does not mean the staged package
    /// should be discarded. Keep `Ready` until the result is finalized, and move to a new
    /// `Available` only when the manifest reports a higher version and old staging cleanup
    /// succeeds.
    ready_check_snapshot: Option<(String, String, Option<String>)>,
}

impl Machine {
    /// With `disabled = Some(reason)`, the initial state is `Disabled` at revision 0 and no
    /// transition is allowed; `can_check` and `begin_download` always reject `Disabled`.
    pub fn new(disabled: Option<DisabledReason>) -> Self {
        let state = match disabled {
            Some(reason) => UpdaterState::Disabled { reason },
            None => UpdaterState::Idle,
        };
        Machine {
            snapshot: UpdaterSnapshot { revision: 0, state },
            skipped_version: None,
            pending_version: None,
            download_gen: 0,
            swap_ready_snapshot: None,
            recovery_ready_snapshot: None,
            ready_check_snapshot: None,
        }
    }

    /// Lets the Tauri shell load the existing database value of `updater.skipped_version`
    /// during `start()`.
    pub fn set_skipped_version(&mut self, version: Option<String>) {
        self.skipped_version = version;
    }

    pub fn snapshot(&self) -> UpdaterSnapshot {
        self.snapshot.clone()
    }

    pub(super) fn bump(&mut self, state: UpdaterState) -> UpdaterSnapshot {
        self.snapshot.revision += 1;
        self.snapshot.state = state;
        self.snapshot.clone()
    }

    /// Concurrency rule: checks can always start from `Idle`, `UpToDate`, or `Error`.
    /// `Available` and `Ready` allow manual checks that refresh the manifest, but reject
    /// automatic checks because an update is already visible. All other states reject checks.
    pub fn can_check(&self, manual: bool) -> bool {
        match &self.snapshot.state {
            UpdaterState::Idle | UpdaterState::UpToDate { .. } | UpdaterState::Error { .. } => true,
            UpdaterState::Available { .. } | UpdaterState::Ready { .. } => manual,
            UpdaterState::Disabled { .. }
            | UpdaterState::Checking
            | UpdaterState::Downloading { .. }
            | UpdaterState::Staging
            | UpdaterState::Swapping
            | UpdaterState::RecoveryOffered { .. } => false,
        }
    }

    /// Attempts to begin a check. On success, transitions to `Checking` and returns the new
    /// snapshot. On rejection, returns `None`, allowing the caller to return the current state
    /// unchanged without an event or revision increment.
    pub fn begin_check(&mut self, manual: bool) -> Option<UpdaterSnapshot> {
        if !self.can_check(manual) {
            return None;
        }
        self.ready_check_snapshot = match &self.snapshot.state {
            UpdaterState::Ready {
                version,
                staged_path,
                last_error,
            } if manual => Some((version.clone(), staged_path.clone(), last_error.clone())),
            _ => None,
        };
        Some(self.bump(UpdaterState::Checking))
    }

    /// Folding rules:
    /// - An automatic `Available` result matching `skipped_version` becomes `UpToDate`; a
    ///   manual check ignores the skipped version so the user can see it.
    /// - Automatic `Error` and `TargetsNotFound` results are logged and return to `Idle` without
    ///   interruption. Manual checks expose `Error`; callers provide the appropriate `al_err`
    ///   key for a manifest missing this platform.
    pub fn on_check_result(&mut self, manual: bool, outcome: CheckOutcome) -> Transition {
        let checked_ready = self.ready_check_snapshot.take();
        match outcome {
            CheckOutcome::UpToDate => {
                self.pending_version = None;
                // An "up to date" result from a manual Ready check only means the server has no
                // manifest entry newer than the installed version. It never authorizes deleting
                // the staged package, so restore Ready. Ordinary checks enter UpToDate.
                match checked_ready {
                    Some((version, staged_path, last_error)) => self.bump(UpdaterState::Ready {
                        version,
                        staged_path,
                        last_error,
                    }),
                    None => self.bump(UpdaterState::UpToDate {
                        checked_at: now_ms(),
                    }),
                }
            }
            CheckOutcome::Available {
                version,
                notes,
                pub_date,
            } => {
                let skipped = !manual && self.skipped_version.as_deref() == Some(version.as_str());
                if let Some((ready_version, staged_path, last_error)) = checked_ready {
                    // `finish_check_with_ready_cleanup` already removed old staging and its
                    // marker for a higher version. Equal, lower, or unparsable manifest versions
                    // conservatively retain Ready so a plugin result based on the installed
                    // version cannot supersede the staged package.
                    if is_version_newer(&version, &ready_version) {
                        self.pending_version = Some(version.clone());
                        self.bump(UpdaterState::Available {
                            version,
                            notes,
                            pub_date,
                        })
                    } else {
                        self.pending_version = None;
                        self.bump(UpdaterState::Ready {
                            version: ready_version,
                            staged_path,
                            last_error,
                        })
                    }
                } else if skipped {
                    self.pending_version = None;
                    self.bump(UpdaterState::UpToDate {
                        checked_at: now_ms(),
                    })
                } else {
                    self.pending_version = Some(version.clone());
                    self.bump(UpdaterState::Available {
                        version,
                        notes,
                        pub_date,
                    })
                }
            }
            CheckOutcome::TargetsNotFound => {
                self.ready_check_snapshot = None;
                if manual {
                    self.bump(UpdaterState::Error {
                        msg: crate::ui_msg::al_err("updater.targets_not_found", &[]),
                        checked_at: now_ms(),
                        retry: ErrorRetry::Check,
                    })
                } else {
                    updater_diag!(
                        "updater: 自动检查发现清单缺本平台（TargetsNotFound），静默回 Idle"
                    );
                    self.bump(UpdaterState::Idle)
                }
            }
            CheckOutcome::Error(msg) => {
                self.ready_check_snapshot = None;
                if manual {
                    self.bump(UpdaterState::Error {
                        msg,
                        checked_at: now_ms(),
                        retry: ErrorRetry::Check,
                    })
                } else {
                    updater_diag!("updater: 自动检查失败（忽略·不打扰用户）：{msg}");
                    self.bump(UpdaterState::Idle)
                }
            }
        }
    }

    /// Single-flight gate: only `Available` can enter `Downloading`. Other states are rejected
    /// with a clone of the current `UpdaterState` and no revision increment, so double clicks or
    /// concurrent requests cannot start a second download. Success allocates a new generation.
    pub fn begin_download(&mut self) -> Result<(UpdaterSnapshot, u64), UpdaterState> {
        match &self.snapshot.state {
            UpdaterState::Available { .. } => {
                self.download_gen += 1;
                let gen = self.download_gen;
                let snap = self.bump(UpdaterState::Downloading {
                    downloaded: 0,
                    total: None,
                });
                Ok((snap, gen))
            }
            other => Err(other.clone()),
        }
    }

    /// Direct rejection after a failed preflight: `Available → Error` without passing through
    /// `Downloading`. This is valid only from `Available`; other states are returned unchanged
    /// without a transition.
    pub fn reject_not_installable(&mut self, msg: String) -> Result<UpdaterSnapshot, UpdaterState> {
        match &self.snapshot.state {
            UpdaterState::Available { .. } => {
                self.pending_version = None;
                Ok(self.bump(UpdaterState::Error {
                    msg,
                    checked_at: now_ms(),
                    retry: ErrorRetry::Check,
                }))
            }
            other => Err(other.clone()),
        }
    }

    /// Download progress callback. A stale generation or a state other than `Downloading`, such
    /// as `Error` after a timeout, is ignored without a transition. This is the second defense
    /// against late progress reviving `Downloading`; cancellation is the first.
    pub fn on_progress(
        &mut self,
        gen: u64,
        downloaded: u64,
        total: Option<u64>,
    ) -> Option<UpdaterSnapshot> {
        if gen != self.download_gen {
            return None;
        }
        if !matches!(self.snapshot.state, UpdaterState::Downloading { .. }) {
            return None;
        }
        Some(self.bump(UpdaterState::Downloading { downloaded, total }))
    }

    /// Called after all `Downloading` bytes arrive and `on_download_finish` fires. Transitions
    /// to `Staging`, which covers unpacking and three validations without fine-grained progress,
    /// and applies the same generation check.
    pub fn begin_staging(&mut self, gen: u64) -> Option<UpdaterSnapshot> {
        if gen != self.download_gen {
            return None;
        }
        if !matches!(self.snapshot.state, UpdaterState::Downloading { .. }) {
            return None;
        }
        Some(self.bump(UpdaterState::Staging))
    }

    pub fn on_staged(
        &mut self,
        gen: u64,
        version: String,
        staged_path: String,
    ) -> Option<UpdaterSnapshot> {
        if gen != self.download_gen {
            return None;
        }
        if !matches!(self.snapshot.state, UpdaterState::Staging) {
            return None;
        }
        self.pending_version = None;
        Some(self.bump(UpdaterState::Ready {
            version,
            staged_path,
            last_error: None,
        }))
    }

    /// Handles failures during download, validation, staging, or marker creation by leaving
    /// `Downloading` or `Staging`, releasing the single-flight guard, and entering `Error`.
    /// Stale generations and other states are ignored. Preflight failures use
    /// `reject_not_installable` because no generation exists before a download starts.
    pub fn on_download_error(&mut self, gen: u64, msg: String) -> Option<UpdaterSnapshot> {
        if gen != self.download_gen {
            return None;
        }
        match self.snapshot.state {
            UpdaterState::Downloading { .. } | UpdaterState::Staging => {
                self.pending_version = None;
                Some(self.bump(UpdaterState::Error {
                    msg,
                    checked_at: now_ms(),
                    retry: ErrorRetry::Check,
                }))
            }
            _ => None,
        }
    }

    /// Skips this version by remembering it for the next automatic check. If the same version is
    /// currently displayed as `Available`, immediately returns to `UpToDate` so the indicator
    /// disappears without waiting for another automatic check.
    pub fn skip(&mut self, version: String) -> UpdaterSnapshot {
        self.skipped_version = Some(version.clone());
        if matches!(&self.snapshot.state, UpdaterState::Available { version: v, .. } if v == &version)
        {
            self.pending_version = None;
            self.bump(UpdaterState::UpToDate {
                checked_at: now_ms(),
            })
        } else {
            self.snapshot.clone()
        }
    }

    pub(super) fn ready_replacement_path(&self, outcome: &CheckOutcome) -> Option<&str> {
        let (ready_version, staged_path, _) = self.ready_check_snapshot.as_ref()?;
        match outcome {
            CheckOutcome::Available { version, .. } if is_version_newer(version, ready_version) => {
                Some(staged_path)
            }
            _ => None,
        }
    }

    pub(super) fn ready_action_failed(&mut self, msg: String) -> UpdaterSnapshot {
        self.ready_check_snapshot = None;
        self.pending_version = None;
        self.bump(UpdaterState::Error {
            msg,
            checked_at: now_ms(),
            retry: ErrorRetry::Check,
        })
    }

    pub(super) fn discard_ready(&mut self) -> Result<UpdaterSnapshot, UpdaterState> {
        if !matches!(self.snapshot.state, UpdaterState::Ready { .. }) {
            return Err(self.snapshot.state.clone());
        }
        self.pending_version = None;
        self.ready_check_snapshot = None;
        Ok(self.bump(UpdaterState::Idle))
    }

    /// Gate for an `updater_relaunch` action: only `Ready` can enter `Swapping`. Other states are
    /// returned unchanged without a transition, following the same single-flight convention as
    /// `begin_download`. The `Ready` version and staged path are saved so `swap_failed` can
    /// restore `Ready` with `last_error` instead of entering an unretryable `Error`.
    pub fn begin_swap(&mut self) -> Result<UpdaterSnapshot, UpdaterState> {
        match &self.snapshot.state {
            UpdaterState::Ready {
                version,
                staged_path,
                ..
            } => {
                self.recovery_ready_snapshot = None;
                self.swap_ready_snapshot = Some((version.clone(), staged_path.clone()));
                Ok(self.bump(UpdaterState::Swapping))
            }
            other => Err(other.clone()),
        }
    }

    /// Gate for an `updater_swap_back` action: only `RecoveryOffered` can enter `Swapping`. It
    /// reuses the same physical swap in the opposite direction and saves the complete
    /// `RecoveryOffered` snapshot so a physical swap failure can roll back and retry.
    pub fn begin_recovery_swap(&mut self) -> Result<UpdaterSnapshot, UpdaterState> {
        match &self.snapshot.state {
            UpdaterState::RecoveryOffered {
                bundle_path,
                staged_path,
                target_version,
                ..
            } => {
                self.swap_ready_snapshot = None;
                self.recovery_ready_snapshot = Some((
                    bundle_path.clone(),
                    staged_path.clone(),
                    target_version.clone(),
                ));
                Ok(self.bump(UpdaterState::Swapping))
            }
            other => Err(other.clone()),
        }
    }

    /// Handles failure of `updater_install::swap` or `swap_back` itself, when the physical swap
    /// definitely did not occur. The marker is normally restored to its pre-failure stage; for
    /// `marker_not_durable`, the pre-swap `Staged` or `Swapped` marker remains the last durable
    /// anchor. A swap started by `begin_swap()` returns to
    /// `Ready{version, staged_path, last_error: Some(msg)}` for retry. One started from
    /// `RecoveryOffered` returns there with `last_error`, also allowing retry.
    ///
    /// Use this only when the swap itself fails. It is intentionally separate from
    /// `relaunch_failed_after_swap`: after a successful swap followed by a launch failure,
    /// returning to retryable `Ready` would invoke `updater_install::swap` again. Because
    /// `RENAME_SWAP` reverses itself, that would silently restore the old version.
    pub fn swap_failed(&mut self, msg: String) -> UpdaterSnapshot {
        if let Some((version, staged_path)) = self.swap_ready_snapshot.take() {
            self.recovery_ready_snapshot = None;
            self.bump(UpdaterState::Ready {
                version,
                staged_path,
                last_error: Some(msg),
            })
        } else if let Some((bundle_path, staged_path, target_version)) =
            self.recovery_ready_snapshot.take()
        {
            self.bump(UpdaterState::RecoveryOffered {
                bundle_path,
                staged_path,
                target_version,
                last_error: Some(msg),
            })
        } else {
            self.bump(UpdaterState::Error {
                msg,
                checked_at: now_ms(),
                retry: ErrorRetry::Check,
            })
        }
    }

    /// Handles a successful swap followed by failure to open the new version, indicated by a
    /// nonzero LaunchServices `open` exit. It must not return to retryable `Ready`, because the
    /// physical swap has already occurred. The user can open the app manually or rely on startup
    /// recovery, so this always enters `Error`. Clear `swap_ready_snapshot` because it describes
    /// the pre-swap state and has no meaning after a successful swap.
    pub fn relaunch_failed_after_swap(&mut self, msg: String) -> UpdaterSnapshot {
        self.swap_ready_snapshot = None;
        self.recovery_ready_snapshot = None;
        self.bump(UpdaterState::Error {
            msg,
            checked_at: now_ms(),
            retry: ErrorRetry::Reopen,
        })
    }

    pub(super) fn is_awaiting_reopen(&self) -> bool {
        matches!(
            self.snapshot.state,
            UpdaterState::Error {
                retry: ErrorRetry::Reopen,
                ..
            }
        )
    }

    /// Startup recovery CAS: applies a recovery result only when the revision observed at the
    /// start of recovery is still current. If a check or another transition wins after a timeout,
    /// the recovery result is discarded.
    pub fn recover_into_if_revision(
        &mut self,
        expected_revision: u64,
        state: UpdaterState,
    ) -> Option<UpdaterSnapshot> {
        if self.snapshot.revision != expected_revision {
            return None;
        }
        Some(self.bump(state))
    }

    #[cfg(test)]
    pub(super) fn pending_version(&self) -> Option<&str> {
        self.pending_version.as_deref()
    }

    /// Test-only constructor that places `Machine` in any state, including states such as
    /// `Swapping` that no public method can otherwise produce, so rejection rules can be tested.
    #[cfg(test)]
    pub(super) fn in_state(state: UpdaterState) -> Machine {
        Machine {
            snapshot: UpdaterSnapshot { revision: 0, state },
            skipped_version: None,
            pending_version: None,
            download_gen: 0,
            swap_ready_snapshot: None,
            recovery_ready_snapshot: None,
            ready_check_snapshot: None,
        }
    }
}

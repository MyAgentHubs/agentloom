export const messages = {
  "updater.button.label": "App update",
  "updater.button.available": "AgentLoom v{version} available",
  "updater.button.downloading": "Downloading update",
  "updater.button.ready": "v{version} ready — click to restart and update",
  "updater.pill.available": "Update to v{version}",
  "updater.pill.downloading.percent": "Downloading {pct}%",
  "updater.pill.downloading.indeterminate": "Downloading…",
  "updater.pill.ready": "Restart to update",
  "updater.pill.failed": "Update failed · Retry",
  "updater.popover.label": "App update",
  "updater.popover.available.title": "AgentLoom v{version} available",
  "updater.popover.notes": "Release notes",
  "updater.popover.downloading.title": "Downloading",
  "updater.popover.downloading.percent": "{pct}%",
  "updater.popover.downloading.indeterminate": "Downloading…",
  "updater.popover.ready.title": "v{version} ready",
  "updater.action.download": "Download & Install",
  "updater.action.skip": "Skip this version",
  "updater.action.later": "Later",
  "updater.action.relaunch": "Restart to update",
  "updater.ready.retryHint":
    "The last update didn't finish — you can try again.",
  "updater.ready.discard": "Discard this update",
  "updater.about.title": "Update",
  "updater.about.currentVersion": "Current v{version}",
  "updater.about.lastChecked": "Last checked {when}",
  "updater.about.neverChecked": "Never checked",
  "updater.about.upToDate": "Up to date",
  "updater.about.checkButton": "Check for updates",
  "updater.about.reopenButton": "Reopen updated app",
  "updater.about.checking": "Checking…",
  "updater.about.availableVersion": "New version v{version} available",
  "updater.about.downloading": "Downloading update",
  "updater.about.staging": "Verifying update",
  "updater.about.readyVersion":
    "v{version} ready — restart to finish installing",
  "updater.about.swapping": "Installing update",
  "updater.about.disabled.dev": "In-app updates aren't available in dev builds",
  "updater.about.disabled.platform":
    "In-app updates aren't available on this platform",
  "updater.about.disabled.unsigned":
    "In-app updates aren't available (unsigned build)",
  "updater.recovery.title": "Update didn't complete · running the backup copy",
  "updater.recovery.body":
    "v{version} failed to start after installing. You can restore this older version to its normal location.",
  "updater.recovery.action": "Restore older version & restart",
  "backend.updater.not_installable":
    "AgentLoom isn't in the Applications folder — move it there before updating",
  "backend.updater.targets_not_found":
    "Update manifest is missing this platform",
  "backend.updater.download_timeout":
    "Download timed out — check your network and try again",
  "backend.updater.check_failed": "Check for updates failed — try again later",
  "backend.updater.stage_failed":
    "Update verification failed — try again later",
  "backend.updater.relaunch_not_ready":
    "Restart-to-install isn't ready yet — try again later",
  "backend.updater.swap_failed": "Version swap failed: {detail}",
  "backend.updater.discard_failed": "Failed to discard update: {detail}",
  "backend.updater.relaunch_failed":
    "Installed, but couldn't relaunch automatically — please reopen AgentLoom manually",
  "backend.updater.reopen_failed":
    "Could not reopen the updated app. Please open AgentLoom manually",
  "backend.updater.relaunch_wrong_state":
    "This action isn't available in the current state — refresh and try again",
} as const;

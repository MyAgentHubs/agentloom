export const messages = {
  "settings.title": "Settings",
  "settings.close": "Close",
  "settings.closeSettings": "Close settings",
  "settings.version": "Version",
  "settings.nav.agents": "Agent Pool",
  "settings.nav.search": "Web Search",
  "settings.nav.repos": "Repositories",
  "settings.nav.archivedProjects": "Archived projects",
  "settings.nav.language": "Language & Region",
  "settings.nav.chat": "Chat",
  "settings.nav.remoteControl": "Remote Control",
  "settings.nav.defaults": "Defaults & Modes",
  "settings.nav.allowlist": "Namespace Allowlist",
  "settings.nav.accounts": "Accounts & Git",
  "settings.nav.budget": "Cost & Budget",
  "settings.nav.shortcuts": "Keyboard Shortcuts",
  "settings.nav.about": "About",
  "settings.nav.general": "General",
  "settings.general.title": "General",
  "settings.general.subtitle":
    "Manage automatic session archiving and permanent cleanup.",
  "settings.general.groupLabel": "Session lifecycle",
  "settings.general.enableLabel": "Enable automatic session lifecycle",
  "settings.general.enableDesc":
    "When enabled, sessions are archived and cleaned up using the rules below. Off by default.",
  "settings.general.enabled": "Enabled",
  "settings.general.disabled": "Disabled",
  "settings.general.archiveLabel": "Auto-archive inactive sessions",
  "settings.general.archiveDesc":
    "Archive sessions after they have been inactive for this many days.",
  "settings.general.purgeLabel": "Permanently delete archived sessions",
  "settings.general.purgeDesc":
    "Permanently delete archived sessions and clean up their persistent resources after this many days. This cannot be undone.",
  "settings.general.protectionHint":
    "Archives that existed before lifecycle automation was first enabled are never permanently deleted automatically; delete them manually if needed.",
  "settings.general.days": "days",
  "settings.general.minHint": "Minimum 1 day",
  "settings.about.support": "Support",
  "settings.about.feedback": "Feedback",
  "settings.about.website": "Website",
  "aboutDialog.close": "Close",
  "aboutDialog.copyVersion": "Copy version",
  "aboutDialog.copied": "Copied",
  "aboutDialog.website": "Website",
  "aboutDialog.feedback": "Feedback",
  "aboutDialog.support": "Support",
  "aboutDialog.copyright": "© 2026 MyAgentHubs",
  "archivedProjects.empty": "No archived projects",
  "archivedProjects.restore": "Restore",
  "archivedProjects.deleteForever": "Delete forever",
  "archivedProjects.deleteConfirm.title": "Delete project forever?",
  "archivedProjects.deleteConfirm.body":
    'Permanently delete "{name}" and all its sessions (irreversible). Code on disk will not be deleted.',
  "archivedProjects.deleteConfirm.confirm": "Delete forever",
  "archivedProjects.deleteConfirm.cancel": "Cancel",
  "backend.project.cannotDeleteDefault":
    "The default project cannot be deleted.",
  "settings.remoteControl.title": "Remote Control",
  "settings.remoteControl.intro":
    "Pair your phone with AgentLoom on this computer through a secure relay server.",
  "settings.remoteControl.enabledLabel": "Allow remote control from a phone",
  "settings.remoteControl.enabled": "On",
  "settings.remoteControl.disabled": "Off",
  "settings.remoteControl.stopped.roomClaimConflict":
    "Another desktop owns this room. To protect paired devices, AgentLoom did not switch rooms automatically.",
  "settings.remoteControl.stopped.roomTombstoned":
    "This room was terminated by the server (410). Pair your phone again.",
  "settings.remoteControl.stopped.roomDeviceStatusUnavailable":
    "Paired-device status could not be verified. Remote control stopped to protect existing devices.",
  "settings.remoteControl.stopped.registryRebaseLimit":
    "The relay registry high-water mark kept advancing. Remote control stopped; change the settings or pair again.",
  "settings.remoteControl.stopped.unknown":
    "Remote control stopped (code: {code}). Change the settings or pair again to recover.",
  "settings.remoteControl.stopped.roomClaimConflictProject":
    "Another desktop claimed the room for the current active project. Per-project rooms can't switch automatically, so this machine stepped down — re-pair this project from Settings.",
  "settings.remoteControl.relayLabel": "Relay server URL",
  "settings.remoteControl.relayHint":
    "Leave empty to use the official MyAgentHubs relay (default). Enter a wss:// URL to use your own self-hosted relay — the relay is open source.",
  "settings.remoteControl.activeProjectLabel": "Active project",
  "settings.remoteControl.activeProjectUnset": "Not set",
  "settings.remoteControl.activeProjectHint":
    "Pairing and devices belong to the room of the current active project. Choose a project before you start pairing.",
  "settings.remoteControl.activeProjectSwitchHint":
    "Switching the active project disconnects any already-paired phones. If you switch back to the original project, the existing pairing usually still works — only generate a new QR code below and re-pair if it doesn't reconnect.",
  "settings.remoteControl.currentServingLabel": "Currently serving",
  "settings.remoteControl.projectMismatch":
    "Remote control is serving “{serving}”, but you're currently in “{current}”.",
  "settings.remoteControl.projectMismatchSwitch": "Switch to current project",
  "settings.remoteControl.switchNotice":
    "The served project changed, so the room changed too — your phone needs to scan a new QR code to reconnect.",
  "settings.remoteControl.switchNoticeClose": "Got it",
  "settings.remoteControl.pairingTitle": "Pair a phone",
  "settings.remoteControl.pairingIntro":
    "Scan the QR code with your phone to pair it with this computer.",
  "settings.remoteControl.generate": "Generate pairing QR code",
  "settings.remoteControl.generating": "Generating…",
  "settings.remoteControl.validity": "Valid for 5 minutes",
  "settings.remoteControl.cancel": "Cancel",
  "settings.remoteControl.cancelling": "Cancelling…",
  "settings.remoteControl.waiting": "Waiting for your phone to scan…",
  "settings.remoteControl.done": "Paired device {deviceId}",
  "settings.remoteControl.copyPairingString": "Copy pairing string",
  "settings.remoteControl.copyPairingStringCopied": "Copied",
  "settings.remoteControl.qrEncodeFailed":
    "Could not generate the pairing QR code: the pairing data is too long or the relay URL is invalid. Check the settings and try again.",
  "settings.remoteControl.devicesTitle": "Paired devices",
  "settings.remoteControl.devicesIntro":
    "Manage phones that can still access this computer.",
  "settings.remoteControl.devicesActiveProjectHint":
    "Choose an active project to see the devices paired with it.",
  "settings.remoteControl.devicesEmpty": "No paired devices yet",
  "settings.remoteControl.createdAt": "Paired {date}",
  "settings.remoteControl.expiresAt": "Access expires: {date}",
  "settings.remoteControl.revoke": "Revoke",
  "settings.remoteControl.revoking": "Revoking…",
  "settings.remoteControl.revokeConfirm.title": "Revoke device access?",
  "settings.remoteControl.revokeConfirm.body":
    'After revoking "{name}", this device can no longer control AgentLoom remotely.',
  "settings.remoteControl.revokeConfirm.consequence":
    "The phone will disconnect immediately and must scan a new QR code to connect again.",
  "settings.remoteControl.revokeConfirm.confirm": "Revoke access",
  "settings.remoteControl.revokeConfirm.cancel": "Cancel",
  "settings.remoteControl.diagnostics.title": "Diagnostics",
  "settings.remoteControl.diagnostics.description":
    "These raw counters refresh every 3 seconds and help locate where remote session events are lost.",
  "settings.remoteControl.diagnostics.empty": "No diagnostics available",
  "settings.search.intro":
    "Not every model ships with built-in web search. AgentLoom connects third-party search services so any agent can search the web. DuckDuckGo works out of the box — no key needed; add a Brave or Exa API key for higher-quality results.",
  "settings.search.formAriaLabel": "Search service settings",
  "settings.search.serviceLabel": "Search service",
  "settings.search.ddgNote": "DuckDuckGo needs no API key.",
  "settings.search.useThisButton": "Use this service",
  "settings.search.useThisSwitching": "Switching…",
  "settings.search.useThisSwitched": "Switched to DuckDuckGo.",
  "settings.search.useThisError": "Failed to switch, please try again",
  "settings.search.apiKeyLabel": "API Key",
  "settings.search.testButton": "Test connection",
  "settings.search.testingButton": "Testing…",
  "settings.search.saveButton": "Save",
  "settings.search.savingButton": "Saving…",
  "settings.search.saveNote":
    "Saving stores the key in the system keychain and makes this service the active one.",
  "settings.search.saved": "Saved",
  "settings.search.saveError": "Save failed, please try again",
  "settings.search.registerLink": "Register a key at {label}",
  "settings.search.placeholderConfigured":
    "Configured · paste a new key to replace it",
  "settings.search.placeholderEmpty": "Paste your {apiName} key",
  "settings.search.searxngComingSoon": "SearXNG (coming soon)",
  "settings.search.category.ok": "Connected",
  "settings.search.category.auth": "Invalid or unauthorized key",
  "settings.search.category.rateLimit": "Rate limited · try again later",
  "settings.search.category.network": "Network error",
  "settings.search.category.missingKey": "Enter a key first",
  "settings.search.status.unknown": "Not checked",
  "settings.search.status.checking": "Checking…",
  "settings.search.status.configured": "Configured",
  "settings.search.status.missing": "Missing",
  "settings.search.checkButton": "Check",
  "settings.language.title": "Language & Region",
  "settings.language.subtitle":
    "Choose the language for the AgentLoom interface.",
  "settings.language.current": "Current language",
  "settings.language.zh": "中文",
  "settings.language.en": "English",
  "settings.chat.title": "Chat",
  "settings.chat.subtitle":
    "How much process detail shows in the chat stream — affects this device only.",
  "settings.chat.groupLabel": "Process detail",
  "settings.chat.full.title": "Full",
  "settings.chat.full.desc":
    "Show user-facing tool activity, command summaries, and thinking (collapsible)",
  "settings.chat.summary.title": "Summary",
  "settings.chat.summary.desc":
    "Fold the process into one activity summary; expand to see each item",
  "settings.chat.minimal.title": "Minimal",
  "settings.chat.minimal.desc":
    "Show only the conversation and cards needing your input; in-progress work still shows the current action",
  "settings.agentAccess.borrow": "Via Claude Code",
  "settings.agentAccess.harness": "Built-in engine",
  "settings.agentAccess.native": "Native CLI",
  "settings.agentKeyState.configured": "Configured ✓",
  "settings.agentKeyState.detected": "Detected",
  "settings.agentKeyState.missing": "Missing",
  "settings.agentKeyState.notInstalled": "Not installed",
  "settings.agents.configuredCount": "{n} agents configured",
  "settings.agents.description":
    "Add an AI worker: choose an engine, pick a model, paste a key.",
  "settings.agents.add": "＋ Add agent",
  "settings.agents.empty": "No agents yet",
  "settings.agents.listAria": "Agent pool list",
  "settings.agents.providerModel.unset": "Provider/model not set",
  "settings.agents.nativeAutoDetectTitle":
    "Uses the local CLI automatically · no setup needed",
  "settings.agents.nativeAutoDetect": "Auto-detected",
  "settings.agents.edit": "Edit",
  "settings.agents.delete": "Delete",
  "settings.agents.deleteAria": "Delete {name}",
  "onboarding.installGuide.title": "No agents are available yet",
  "onboarding.installGuide.reason":
    "AgentLoom can run agents with its built-in engine, myagent — all it needs is your own API key — or drive Claude Code or the Codex CLI on this computer. None of these is set up yet.",
  "onboarding.installGuide.harnessDescription":
    "Run agents with your own API key. No vendor CLI to install.",
  "onboarding.installGuide.configureHarness": "Set up",
  "onboarding.installGuide.claudeDescription":
    "Run Claude agents with your Anthropic account.",
  "onboarding.installGuide.codexDescription":
    "Run Codex agents with your OpenAI account.",
  "onboarding.installGuide.openInstallGuide": "Open installation guide",
  "onboarding.installGuide.openSettings": "Open Agent settings",
  "onboarding.installGuide.dismiss": "Maybe later",
  "settings.agentForm.category.auth": "Key is invalid or unauthorized",
  "settings.agentForm.category.rateLimit":
    "Quota exhausted or rate limited (not an endpoint issue)",
  "settings.agentForm.category.network":
    "Cannot reach the endpoint. Check network/address.",
  "settings.agentForm.category.notFound": "Endpoint or model not found",
  "settings.agentForm.category.missingKey": "Paste an API Key before testing",
  "settings.agentForm.category.endpointRequired": "Endpoint is required",
  "settings.agentForm.category.other": "Request failed",
  "settings.agentForm.group.account": "Account",
  "settings.agentForm.saveWarning.nativeMissing":
    "{cli} CLI not detected — you can still save, but this agent will not run until it is installed.",
  "settings.agentForm.saveWarning.nativeOverrideInvalid":
    "The specified {cli} CLI path cannot be used — you can still save, but this agent will not run until the path is fixed.",
  "settings.agentForm.saveBlocked.testFailed":
    "Connection test has not passed, so this cannot be saved yet",
  "settings.agentForm.engineStatus.builtIn": "✓ Built in · no install",
  "settings.agentForm.engineStatus.installedLoggedIn":
    "✓ Installed · signed in",
  "settings.agentForm.engineStatus.installed": "✓ Installed",
  "settings.agentForm.engineStatus.notDetected": "⚠ Not detected ·",
  "settings.agentForm.engineStatus.installGuide": "Install guide",
  "settings.agentForm.engineStatus.installGuideAria": "{engine} install guide",
  "settings.agentForm.nativeStatus.loggedIn":
    "✓ {cli} CLI detected · signed in with your {account} account, no API Key needed",
  "settings.agentForm.nativeStatus.installedNoCredsPrefix":
    "⚠ {cli} CLI is installed, but no login credentials were detected. You can save now; if it fails at runtime, run",
  "settings.agentForm.nativeStatus.installedNoCredsSuffix":
    "in a terminal, then",
  "settings.agentForm.nativeStatus.notDetected": "⚠ {cli} CLI not detected",
  "settings.agentForm.nativeStatus.pathSpecified": "✓ Path specified",
  "settings.agentForm.nativeStatus.specifiedPathInvalid":
    "⚠ The {cli} CLI path you specified cannot be used",
  "settings.agentForm.nativeStatus.choosePath": "Specify path…",
  "settings.agentForm.nativeStatus.clearPath": "Clear",
  "settings.agentForm.nativeStatus.notDetectedHelp.claude":
    "AgentLoom needs the Claude Code command-line tool — not the Claude desktop app. If you just installed it, restarting AgentLoom can help; otherwise, view the installation guide.",
  "settings.agentForm.nativeStatus.notDetectedHelp.codex":
    "If you just installed Codex CLI, restarting AgentLoom can help; otherwise, view the installation guide.",
  "settings.agentForm.nativeStatus.recheck": "Recheck",
  "settings.agentForm.nativeStatus.viewInstallGuide": "View install guide",
  "settings.agentForm.moreSummary.borrow":
    "Model · reasoning · endpoint · auth · model mapping · timeout · compatibility",
  "settings.agentForm.moreSummary.harness":
    "Model · reasoning · endpoint · timeout",
  "settings.agentForm.moreSummary.native": "Model · reasoning",
  "settings.agentForm.modelLabel": "Model",
  "settings.agentForm.primaryModelLabel": "Primary model",
  "settings.agentForm.harnessModelPlaceholder": "Blank = myagent default",
  "settings.agentForm.harnessDefaultModelOption": "myagent default",
  "settings.agentForm.fromList": "↩ Choose from list",
  "settings.agentForm.unknownModelWarning":
    "Unrecognized model id — double-check the spelling (e.g. claude-fable-5). Saving is allowed, but the agent may fail to start.",
  "settings.agentForm.modelPlaceholder.cliDefault": "CLI default",
  "settings.agentForm.modelPlaceholder.select": "Select a model",
  "settings.agentForm.reasoningLabel": "reasoning default",
  "settings.agentForm.reasoningDisabledHint": "reasoning tiers are disabled",
  "settings.agentForm.authLabel": "Auth mode",
  "settings.agentForm.autoMark": "· Auto",
  "settings.agentForm.modelMappingLabel": "Model mapping",
  "settings.agentForm.modelMappingHint":
    "Important: Claude Code uses the haiku tier for background work/subagent tasks. If this endpoint does not have claude-haiku, map it to that provider's small model or background jobs may return 400/404.",
  "settings.agentForm.maxOutputTokensPlaceholder":
    "Default: follow the model limit (recommended)",
  "settings.agentForm.compatLabel":
    "Compatibility switches (Claude Code routing tweaks · usually leave unchanged)",
  "settings.agentForm.compatDisableThinking": "Disable thinking",
  "settings.agentForm.compatDisableBetas": "Disable betas",
  "settings.agentForm.compatDisableNonessential":
    "Disable nonessential traffic",
  "settings.agentForm.compatProxyPlaceholder": "e.g. thinking_passback",
  "settings.agentForm.formAria": "Add / edit agent",
  "settings.agentForm.title.add": "Add agent",
  "settings.agentForm.title.edit": "Edit agent",
  "settings.agentForm.borrowIntro":
    "Use native mode when you have the CLI (claude/codex). Other providers route through Claude Code automatically; just paste a key.",
  "settings.agentForm.basic": "Basic",
  "settings.agentForm.engineLabel": "Engine",
  "settings.agentForm.engineDesc.claudeCode":
    "Local claude command. Runs Anthropic models directly or routes other providers.",
  "settings.agentForm.engineDesc.codex":
    "Local codex command. Runs OpenAI models directly.",
  "settings.agentForm.engineDesc.myagent":
    "Custom harness that connects directly to provider APIs",
  "settings.agentForm.presetLabel.custom": "Custom",
  "settings.agentForm.providerUpcoming": "Proxy mode · later version",
  "settings.agentForm.accountChip": "{account} account",
  "settings.agentForm.cliLoggedIn": "CLI signed in",
  "settings.agentForm.accessPointLabel": "Access point",
  "settings.agentForm.accessPoint.default": "Default",
  "settings.agentForm.accessPoint.cn": "China",
  "settings.agentForm.accessPoint.intl": "International",
  "settings.agentForm.accessPoint.cn-coding": "China · Coding plan",
  "settings.agentForm.accessPoint.intl-coding": "International · Coding plan",
  "settings.agentForm.borrowPresetHint":
    "After you choose a Provider preset, model mapping / auth mode / timeout / compatibility switches are filled in automatically; providers that need a key only need an API Key.",
  "settings.agentForm.harnessHint":
    "myagent connects directly to this provider (OpenAI compatible). Pick a model (or leave it blank for the default) and paste a key; endpoint is built in, with no model mapping/auth mode needed.",
  "settings.agentForm.nameLabel": "Name",
  "settings.agentForm.apiKeyHint":
    "Stored in the local keychain · not uploaded · this machine only",
  "settings.agentForm.existingKeyPlaceholder":
    "Configured · leave blank to keep the existing key",
  "settings.agentForm.showApiKey": "Show API Key",
  "settings.agentForm.hideApiKey": "Hide API Key",
  "settings.agentForm.testing": "Testing…",
  "settings.agentForm.testConnection": "Test connection",
  "settings.agentForm.keyStatusPrefix": "Key status: ",
  "settings.agentForm.keepStoredKeyHint":
    "; leave blank to keep the stored key",
  "settings.agentForm.borrowKeyMissing":
    "No key configured · this agent is unavailable for now",
  "settings.agentForm.multiAp.keyHint":
    "{accessPoints} keys are not interchangeable · use a key from {keyHint}",
  "settings.agentForm.multiAp.noKeyHint":
    "{accessPoints} keys are not interchangeable · use a key for the current access point",
  "settings.agentForm.testSuccess": "Connection successful",
  "settings.agentForm.testSuccessFetchedHarness": " · fetched {n} models",
  "settings.agentForm.testSuccessFetchedBorrow": " · fetched {n} models",
  "settings.agentForm.rawErrorToggle": "Show raw error",
  "settings.agentForm.moreOptions": "More options",
  "settings.agentForm.runModeLabel": "Runtime",
  "settings.agentForm.runModeHint":
    "Chosen automatically by the provider preset; no manual selection needed",
  "settings.agentForm.cancel": "Cancel",
  "settings.agentForm.save": "Save",
  "settings.agentForm.add": "Add",
  "settings.agentForm.error.nameRequired": "Name is required",
  "settings.agentForm.error.primaryModelRequired": "Primary model is required",
  "settings.agentForm.error.endpointRequired":
    "This agent requires an Endpoint",
  "settings.agentForm.error.saveFailed": "Save failed, please try again later",
  "settings.modelDropdown.custom": "Custom… (type a model name)",
  "settings.modelDropdown.placeholder": "Select a model",
  "settings.modelDropdown.live": "Live",
} as const;

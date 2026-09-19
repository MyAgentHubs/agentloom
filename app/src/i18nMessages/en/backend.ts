export const messages = {
  "backend.ui.badLocale": "Invalid UI locale",
  "backend.session.idContainsPipe":
    "Session id must not contain a pipe character (|): {id}",
  "backend.agent.missingApiKey": "API key is required",
  "backend.agent.unknownAccess": "Unknown agent access mode: {access}",
  "backend.agent.notFound": "Unknown agent",
  "backend.agent.missingId": "agent_id is required",
  "backend.agent.invalidReasoningTier": "Invalid reasoning_tier: {tier}",
  "backend.agent.nativeAccessImmutable":
    "A native agent's access mode cannot be changed",
  "backend.agent.nativeKeyUnsupported":
    "API keys cannot be configured for native agents",
  "backend.agent.keychainSaveFailed":
    "Could not save the API key to the system keychain, so the key was not applied. Try again or check the system keychain permissions. (Details: {detail})",
  "backend.agent.keychainKeyUnavailable": "{detail}",
  "backend.agent.sessionRunUnknown":
    "This session has no run history, so its agent cannot be determined",
  "backend.agent.idNotFound": "Agent {id} does not exist",
  "backend.agent.emptyFilteredId": "The agent id is empty after filtering",
  "backend.agent.unknownEngine": "Unknown engine: {engine}",
  "backend.agent.configDirCreateFailed":
    "Failed to create the configuration directory: {detail}",
  "backend.agent.promptFileDirCreateFailed":
    "Failed to create the prompt temporary directory: {detail}",
  "backend.agent.promptFileCreateFailed":
    "Failed to create the prompt temporary file: {detail}",
  "backend.agent.promptFileWriteFailed":
    "Failed to write the prompt temporary file: {detail}",
  "backend.agent.pastedDirCreateFailed":
    "Failed to create the pasted attachment directory: {detail}",
  "backend.agent.missingEndpoint": "Agent {id} is missing an endpoint",
  "backend.cliPath.invalidCli":
    "Cannot set the CLI path: {cli} is not supported.",
  "backend.cliPath.invalidPath":
    "The selected path cannot be used: {path}. Choose the CLI executable file. On Windows, choose an .exe, .cmd, or .bat file.",
  "backend.cliPath.databaseUnavailable":
    "The setting was not saved. Try again. Details: {detail}",
  "backend.remoteControl.invalidRelayUrl": "Relay URL must start with wss://",
  "backend.remoteControl.activeProjectMissing":
    "Could not find that project (id: {repoId}); it cannot be set as the active project",
  "backend.remoteControl.pairingNeedsActiveProject":
    "Pairing can't start without an active project. Choose one in remote control settings first.",
  "backend.member.notInSessionPool":
    "Agent {id} is not in this session's member pool",
  "backend.member.unavailableMissing":
    "Agent {id} is unavailable because it does not exist",
  "backend.member.unavailableDisabled":
    "Agent {id} is unavailable because it is disabled",
  "backend.member.emptyTeam": "A team run requires at least one member",
  "backend.member.spawnFailed": "Failed to start the member: {detail}",
  "backend.member.noResult":
    "run_single_worker: the worker did not produce a MemberResult",
  "backend.gh.gitSpawnFailed": "Failed to start Git: {detail}",
  "backend.mcp.noPort": "Unable to obtain the listening port",
  "backend.proxy.noPort": "The proxy could not resolve its listening port",
  "backend.criteria.lineTooLong":
    "An acceptance criterion exceeds the {max}-character limit",
  "backend.criteria.invalidSyntax":
    "Invalid acceptance criterion: {raw} (use cmd:/contains:<s>:/judge:)",
  "backend.criteria.tooMany":
    "There are too many acceptance criteria (maximum {max})",
  "backend.file.markdownOnly": "Only .md/.markdown files are allowed",
  "backend.file.parentMissing": "The parent directory does not exist",
  "backend.file.pathOutOfBounds": "The path is outside the project",
  "backend.file.notFound": "The file does not exist",
  "backend.file.openFilesOnly": "Only files can be opened",
  "backend.file.tooLarge":
    "The file is {size} bytes; text preview supports up to {max} bytes",
  "backend.file.binaryPreviewUnsupported":
    "Binary file preview is not supported yet",
  "backend.file.htmlOnly": "Only .html/.htm files can be opened in a browser",
  "backend.file.ambiguousBasename":
    "Multiple files have this name. Use a more complete path: {0} → {1}",
  "backend.file.basenameBudget":
    'Too many files to search for "{0}" — please provide a fuller path',
  "backend.file.openExternalFailed":
    "Could not open the file in the system browser: {detail}",
  "backend.file.repoLookupFailed": "Failed to look up the project: {detail}",
  "backend.file.repoNotFound": "The project does not exist",
  "backend.repo.namespaceLookupFailed":
    "Failed to look up the namespace: {detail}",
  "backend.repo.lookupFailed": "Failed to look up the repository: {detail}",
  "backend.repo.activeReposLookupFailed":
    "Failed to list active repositories: {detail}",
  "backend.repo.duplicateLookupFailed":
    "Failed to check for an existing repository: {detail}",
  "backend.repo.setLastActiveFailed":
    "Failed to set the last active repository: {detail}",
  "backend.repo.ensureNamespaceFailed":
    "Failed to ensure the namespace exists: {detail}",
  "backend.repo.insertRepoFailed": "Failed to add the repository: {detail}",
  "backend.repo.namespaceMismatch":
    "Repository {repoId} belongs to {actualNamespaceId}, not {namespaceId}",
  "backend.repo.pathNotFound": "The path does not exist: {path}",
  "backend.repo.pathNotDirectory": "The path is not a directory: {path}",
  "backend.repo.pathInsideAppDomain":
    "Projects inside AgentLoom's own data directory (~/.agentloom) cannot be added: {path}. Move the project outside that directory and try again.",
  "backend.repo.insertFailed": "Failed to add the repository: {detail}",
  "backend.project.notFound": "This project does not exist",
  "backend.project.pathRequired": "Choose a new working directory",
  "backend.project.canonicalizeFailed": "Could not resolve the path: {detail}",
  "backend.project.pathNotWritable":
    "This directory is not writable, please check permissions: {detail}",
  "backend.project.pathAlreadyRegistered":
    'This directory is already the working directory of project "{name}"; it can\'t be linked twice: {path}',
  "backend.project.pathNestsAnotherProject":
    "This directory and project \"{name}\"'s working directory are nested in each other; projects can't overlap: {path}",
  "backend.project.githubPathNotGitRepo":
    "This is not a git repository (no .git found): {path}. A GitHub project's working directory must be a cloned git repository.",
  "backend.project.invalidPath":
    "The path contains characters that can't be handled",
  "backend.project.pathUpdateFailed":
    "Failed to update the project path: {detail}",
  "backend.project.databaseUnavailable":
    "The database is temporarily unavailable, please retry: {detail}",
  "backend.landing.protectedPath":
    "Pre-landing check failed: protected paths {paths}",
  "backend.landing.noEvidence":
    "Pre-landing check failed: worker changed_files evidence not found",
  "backend.landing.scopeExceeded":
    "Pre-landing check failed: changes exceed the worker declaration {files}",
  "backend.landing.l1NotGreen":
    "L1 is not green (no passed re-verification, or the evidence SHA does not match the current commit) · Merge blocked · See spec L4",
  "backend.merge.stagingConflict":
    "Changes conflict with staging · Rejected (conflicts are not resolved automatically)",
  "backend.finalize.noChanges":
    "The worker made no changes · Nothing to finalize",
  "backend.finalize.gitUnavailable":
    "This project is not a Git repository; the agent's changes remain in place, but Git relay is unavailable",
  "backend.finalize.uncommittedChanges":
    "The worker left uncommitted changes; the app will not commit them, and they remain in the working directory",
  "backend.artifact.notReadyVerify":
    "Artifact is not ready (no commit_sha) · Cannot verify",
  "backend.artifact.noShaPreflight":
    "Artifact has no commit_sha · Cannot run pre-landing checks",
  "backend.artifact.notReadyMerge":
    "Artifact is not ready (state={state}) · Cannot merge",
  "backend.artifact.noShaMerge": "Artifact has no commit_sha · Cannot merge",
  "backend.artifact.notFound": "Artifact does not exist: {id}",
  "backend.run.repoNotFound": "Repository {id} does not exist",
  "backend.run.preHeadReadFailed": "Failed to read pre_head: {detail}",
  "backend.run.ledgerPendingWriteFailed":
    "Failed to write the pending ledger entry: {detail}",
  "backend.run.spawnFailed": "Failed to start the run: {detail}",
  "backend.run.teamMembersActive": "Cannot start a new run: {detail}",
  "backend.run.stdoutUnavailable": "Unable to read run output",
  "backend.run.workspaceCanonicalizeFailed":
    "Failed to canonicalize the workspace: {detail}",
  "backend.run.unknownLeadAgent": "Unknown lead agent: {id}",
  "backend.run.unknownLeadAgentGeneric": "Unknown lead agent",
  "backend.run.tombstoneRestoreFailed":
    "Failed to tombstone the session and restore its branch (database/Git divergence; reconciliation required): tombstone={tombstone}; restore={restore}",
  "backend.run.invalidSessionId": "Invalid session_id",
  "backend.run.inplaceDeliveryUncommitted":
    "In-place changes are not committed yet: {count} file(s) remain in the working tree ({files}). Commit them in the project first, then push / create a PR / publish.",
  "backend.delivery.confirmationRequired":
    "{operation} requires explicit confirmation for this attempt; no remote operation was performed",
  "backend.publish.pushed": "Pushed to origin/{branch}",
  "backend.publish.failed.boundRepo":
    "PUBLISH_FAILED:This session is linked to a GitHub repository; use push/PR instead of publish",
  "backend.publish.needsAccount.missing":
    "PUBLISH_NEEDS_ACCOUNT:No signed-in gh account was detected (run gh auth login)",
  "backend.publish.needsAccount.multiple":
    "PUBLISH_NEEDS_ACCOUNT:Multiple gh accounts were detected; choose an identity ({list})",
  "backend.publish.failed": "PUBLISH_FAILED:{detail}",
  "backend.publish.failed.missingRepoName":
    "PUBLISH_FAILED:repo_name is required (no goal_title fallback is available)",
  "backend.continuation.invalidParentSessionId":
    "The parent session_id is empty after sanitization; cannot create a continuation child session",
  "backend.continuation.childSessionIdUnavailable":
    "Unable to generate a unique continuation child session id",
  "backend.continuation.startCleanupFailed":
    "{original}; cleanup errors: {errors}",
  "backend.continuation.handoffRequired":
    "The continuation launch instructions (handoff document) cannot be empty",
  "backend.continuation.handoffTimedOut":
    "Continuation draft generation timed out. Please retry; the parent session is available again.",
  "backend.continuation.invalidSessionId":
    "The session_id is empty after sanitization; cannot assemble the continuation handoff",
  "backend.continuation.localSessionUnsupported":
    "Local sessions do not support continuation yet (this feature is not yet available for local sessions)",
  "backend.lead.claudeOnlyContinuation":
    "Team continuation currently supports only native Claude sessions (non-Claude lead support is in progress; use Solo for now)",
  "backend.apply.repoDetached":
    "The current repository has a detached HEAD and is not on a branch, so changes were not applied (check out a branch first)",
  "backend.apply.repoDirty":
    "The current repository worktree has uncommitted changes; commit or stash them before applying",
  "backend.apply.branchAdvanced":
    "The current branch cannot fast-forward to staging (it may have advanced or diverged; v1 will not force-push): the current branch has advanced",
  "backend.apply.fastForwardFailed":
    "The current branch cannot fast-forward to staging (it may have advanced or diverged; v1 will not force-push): {detail}",
  "backend.wt.verifier.unsupportedPlatform":
    "The verifier sandbox is unavailable on this platform. The MVP supports macOS only; Linux sandboxing is deferred.",
  "backend.wt.verifier.writeAttempt":
    "The verifier attempted to modify files. The attempt was rejected; use dispatch_worker for file changes.",
  "backend.wt.verifier.canonicalizeFailed":
    "Could not canonicalize the session worktree / HOME path; the verifier refuses to run in-place without sandbox guardrails: {detail}",
  "backend.wt.sessionMerge.artifactBaseMismatch":
    "Artifact {artifact} is not based on base_sha {base}; merge rejected",
  "backend.wt.sessionMerge.stagingBaseMismatch":
    "Staging branch {staging} is not based on base_sha {base}; merge rejected",
  "backend.wt.sessionMerge.outsideAppDomain":
    "Stage 1 merge rejected: session_wt {path} is outside the app domain (~/.agentloom) · fail-closed",
  "backend.wt.sessionMerge.invalidHead":
    "Stage 1 merge rejected: the session worktree HEAD is not attached to agentloom/* (detached or on a non-agentloom branch) · fail-closed",
  "backend.wt.sessionMerge.memberMissing":
    "Stage 1 merge rejected: member branch {member} does not exist",
  "backend.wt.sessionMerge.dirtyWorktree":
    "Stage 1 merge rejected: the session worktree has uncommitted changes · fail-closed",
  "backend.wt.sessionMerge.stagingBranchMissing":
    "Staging branch does not exist: {staging}",
  "backend.wt.cleanup.commitOutsideAppDomain":
    "commit_dirty rejected: {path} is outside the app domain · fail-closed",
  "backend.wt.cleanup.commitInvalidHead":
    "commit_dirty rejected: {path} HEAD is not attached to agentloom/* · fail-closed",
  "backend.wt.cleanup.sessionWorktreeReleased":
    "Cleanup rejected: the session worktree was released while {pending} member branches still need merging · fail-closed (recreate the session worktree and retry, or reconcile)",
  "backend.wt.cleanup.invalidMemberRef":
    "Cleanup rejected: member ref {member} has an invalid format · fail-closed",
  "backend.wt.cleanup.memberWorktreeDetached":
    "Cleanup rejected: member worktree {path} is not attached to {member} (detached or invalid state) · fail-closed · reconcile required",
  "backend.wt.cleanup.notFastForward":
    "Cleanup rejected: member {member} cannot be fast-forwarded (stale base or parallel changes) · fail-closed",
  "backend.wt.cleanup.registrationIncomplete":
    "Release/trash rejected: worktree {path} is still registered · fail-closed",
  "backend.wt.cleanup.trashRefExists":
    "Trash rejected: {trash} already exists; refusing to overwrite the previous recovery copy · fail-closed",
  "backend.wt.restore.headsRefExists":
    "Restore rejected: {heads} already exists; refusing to overwrite the live branch · fail-closed",
  "backend.wt.restore.refsMissing":
    "Restore rejected: neither the trash nor heads ref exists for session {session}; nothing can be restored · fail-closed",
  "backend.wt.restore.compensationTrashExists":
    "Restore compensation rejected: {trash} already exists; refusing to overwrite the trash ref · fail-closed",
  "backend.wt.restore.compensationHeadsMissing":
    "Restore compensation rejected: {heads} does not exist and cannot be moved back to trash · fail-closed",
  "backend.wt.gc.liveWorktree":
    "GC rejected: session {session} still has a live registered worktree · fail-closed",
  "backend.wt.gc.liveHeads":
    "GC rejected: session {session} still has a live heads branch; its base is the diff fork point · fail-closed",
  "backend.wt.session.gitStatusSpawnFailed":
    "Failed to start git status: {detail}",
  "backend.wt.session.gitStatusFailed": "git status failed: {detail}",
  "backend.wt.session.worktreeDirty":
    "The worktree has uncommitted changes (the ledger expects an idle clean state)",
  "backend.wt.session.postHeadMissing":
    "Ledger post_head {postHead} does not exist in Git",
  "backend.wt.session.postHeadNotAncestor":
    "Ledger post_head {postHead} is not an ancestor of the current HEAD",
  "backend.wt.session.invalidDefaultId":
    "session_id is empty after sanitization; cannot create the default worktree",
  "backend.wt.session.invalidId":
    "session_id is empty after sanitization; cannot create the worktree",
  "backend.wt.session.invalidMemberIds":
    "session_id or assignment_id is empty after sanitization; cannot create the member worktree",
  "backend.wt.session.invalidSessionId": "Invalid session_id",
  "backend.wt.continuation.invalidIds":
    "The parent or child session_id is empty after sanitization; cannot derive the continuation worktree",
  "backend.wt.continuation.childBranchExists":
    "The continuation child branch already exists: {child}",
  "backend.wt.continuation.baseRefExists":
    "The continuation child base ref already exists: {base}",
  "backend.wt.continuation.invalidChildId":
    "The child session_id is empty after sanitization; cannot clean up the continuation worktree",
  "backend.wt.continuation.pathNotUtf8":
    "The worktree path is not valid UTF-8: {path}",
  "backend.wt.continuation.removeResidualFailed":
    "Failed to remove the leftover worktree directory: {detail}",
  "backend.wt.continuation.refsStillRegistered":
    "Continuation cleanup refused to delete refs because the child worktree is still registered: {path}",
  "backend.wt.git.spawnFailed": "Failed to start git {cmd}: {detail}",
  "backend.wt.git.commandFailed": "git {cmd} failed: {stderr}",
  "backend.wt.git.revParseSpawnFailed":
    "Failed to start git rev-parse: {detail}",
  "backend.wt.git.revParseFailed": "git rev-parse HEAD failed: {stderr}",
  "backend.wt.git.sessionStatusSpawnFailed":
    "Failed to inspect session_wt {phase}-status because git could not start: {detail}",
  "backend.wt.git.sessionStatusFailed":
    "Failed to inspect session_wt {phase}-status; git {cmd} failed: {stderr}",
  "backend.wt.git.verifierSpawnFailed":
    "Failed to start the verification command: {detail}",
  "backend.wt.git.worktreeListFailed":
    "Failed to run git worktree list: {detail}",
  "backend.wt.git.worktreeListNonZero":
    "git worktree list exited non-zero (code {exitCode}): {stderr}",
  "backend.wt.scaffold.worktreeAddSpawnFailed":
    "Failed to start git worktree add: {detail}",
  "backend.wt.scaffold.verifyCheckoutFailed":
    "Failed to create the temporary verification checkout: {stderr}",
  "backend.wt.scaffold.stagingWorktreeFailed":
    "Failed to create the staging worktree: {stderr}",
  "backend.wt.scaffold.createDirFailed":
    "Failed to create the directory: {detail}",
  "backend.wt.scaffold.defaultInitSpawnFailed":
    "Failed to start git init: {detail}",
  "backend.wt.scaffold.defaultInitFailed": "git init failed: {stderr}",
  "backend.wt.scaffold.sessionWorktreeSpawnFailed":
    "Failed to start git worktree: {detail}",
  "backend.wt.scaffold.sessionWorktreeFailed": "git worktree failed: {stderr}",
  "backend.wt.scaffold.continuationWorktreeSpawnFailed":
    "Failed to start the continuation git worktree: {detail}",
  "backend.wt.scaffold.continuationWorktreeFailed":
    "The continuation git worktree failed: {stderr}",
  "backend.wt.scaffold.memberWorktreeSpawnFailed":
    "Failed to start the member git worktree: {detail}",
  "backend.wt.scaffold.memberWorktreeFailed":
    "The member git worktree failed: {stderr}",
  "backend.db.restore.parentMissing":
    "The parent session does not exist, so the continuation child cannot be restored",
  "backend.db.restore.parentDeleted":
    "The parent session is deleted, so the continuation child cannot be restored",
  "backend.db.restore.parentPointsElsewhere":
    "The parent session's continued_to_session_id points to another child, so this continuation child cannot be restored",
  "backend.db.restore.liveChildExists":
    "The parent session already has a live child, so the old continuation child cannot be restored",
  "backend.db.memory.badJson": "{field} is not valid JSON: {detail}",
  "backend.lead.spawnDriverFailed": "Failed to spawn driver: {detail}",
  "backend.lead.spawnLeadFailed": "Failed to spawn lead: {detail}",
  "backend.lead.noFinalText": "Lead produced no terminal final_text",
  "backend.lead.noFinalTextStderr":
    "Lead produced no terminal final_text · stderr tail: {stderr}",
  "backend.lead.claudeOnlyBlock1":
    "Block 1 only supports a native claude Lead (current provider={provider} access={access})",
  "backend.lead.engineNotSupported":
    "This engine can't be a lead yet (provider={provider} access={access})",
  "backend.team.oneshotSpawnFailed":
    "Failed to start run_oneshot_llm: {detail}",
  "backend.team.oneshotFailed": "run_oneshot_llm failed: {detail}",
  "backend.team.oneshotNoText": "run_oneshot_llm produced no assistant text",
  "backend.team.noMemberOutput":
    "No team member produced output, so synthesis cannot continue",
  "backend.team.summarizeSpawnFailed":
    "Failed to start lead_summarize: {detail}",
  "backend.team.summarizeFailed": "Lead synthesis failed: {detail}",
  "backend.team.summarizeNoText": "Lead synthesis produced no assistant text",
  "backend.lead.claudeOnlyStep":
    "lead_step only supports a native claude Lead (current access={access} provider={provider})",
  "backend.lead.parseSpawnFailed": "Unable to parse Lead output: {detail}",
  "backend.lead.parseNoOutput": "Unable to parse Lead output: {detail}",
  "backend.lead.parseFailed": "Unable to parse Lead output: {detail}",
  "backend.lead.claudeOnlyDraft":
    "B1 only supports native claude as the driver (current access={access} provider={provider}; borrowed/codex drivers are deferred)",
  "backend.lead.draftNoFinalText":
    "Driver produced no terminal final_text or reported an error",
  "backend.lead.draftNoFinalTextStderr":
    "Driver produced no terminal final_text or reported an error · stderr tail: {tail}",
  "backend.leadTools.askUserNeedsOptions":
    "ask_user requires options (at least 2)",
  "backend.leadTools.verifierLocalUnsupported":
    "propose_verifier: Local sessions are not supported yet",
} as const;

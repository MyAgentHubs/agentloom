export const messages = {
  "backend.ui.badLocale": "无效的界面语言",
  "backend.session.idContainsPipe": "会话 ID 不能包含竖线（|）：{id}",
  "backend.agent.missingApiKey": "缺少 API key",
  "backend.agent.unknownAccess": "未知 agent access：{access}",
  "backend.agent.notFound": "未知 agent",
  "backend.agent.missingId": "缺少 agent_id",
  "backend.agent.invalidReasoningTier": "无效 reasoning_tier：{tier}",
  "backend.agent.nativeAccessImmutable": "原生 agent 不可切换接入方式",
  "backend.agent.nativeKeyUnsupported": "原生 agent 不可设置 key",
  "backend.agent.keychainSaveFailed":
    "无法将 API key 保存到系统钥匙串，key 未生效。请重试或检查系统钥匙串权限。（详情：{detail}）",
  "backend.agent.keychainKeyUnavailable": "{detail}",
  "backend.agent.sessionRunUnknown":
    "resolve_session_run_agent: 会话尚无运行记录·无法判定 agent",
  "backend.agent.idNotFound": "agent {id} 不存在",
  "backend.agent.emptyFilteredId": "agent id 过滤后为空",
  "backend.agent.unknownEngine": "未知引擎：{engine}",
  "backend.agent.configDirCreateFailed": "创建配置目录失败：{detail}",
  "backend.agent.promptFileDirCreateFailed":
    "创建 prompt 临时目录失败：{detail}",
  "backend.agent.promptFileCreateFailed": "创建 prompt 临时文件失败：{detail}",
  "backend.agent.promptFileWriteFailed": "写入 prompt 临时文件失败：{detail}",
  "backend.agent.pastedDirCreateFailed": "创建粘贴附件目录失败：{detail}",
  "backend.agent.missingEndpoint": "agent {id} 缺少 endpoint",
  "backend.cliPath.invalidCli": "无法设置 CLI 路径：不支持 {cli}。",
  "backend.cliPath.invalidPath":
    "无法使用你选择的路径：{path}。请选择 CLI 的可执行文件；Windows 上请选择 .exe、.cmd 或 .bat 文件。",
  "backend.cliPath.databaseUnavailable":
    "设置没有保存成功，请重试。详情：{detail}",
  "backend.remoteControl.invalidRelayUrl": "中继地址必须以 wss:// 开头",
  "backend.remoteControl.activeProjectMissing":
    "找不到该项目（id：{repoId}），无法设为活跃项目",
  "backend.remoteControl.pairingNeedsActiveProject":
    "未设置活跃项目时不能开始配对，请先在遥控设置里选择项目",
  "backend.member.notInSessionPool": "agent {id} 不在当前会话成员池",
  "backend.member.unavailableMissing": "agent {id} 不可用：不存在",
  "backend.member.unavailableDisabled": "agent {id} 不可用：disabled",
  "backend.member.emptyTeam": "team run 至少需要一个成员",
  "backend.member.spawnFailed": "成员启动失败：{detail}",
  "backend.member.noResult": "run_single_worker：worker 未产生 MemberResult",
  "backend.gh.gitSpawnFailed": "git 启动失败：{detail}",
  "backend.mcp.noPort": "无法取得监听端口",
  "backend.proxy.noPort": "代理无法解析监听端口",
  "backend.criteria.lineTooLong": "验收标准单行过长（>{max}）",
  "backend.criteria.invalidSyntax":
    "验收标准语法非法：{raw}（用 cmd:/contains:<s>:/judge:）",
  "backend.criteria.tooMany": "验收标准过多（>{max}）",
  "backend.file.markdownOnly": "仅允许 .md/.markdown",
  "backend.file.parentMissing": "父目录不存在",
  "backend.file.pathOutOfBounds": "路径越界",
  "backend.file.notFound": "文件不存在",
  "backend.file.openFilesOnly": "只能打开文件",
  "backend.file.tooLarge":
    "文件过大：{size} bytes，当前只预览 {max} bytes 以内的文本文件",
  "backend.file.binaryPreviewUnsupported": "暂不支持二进制文件预览",
  "backend.file.htmlOnly": "只能在浏览器打开 .html/.htm 文件",
  "backend.file.ambiguousBasename":
    "同名文件有多个，请用更完整的路径：{0} → {1}",
  "backend.file.basenameBudget":
    "同名文件太多，搜索范围超限，请提供更完整的路径（{0}）",
  "backend.file.openExternalFailed": "无法在系统浏览器打开文件：{detail}",
  "backend.file.repoLookupFailed": "查项目失败：{detail}",
  "backend.file.repoNotFound": "项目不存在",
  "backend.repo.namespaceLookupFailed": "查 namespace 失败：{detail}",
  "backend.repo.lookupFailed": "查 repo 失败：{detail}",
  "backend.repo.activeReposLookupFailed": "查 active repos 失败：{detail}",
  "backend.repo.duplicateLookupFailed": "查重失败：{detail}",
  "backend.repo.setLastActiveFailed": "set last_active 失败：{detail}",
  "backend.repo.ensureNamespaceFailed": "ensure namespace 失败：{detail}",
  "backend.repo.insertRepoFailed": "插入 repo 失败：{detail}",
  "backend.repo.namespaceMismatch":
    "REPO_NAMESPACE_MISMATCH:repo {repoId} 属 {actualNamespaceId} 非 {namespaceId}",
  "backend.repo.pathNotFound": "路径不存在：{path}",
  "backend.repo.pathNotDirectory": "路径不是目录：{path}",
  "backend.repo.pathInsideAppDomain":
    "不能添加 AgentLoom 自己的数据目录（~/.agentloom）中的项目：{path}。请把项目移到该目录之外后重试。",
  "backend.repo.insertFailed": "插入失败：{detail}",
  "backend.project.notFound": "项目不存在",
  "backend.project.pathRequired": "请选择新的工作目录",
  "backend.project.canonicalizeFailed": "无法解析路径：{detail}",
  "backend.project.pathNotWritable": "该目录不可写，请检查权限：{detail}",
  "backend.project.pathAlreadyRegistered":
    "该目录已是项目「{name}」的工作目录，不能重复关联：{path}",
  "backend.project.pathNestsAnotherProject":
    "该目录与项目「{name}」的工作目录互为父子目录，两个项目不能嵌套：{path}",
  "backend.project.githubPathNotGitRepo":
    "这不是一个 git 仓库（找不到 .git）：{path}。GitHub 项目的工作目录必须是已 clone 的 git 仓库。",
  "backend.project.invalidPath": "路径包含无法识别的字符",
  "backend.project.pathUpdateFailed": "更新项目路径失败：{detail}",
  "backend.project.databaseUnavailable": "数据库暂不可用，请重试：{detail}",
  "backend.landing.protectedPath": "落地前检查未通过：受保护路径 {paths}",
  "backend.landing.noEvidence":
    "落地前检查未通过：找不到 worker changed_files 证据",
  "backend.landing.scopeExceeded":
    "落地前检查未通过：改动超出 worker 声明 {files}",
  "backend.landing.l1NotGreen":
    "L1 未绿（无 passed 复验·或证据 SHA 不对应当前 commit）·不准合·见 spec L4",
  "backend.merge.stagingConflict": "改动与 staging 冲突·已拒（不自动解冲突）",
  "backend.finalize.noChanges": "worker 没有改动·无可固化",
  "backend.finalize.gitUnavailable":
    "当前项目不是 git 仓库；agent 改动已保留，但 git 接力不可用",
  "backend.finalize.uncommittedChanges":
    "worker 留有未提交改动；app 不会自动提交，改动已保留在工作目录",
  "backend.artifact.notReadyVerify":
    "artifact 未 ready（无 commit_sha）·不能验",
  "backend.artifact.noShaPreflight": "artifact 无 commit_sha·不能做落地检查",
  "backend.artifact.notReadyMerge": "artifact 未 ready（state={state}）·不能合",
  "backend.artifact.noShaMerge": "artifact 无 commit_sha·不能合",
  "backend.artifact.notFound": "artifact 不存在：{id}",
  "backend.run.repoNotFound": "repo {id} 不存在",
  "backend.run.preHeadReadFailed": "读 pre_head 失败：{detail}",
  "backend.run.ledgerPendingWriteFailed": "写 ledger pending 失败：{detail}",
  "backend.run.spawnFailed": "启动失败：{detail}",
  "backend.run.teamMembersActive": "无法开始新运行：{detail}",
  "backend.run.stdoutUnavailable": "无法读取输出",
  "backend.run.workspaceCanonicalizeFailed": "canonicalize 失败：{detail}",
  "backend.run.unknownLeadAgent": "未知 lead agent：{id}",
  "backend.run.unknownLeadAgentGeneric": "未知 lead agent",
  "backend.run.tombstoneRestoreFailed":
    "TOMBSTONE_FAILED_RESTORE_FAILED:tombstone={tombstone};restore={restore}（DB/git 背离·需刀二b reconcile）",
  "backend.run.invalidSessionId": "session_id 无效",
  "backend.run.inplaceDeliveryUncommitted":
    "就地改动尚未提交：{count} 个文件仍在工作区（{files}）。请先在项目里提交这些改动，再推送 / 创建 PR / 发布。",
  "backend.delivery.confirmationRequired":
    "{operation} 需要本次用户明确确认，未执行任何远端操作",
  "backend.publish.pushed": "已推送到 origin/{branch}",
  "backend.publish.failed.boundRepo":
    "PUBLISH_FAILED:会话已绑 GitHub repo·应走 push/PR 而非 publish",
  "backend.publish.needsAccount.missing":
    "PUBLISH_NEEDS_ACCOUNT:未检测到 gh 登录账户（gh auth login）",
  "backend.publish.needsAccount.multiple":
    "PUBLISH_NEEDS_ACCOUNT:检测到多个 gh 账户·请指定身份（{list}）",
  "backend.publish.failed": "PUBLISH_FAILED:{detail}",
  "backend.publish.failed.missingRepoName":
    "PUBLISH_FAILED:缺 repo_name（无 goal_title 可回退）",
  "backend.continuation.invalidParentSessionId":
    "parent session_id 清洗后为空，无法创建接续子会话",
  "backend.continuation.childSessionIdUnavailable": "无法生成唯一接续子会话 id",
  "backend.continuation.startCleanupFailed":
    "{original}; cleanup errors: {errors}",
  "backend.continuation.handoffRequired": "接续启动指令（交接文档）不得为空",
  "backend.continuation.handoffTimedOut":
    "接续草稿生成超时，请重试。父会话现已可以继续操作。",
  "backend.continuation.invalidSessionId":
    "session_id 清洗后为空，无法拼装接续交棒单",
  "backend.continuation.localSessionUnsupported":
    "本地会话暂不支持接续（此功能对本地会话尚未开放）",
  "backend.lead.claudeOnlyContinuation":
    "Team 接续暂仅支持 native claude 会话（非-claude lead 泛化中·先用 Solo）",
  "backend.apply.repoDetached":
    "当前 repo 处于 detached HEAD·不在分支上·不应用（先 checkout 一个分支）",
  "backend.apply.repoDirty": "当前 repo 工作树有未提交改动·先提交或暂存再应用",
  "backend.apply.branchAdvanced":
    "当前分支无法 ff 到 staging（可能已前进或分叉·v1 不强推）：当前分支已前进",
  "backend.apply.fastForwardFailed":
    "当前分支无法 ff 到 staging（可能已前进或分叉·v1 不强推）：{detail}",
  "backend.wt.verifier.unsupportedPlatform":
    "verifier sandbox 在本平台不可用·MVP 仅支持 macOS·Linux sandbox 是 follow-up",
  "backend.wt.verifier.writeAttempt":
    "verifier 试图改文件·已拒·写文件请走 dispatch_worker",
  "backend.wt.verifier.canonicalizeFailed":
    "无法规范化会话工作树/HOME 路径·verifier 拒绝在无沙箱护栏下就地跑：{detail}",
  "backend.wt.sessionMerge.artifactBaseMismatch":
    "artifact {artifact} 不基于 base_sha {base}·拒合",
  "backend.wt.sessionMerge.stagingBaseMismatch":
    "staging {staging} 不基于 base_sha {base}·拒合",
  "backend.wt.sessionMerge.outsideAppDomain":
    "Stage① 拒合：session_wt {path} 不在 app 域（~/.agentloom）·fail-closed",
  "backend.wt.sessionMerge.invalidHead":
    "Stage① 拒合：会话 wt HEAD 非 attached 到 agentloom/*（detached 或非 agentloom 分支）·fail-closed",
  "backend.wt.sessionMerge.memberMissing":
    "Stage① 拒合：member 分支 {member} 不存在",
  "backend.wt.sessionMerge.dirtyWorktree":
    "Stage① 拒合：会话 worktree 有未提交改动·fail-closed",
  "backend.wt.sessionMerge.stagingBranchMissing":
    "staging 分支不存在：{staging}",
  "backend.wt.cleanup.commitOutsideAppDomain":
    "commit_dirty 拒:{path} 不在 app 域·fail-closed",
  "backend.wt.cleanup.commitInvalidHead":
    "commit_dirty 拒:{path} HEAD 非 attached 到 agentloom/*·fail-closed",
  "backend.wt.cleanup.sessionWorktreeReleased":
    "finalize-before-cleanup 拒清:会话 wt 已释放但仍有 {pending} 个 member 分支待并·fail-closed(重建会话 wt 后重试/刀二b reconcile)",
  "backend.wt.cleanup.invalidMemberRef":
    "finalize-before-cleanup 拒清:member ref {member} 格式异常·fail-closed",
  "backend.wt.cleanup.memberWorktreeDetached":
    "finalize-before-cleanup 拒清:member worktree {path} 非 attached 到 {member}(detached/异常态)·fail-closed·交刀二b reconcile",
  "backend.wt.cleanup.notFastForward":
    "finalize-before-cleanup 拒清:member {member} 非 ff(stale-base·刀二b/并行处理)·fail-closed",
  "backend.wt.cleanup.registrationIncomplete":
    "release/trash 拒:worktree {path} 反登记未完成·fail-closed",
  "backend.wt.cleanup.trashRefExists":
    "trash 拒:{trash} 已存在·防覆盖旧 grace 副本·fail-closed(刀二b reconcile)",
  "backend.wt.restore.headsRefExists":
    "restore 拒:{heads} 已存在·防覆盖 live 分支·fail-closed(刀二b reconcile)",
  "backend.wt.restore.refsMissing":
    "restore 拒:会话 {session} 的 trash 与 heads ref 均不存在(purge 半失败/已 gc)·无可恢复·fail-closed(刀二b reconcile)",
  "backend.wt.restore.compensationTrashExists":
    "restore 补偿拒:{trash} 已存在·防覆盖 trash ref·fail-closed(刀二b reconcile)",
  "backend.wt.restore.compensationHeadsMissing":
    "restore 补偿拒:{heads} 不存在·无法移回 trash·fail-closed(刀二b reconcile)",
  "backend.wt.gc.liveWorktree":
    "gc 拒:会话 {session} 仍有活 worktree 注册·fail-closed",
  "backend.wt.gc.liveHeads":
    "gc 拒:会话 {session} 的 heads(live 分支)仍在·base 是其 diff fork 点·fail-closed(刀二b reconcile)",
  "backend.wt.session.gitStatusSpawnFailed": "git status 启动失败：{detail}",
  "backend.wt.session.gitStatusFailed": "git status 失败：{detail}",
  "backend.wt.session.worktreeDirty":
    "worktree 有未提交改动（ledger 静止态应干净）",
  "backend.wt.session.postHeadMissing":
    "ledger post_head {postHead} 在 git 中不存在",
  "backend.wt.session.postHeadNotAncestor":
    "ledger post_head {postHead} 非当前 HEAD 祖先",
  "backend.wt.session.invalidDefaultId":
    "session_id 清洗后为空，无法建默认 worktree",
  "backend.wt.session.invalidId": "session_id 清洗后为空，无法建 worktree",
  "backend.wt.session.invalidMemberIds":
    "session_id/assignment_id 清洗后为空，无法建 member worktree",
  "backend.wt.session.invalidSessionId": "session_id 无效",
  "backend.wt.continuation.invalidIds":
    "parent/child session_id 清洗后为空，无法派生接续 worktree",
  "backend.wt.continuation.childBranchExists": "接续子分支已存在：{child}",
  "backend.wt.continuation.baseRefExists": "接续子 base-ref 已存在：{base}",
  "backend.wt.continuation.invalidChildId":
    "child session_id 清洗后为空，无法清理接续 worktree",
  "backend.wt.continuation.pathNotUtf8": "worktree 路径非 UTF-8：{path}",
  "backend.wt.continuation.removeResidualFailed":
    "删除残留 worktree 目录失败：{detail}",
  "backend.wt.continuation.refsStillRegistered":
    "接续清理拒删 refs:子 worktree 仍注册：{path}",
  "backend.wt.git.spawnFailed": "git 启动失败：{detail}",
  "backend.wt.git.commandFailed": "git {cmd} 失败：{stderr}",
  "backend.wt.git.revParseSpawnFailed": "git rev-parse 启动失败：{detail}",
  "backend.wt.git.revParseFailed": "git rev-parse HEAD 失败：{stderr}",
  "backend.wt.git.sessionStatusSpawnFailed":
    "session_wt {phase}-status 失败：git 启动失败：{detail}",
  "backend.wt.git.sessionStatusFailed":
    "session_wt {phase}-status 失败：git {cmd} 失败：{stderr}",
  "backend.wt.git.verifierSpawnFailed": "验证命令启动失败：{detail}",
  "backend.wt.git.worktreeListFailed": "git worktree list 失败：{detail}",
  "backend.wt.git.worktreeListNonZero":
    "git worktree list 退出非 0(code {exitCode})：{stderr}",
  "backend.wt.scaffold.worktreeAddSpawnFailed":
    "git worktree add 启动失败：{detail}",
  "backend.wt.scaffold.verifyCheckoutFailed":
    "建临时 verify checkout 失败：{stderr}",
  "backend.wt.scaffold.stagingWorktreeFailed":
    "建 staging worktree 失败：{stderr}",
  "backend.wt.scaffold.createDirFailed": "建目录失败：{detail}",
  "backend.wt.scaffold.defaultInitSpawnFailed": "git init 启动失败：{detail}",
  "backend.wt.scaffold.defaultInitFailed": "git init 失败：{stderr}",
  "backend.wt.scaffold.sessionWorktreeSpawnFailed":
    "git worktree 启动失败：{detail}",
  "backend.wt.scaffold.sessionWorktreeFailed": "git worktree 失败：{stderr}",
  "backend.wt.scaffold.continuationWorktreeSpawnFailed":
    "git continuation worktree 启动失败：{detail}",
  "backend.wt.scaffold.continuationWorktreeFailed":
    "git continuation worktree 失败：{stderr}",
  "backend.wt.scaffold.memberWorktreeSpawnFailed":
    "git member worktree 启动失败：{detail}",
  "backend.wt.scaffold.memberWorktreeFailed":
    "git member worktree 失败：{stderr}",
  "backend.db.restore.parentMissing": "父会话不存在，无法恢复接续子会话",
  "backend.db.restore.parentDeleted": "父会话已删除，无法恢复接续子会话",
  "backend.db.restore.parentPointsElsewhere":
    "父会话 continued_to_session_id 指向其它子会话，无法恢复接续子会话",
  "backend.db.restore.liveChildExists":
    "父会话已有 live 子会话，无法恢复旧接续子会话",
  "backend.db.memory.badJson": "{field} 非合法 JSON: {detail}",
  "backend.lead.spawnDriverFailed": "spawn driver 失败：{detail}",
  "backend.lead.spawnLeadFailed": "spawn lead 失败：{detail}",
  "backend.lead.noFinalText": "lead 无终态 final_text",
  "backend.lead.noFinalTextStderr":
    "lead 无终态 final_text·stderr 尾部：{stderr}",
  "backend.lead.claudeOnlyBlock1":
    "块① 仅支持 native claude 队长（当前 provider={provider} access={access}）",
  "backend.lead.engineNotSupported":
    "当前引擎暂不支持当队长（provider={provider} access={access}）",
  "backend.team.oneshotSpawnFailed": "run_oneshot_llm 启动失败：{detail}",
  "backend.team.oneshotFailed": "run_oneshot_llm 失败：{detail}",
  "backend.team.oneshotNoText": "run_oneshot_llm 没有产生 assistant 文本",
  "backend.team.noMemberOutput": "无队员产出·无法综合",
  "backend.team.summarizeSpawnFailed": "lead_summarize 启动失败：{detail}",
  "backend.team.summarizeFailed": "lead 综合失败：{detail}",
  "backend.team.summarizeNoText": "lead 综合没有产生 assistant 文本",
  "backend.lead.claudeOnlyStep":
    "lead_step 仅支持 native claude lead（当前 access={access} provider={provider}）",
  "backend.lead.parseSpawnFailed": "lead 输出无法解析：{detail}",
  "backend.lead.parseNoOutput": "lead 输出无法解析：{detail}",
  "backend.lead.parseFailed": "lead 输出无法解析：{detail}",
  "backend.lead.claudeOnlyDraft":
    "B1 仅支持 native claude 作为 driver（当前 access={access} provider={provider}·borrow/codex driver 留 follow-up）",
  "backend.lead.draftNoFinalText": "driver 无终态 final_text 或报错",
  "backend.lead.draftNoFinalTextStderr":
    "driver 无终态 final_text 或报错·stderr 尾部：{tail}",
  "backend.leadTools.askUserNeedsOptions": "ask_user 需 options (至少 2 个)",
  "backend.leadTools.verifierLocalUnsupported":
    "propose_verifier: Local 会话暂不支持",
} as const;

# #1 文件任务索引

每行对应 graph.json 的完整节点。顺序是建议优先级；同一 mutex 的节点串行合入，不能共享工作目录并发写。file_epic 是拆单容器，不是无限预算的实现任务。

| 节点 | 文件 | 当前 / 上限 | 可用性 | 首步 |
|---|---|---:|---|---|
| F01 | `app/src-tauri/src/agent.rs` | 1348 / 800 | public | 按 agent profile/环境/命令构造/执行适配职责逐组拆 helper；保留错误、凭证与平台 cfg |
| F02 | `app/src-tauri/src/agent_event.rs` | 1229 / 800 | public | 现有 parse_claude/parse_harness 保留；按其余事件族及映射 helper 拆 |
| F03 | `app/src-tauri/src/checkpoint_hook.rs` | 1503 / 800 | public | 先把测试/纯输入处理分开 |
| F04 | `app/src-tauri/src/commit_broker.rs` | 893 / 800 | public | 外移完整内联 mod tests 到同名子目录/tests.rs，保留 cfg(test) 和既有子模块 |
| F05 | `app/src-tauri/src/continuation.rs` | 1004 / 800 | public | 外移完整内联 mod tests 到同名子目录/tests.rs，保留 cfg(test) 和既有子模块 |
| F06 | `app/src-tauri/src/db.rs` | 5814 / 800 | public | 逐域拆 settings、goals/acceptance、remote rooms/inbox、devices/registry |
| F07 | `app/src-tauri/src/detect.rs` | 940 / 800 | public | 先搬测试 helper，再按 CLI 发现/provider 探测纯函数边界拆 |
| F08 | `app/src-tauri/src/event_transport.rs` | 902 / 800 | public | 分离纯批处理/帧组装 helper 或内联测试 |
| F09 | `app/src-tauri/src/github.rs` | 987 / 800 | public | 外移完整内联 mod tests 到同名子目录/tests.rs，保留 cfg(test) 和既有子模块 |
| F10 | `app/src-tauri/src/lead_step.rs` | 814 / 800 | public | 先搬独立 prompt/result helper，避免已有 context_prompt 回搬 |
| F11 | `app/src-tauri/src/lead_tools.rs` | 1148 / 800 | public | 按工具注册、参数验证、执行与结果转换拆 |
| F12 | `app/src-tauri/src/lib.rs` | 16735 / 800 | public | 先验证单个 tauri command 连同属性迁出后裸名 generate_handler 注册仍能编译；保持命令清单内容和顺序 |
| F13 | `app/src-tauri/src/member_runner.rs` | 2782 / 800 | public | 按 spawn/reader/watchdog、report、usage、finalization 边界各做一单 |
| F14 | `app/src-tauri/src/remote_gateway.rs` | 6868 / 800 | public | 先盘点类型和测试模块父作用域依赖 |
| F15 | `app/src-tauri/src/remote_pairing.rs` | 1563 / 800 | public | 搬内联 store 模块；store 仍过大则按 token/registry 存储再拆 |
| F16 | `app/src-tauri/src/sandbox.rs` | 1228 / 800 | public | 外移完整内联 mod tests 到同名子目录/tests.rs，保留 cfg(test) 和既有子模块 |
| F17 | `app/src-tauri/src/updater.rs` | 2505 / 800 | public | 先拆内联 mac_shell/other_platform_shell，模块本身再按功能细分到 <=720 |
| F18 | `app/src-tauri/src/updater_install.rs` | 1598 / 800 | public | 按路径/平台、staging、安装动作、健康确认与恢复拆 |
| F19 | `app/src-tauri/src/worktree.rs` | 4367 / 800 | public | 逐域拆 git read/query、argv/ignored、landing |
| F20 | `app/src/App.tsx` | 7105 / 500 | public | 先抽纯类型、eventBatch、navHistory、codingSelectors；原入口回导保持现有测试导入 |
| F21 | `app/src/components/FilesPanel.tsx` | 635 / 500 | public | 抽图片源解析、match/highlight、ancestor/visible tree 纯函数到 filesPanelUtils.tsx |
| F22 | `app/src/components/InputArea.tsx` | 830 / 500 | public | 抽 RunningStatusDetails/Clock、语言/编码纯 helper |
| F23 | `app/src/components/MessageContent.tsx` | 1136 / 500 | public | 抽图片菜单纯函数与 ImageBlocks，保持 AttachmentPort 注入 |
| F24 | `app/src/components/MessageStream.tsx` | 702 / 500 | public | 先抽 key/hash/equality helper |
| F25 | `app/src/components/OverviewHome.tsx` | 759 / 500 | public | 外移 RecentActivitySection |
| F26 | `app/src/components/Sidebar.tsx` | 518 / 500 | public | 搬 arrangeContinuationThreads/continuationChildrenByParent 到 sidebarThreads.ts；导出与排序不变 |
| F27 | `app/src/components/UndoReviewPanel.tsx` | 700 / 500 | public | 抽 skipReason/bytes/path/result 纯函数 |
| F28 | `app/src/components/settings/AgentForm.tsx` | 1989 / 500 | public | 抽静态样式与纯模型/provider 数据 |
| F29 | `app/src/components/settings/SettingsRemoteControl.tsx` | 1284 / 500 | public | 抽样式与 QR/relay URL 纯函数 |
| F30 | `app/src/components/settings/SettingsSearch.tsx` | 522 / 500 | public | 只搬 styles 到同级 SettingsSearch.styles.ts；宿主保持逻辑不变 |
| F31 | `app/src/components/settings/agentFormHelpers.ts` | 696 / 500 | public | 搬 PROVIDER_PRESETS 到 providerPresets.ts |
| F32 | `app/src/styles/global.css` | 10645 / 800 | public | 从文件头按完整 CSS 规则边界顺序切，每块最多 720 行 |
| F33 | `app/src/types/agent.ts` | 725 / 500 | public | 若为 app/src/types：按 events/teamRun/memberResult/blocks/session 纯类型拆并从原入口回导 |
| F34 | `harness-agent/src/cli.rs` | 1924 / 800 | public | 按完整测试组搬内联测试 |
| F35 | `harness-agent/src/evaluator.rs` | 1905 / 800 | public | 先搬内联测试并修 tests/fixtures 相对路径 |
| F36 | `harness-agent/src/exec/controlled/mod.rs` | 960 / 800 | public | 仅 exec/controlled/mod.rs：按进程启动/等待/输出处理边界拆 helper |
| F37 | `harness-agent/src/orchestrator/probe_runner.rs` | 1488 / 800 | public | 搬内联测试到 probe_runner/tests.rs；保持 use super::* 作用域 |
| F38 | `harness-agent/src/orchestrator/tests.rs` | 8910 / 1500 | public | 保留共享 setup/helper 与既有子模块 |
| F39 | `harness-agent/src/plan/contract.rs` | 1138 / 800 | public | REC-C1 已提供本文件实现；补丁合入后只复验，标 done，不再拆一次 |
| F40 | `harness-agent/src/plan/replan.rs` | 1023 / 800 | public | REC-C1 已提供本文件实现；补丁合入后只复验，标 done，不再拆一次 |
| F41 | `harness-agent/src/plan/run_plan.rs` | 4114 / 800 | public | 先逐组外移内联测试，修夹具相对路径；测试模块 <=1350 |
| F42 | `harness-agent/src/plan/write_audit.rs` | 1031 / 800 | public | REC-C1 已提供本文件实现；补丁合入后只复验，标 done，不再拆一次 |
| F43 | `harness-agent/src/provider/openai_compatible.rs` | 1054 / 800 | public | 搬内联测试到 openai_compatible/tests.rs |
| F44 | `remote-relay/src/room-do.js` | 2296 / 500 | 外部仓 / 源码待对齐 | 按 connection/attach、frame validation、command routing、delivery/cleanup 拆 |
| F45 | `remote-relay/src/room-store.js` | 1349 / 500 | 外部仓 / 源码待对齐 | 按 schema/migration、token registry、refresh reply routes、pending input 拆 |
| F46 | `remote-relay/test/g8-message-rate-limit.test.js` | 965 / 500 | 外部仓 / 源码待对齐 | 门禁分类未经批准保持当前 500 上限；按完整场景/describe 拆小测试文件 |
| F47 | `remote-relay/test/msg-reply-route.test.js` | 621 / 500 | 外部仓 / 源码待对齐 | 门禁分类未经批准保持当前 500 上限；按完整场景/describe 拆小测试文件 |
| F48 | `remote-relay/test/room-do.test.js` | 1616 / 500 | 外部仓 / 源码待对齐 | 门禁分类未经批准保持当前 500 上限；按完整场景/describe 拆小测试文件 |
| F49 | `remote-relay/test/room-lifecycle-fixture.test.js` | 826 / 500 | 外部仓 / 源码待对齐 | 门禁分类未经批准保持当前 500 上限；按完整场景/describe 拆小测试文件 |
| F50 | `remote-relay/test/room-store.test.js` | 565 / 500 | 外部仓 / 源码待对齐 | 门禁分类未经批准保持当前 500 上限；按完整场景/describe 拆小测试文件 |
| F51 | `remote-relay/test/s1d-fixture.test.js` | 868 / 500 | 外部仓 / 源码待对齐 | 门禁分类未经批准保持当前 500 上限；按完整场景/describe 拆小测试文件 |
| F52 | `remote-relay/test/s1i2-fixture.test.js` | 855 / 500 | 外部仓 / 源码待对齐 | 门禁分类未经批准保持当前 500 上限；按完整场景/describe 拆小测试文件 |
| F53 | `remote-relay/test/s1ja-fake-mobile-e2e.test.js` | 790 / 500 | 外部仓 / 源码待对齐 | 门禁分类未经批准保持当前 500 上限；按完整场景/describe 拆小测试文件 |
| F54 | `remote-relay/test/wire-fixture.test.js` | 724 / 500 | 外部仓 / 源码待对齐 | 门禁分类未经批准保持当前 500 上限；按完整场景/describe 拆小测试文件 |
| F55 | `remote-web/src/app/AppRuntime.e2e.test.tsx` | 1985 / 1500 | 外部仓 / 源码待对齐 | 按设置、replay、reconnect、history 分测试文件 |
| F56 | `remote-web/src/app/AppRuntime.tsx` | 1470 / 500 | 外部仓 / 源码待对齐 | 依次抽 bodyCache、historyRequests、frameRouter、connectionLifecycle hook |
| F57 | `remote-web/src/app/commandChannel.ts` | 774 / 500 | 外部仓 / 源码待对齐 | 抽 command record 类型/常量、retry/watchdog |
| F58 | `remote-web/src/connection/connectionSession.ts` | 931 / 500 | 外部仓 / 源码待对齐 | 按 handshake、refresh、replayWatchdog 拆自由函数模块 |
| F59 | `remote-web/src/events/parseFrame.ts` | 876 / 500 | 外部仓 / 源码待对齐 | 抽 frameTypes 并回导；按 session index/message/control 帧族拆 parser |
| F60 | `remote-web/src/ui/stream/SessionStreamScreen.tsx` | 669 / 500 | 外部仓 / 源码待对齐 | 外移 MessageRow/ActivitySummaryChip/MsgFetchFooter/LiveMessageRow/StopBadge |

## 追加：旧清单遗漏的前端长函数

这是独立于文件大小的债务轴。文件父节点先完成，再清该文件剩余长函数；无文件债务的路径直接从 BOOT 开始。

| 节点 | 文件 | 实测函数数 | 诊断位置（仅作定位线索） |
|---|---|---:|---|
| LF01 | `app/src/App.tsx` | 6 | 534, 2494, 3386, 3390, 3490, 5161 |
| LF02 | `app/src/__tests__/helpers/appTestFixtures.ts` | 1 | 8 |
| LF03 | `app/src/__tests__/helpers/appTestMocks.ts` | 1 | 6 |
| LF04 | `app/src/__tests__/helpers/appTestReviewScenarios.tsx` | 1 | 16 |
| LF05 | `app/src/components/AboutDialog.tsx` | 1 | 20 |
| LF06 | `app/src/components/ComposerAgentSelector.tsx` | 1 | 119 |
| LF07 | `app/src/components/ContinuationBriefPanel.tsx` | 1 | 46 |
| LF08 | `app/src/components/FilesPanel.tsx` | 1 | 208 |
| LF09 | `app/src/components/GateCard.tsx` | 1 | 28 |
| LF10 | `app/src/components/GlobalSearch.tsx` | 1 | 84 |
| LF11 | `app/src/components/InputArea.tsx` | 1 | 236 |
| LF12 | `app/src/components/MarkdownBody.tsx` | 1 | 58 |
| LF13 | `app/src/components/MemberDrillIn.tsx` | 1 | 116 |
| LF14 | `app/src/components/MessageContent.tsx` | 3 | 245, 747, 876 |
| LF15 | `app/src/components/MessageStream.tsx` | 1 | 415 |
| LF16 | `app/src/components/NewProjectSheet.tsx` | 1 | 37 |
| LF17 | `app/src/components/OverviewHome.tsx` | 1 | 245 |
| LF18 | `app/src/components/RepoList.tsx` | 1 | 26 |
| LF19 | `app/src/components/RepoManagePanel.tsx` | 1 | 173 |
| LF20 | `app/src/components/RepoSwitcherDropdown.tsx` | 1 | 30 |
| LF21 | `app/src/components/RightPanel.tsx` | 1 | 136 |
| LF22 | `app/src/components/RunCard.tsx` | 1 | 11 |
| LF23 | `app/src/components/SessionMain.tsx` | 1 | 116 |
| LF24 | `app/src/components/SessionMenu.tsx` | 1 | 31 |
| LF25 | `app/src/components/SessionRow.tsx` | 1 | 66 |
| LF26 | `app/src/components/Sidebar.tsx` | 1 | 135 |
| LF27 | `app/src/components/SurfaceHeader.tsx` | 1 | 66 |
| LF28 | `app/src/components/UndoReviewPanel.tsx` | 1 | 386 |
| LF29 | `app/src/components/UpdateButton.tsx` | 1 | 116 |
| LF30 | `app/src/components/settings/AgentForm.tsx` | 1 | 639 |
| LF31 | `app/src/components/settings/SettingsAgents.tsx` | 1 | 158 |
| LF32 | `app/src/components/settings/SettingsRemoteControl.tsx` | 1 | 443 |
| LF33 | `app/src/components/settings/SettingsSearch.tsx` | 1 | 158 |
| LF34 | `app/src/components/settings/UpdateSection.tsx` | 1 | 28 |
| LF35 | `app/src/lib/useTeamConfig.ts` | 1 | 194 |
| LF36 | `remote-web/src/app/AppRuntime.tsx` | 1 | 177 |
| LF37 | `remote-web/src/app/RootRouter.tsx` | 1 | 161 |
| LF38 | `remote-web/src/app/pairingTransport.ts` | 1 | 89 |
| LF39 | `remote-web/src/ui/stream/SessionStreamScreen.tsx` | 1 | 177 |

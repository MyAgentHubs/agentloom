# 执行与验收

## Bootstrap：全新 public 克隆

维护者先发布并激活专用分支，贡献者再执行。已有工作目录不运行清空依赖命令。没有 macOS/Tauri 条件的贡献者先选择 engine 或纯 TypeScript 节点，不能声称 app 已验证。

```bash
git clone https://github.com/MyAgentHubs/agentloom.git
cd agentloom
git fetch origin myagenthubs/number1-debt-graph
git switch --detach origin/myagenthubs/number1-debt-graph
# SHA 应匹配 publication 状态中的批准基线；每次节点开始记录父提交。
git switch -c myagenthubs/number1-YOUR-NODE
# Rust stable + rustfmt + clippy，Node.js 20+，本平台 Tauri prerequisites。
cargo build --release --locked --manifest-path harness-agent/Cargo.toml
triple="$(rustc -vV | sed -n 's/^host: //p')"
mkdir -p app/src-tauri/binaries
cp harness-agent/target/release/myagent "app/src-tauri/binaries/myagent-${triple}"
# Windows 对应 .exe；不要为其他 triple 伪造二进制。
# 仅全新独立克隆：
(cd app && npm ci)
```

不要把自己的私有 API key、生产数据库、真实用户消息用作测试夹具。测试应使用临时目录，不包含机器专属 home 路径。

## 模型与预算

执行前读取 [MODEL-POLICY.md](MODEL-POLICY.md)。节点状态追加实际 model、reasoning_effort、角色、轮数、用量可观测性和升级/替代批准引用；实现与独立审查共用节点预算。

## 一轮 loop

1. `observe`：核工作区和父 SHA；读当前节点、直接依赖和真实代码；确认测试基线，写入 claim。不同人不共享可写工作区。
2. `plan`：子节点一般最多 3 个源/测试文件；大型原节点拆串行子节点，明确要搬的符号、实际路径、guard 清单。不能用通配符 scope 直接开工。自动生成的父节点 scope 只是规划边界。
3. `act`：机械提取，原 API/导出/事件名/文案保留。改测试只限跟随迁移或真实回归测试；不删除断言、放宽阈值或仅改快照接受新行为。
4. `verify`：先 fmt/目标测试，再相关组件 profile；保存真实 exit code、测试数量、warning 数和日志路径。代码或失败发生变化才重新跑相关检查，不反复运行已通过全套。
5. `review`：独立 reviewer 读取 immutable diff 和证据，重点查 `?`、return/continue、锁范围、Drop、await、事件顺序。复杂安全/迁移边界用第二人复核；不要求 Claude。
6. `persist`：写状态：base_sha/result_sha、scope、命令/退出码、review verdict、rounds、tokens_used（未知为 null）、下一步。成功合入后 done；需要人介入写 blocked，不自动推进依赖。

默认一次只做一个节点；同组件 mutex 的节点串行合入，尤其两侧各自 ratchet 文件。不同组件可并行写独立克隆，但每批最多两个 worker。集成者在当前批准分支最新 HEAD 验证，不从旧基线强推。提交仅精确文件，禁止 stash、reset --hard、add -A、目录级 restore、no-verify、force push 或删除他人分支。

## 确定性 harness

`bash docs/number1/tools/verify.sh <docs|engine|app|frontend|all>`，必须先有有效 Git 克隆及批准的基线 refs。它保留实际退出码，不把 grep 无 error 当成功。

- docs：图结构、SHA、补丁摘要及必需字段；不代表代码通过。
- engine：fmt、Clippy 全 targets、`cargo test -p myagent --locked --no-fail-fast`，包含 integration tests。
- app：macOS/Tauri 环境、真实 sidecar，fmt、Clippy、lib、全部测试编译和全部测试执行。bridge/replay 的 `MYAGENT_BIN` 指向刚构建的 engine。lib 2946、engine lib 1316 是源基线参考，不是硬编码目标；不允许无解释减少用例。
- frontend：typecheck、lint、format、npm test、build；Vitest 不能代替 typecheck。npm test 的文件门基线由维护者 CI 固定，不临时改 refs 哄绿。
- file-size：`python3 -I scripts/check_file_size.py --base <节点父SHA>` 对增量不增长；原 public main 门禁也必须通过。conventions 当前从 origin/main/master 取基线，公共启动时需核实参考 ref，不能偷偷切换至候选 HEAD。
- 单文件终态：`python3 docs/number1/tools/check_graph.py --target <path>`；不证明行为等价。每张子单未必把宿主一次拆完，但必须严格减少且新增文件不超上限。

现有 warning 不统一使用 `-D warnings`，先在相同参数/平台的父 SHA 比较。新增 warning 必须解决，不能加 allow 只压数字。若父 SHA 有已有失败，保留原输出、证明与 diff 的关系；一次针对性重跑绿也不能隐去最初失败。已知时序抖动：search_backfill、bodyCache、mcp spawn、agentic_loop；失败名字相同不等于原因相同。

## 守卫与迁移风险

- **源码切片**：全仓搜索被搬符号和 include_str，正向守卫证明真实生产入口调用核心；负向守卫覆盖 streaming 和 terminal 的完整范围。不要把整份 helper 无条件拼入正向断言。逐条注入被禁止调用，必须使断言失败；只编译失败不算变异证明。恢复后检查 diff 无实验残留。
- **L2B6**：四个 prespawn 块的 persist→note→emit→drain，以及正常 commit→finish→release→drain→drop 的顺序原样；J13 五步留在 `run_lead_runner` 体内。Completed/terminal 先暂存，不提前 push；usage 只计最后 Completed；guard 不提前 disarm/drop。slice tests 的 4 个文件必须全部随迁。
- **AppHandle**：现有某些运行时路径无法在普通测试线程创建真实 macOS EventLoop。不得编造“测试覆盖”；可提纯判定另立小测试节点，或由维护者提供主线程 harness。bridge/replay 复刻测试不等于调用新生产函数的运行时证明。
- **Rust 外移测试**：保留原模块层级，通常仍 `use super::*`。evaluator fixtures 路径可能需多一层 `../`；openai tests 既有 image_tests 不能覆盖。
- **Tauri command**：A0 先验证一个真实 command 的迁移/注册；不猜测宏导出行为，不改 generate_handler 名字、顺序、参数契约。源码 invoke guard 与实际编译都需过。
- **UI/mock**：原模块 ID、具名回导、vi.mock 路径保持；大 hook 逐个拆，先纯函数再状态再 JSX。公开缺远程消费者源码时，共享组件合并必须有维护者的远端 CI 证据。
- **GUI**：每张 UI/CSS 子单写实际点击路径。最低：启动桌面→打开会话/输入发送→流式更新→停止/错误态→展开附件/文件→切设置并保存/取消；按受影响组件裁剪。记录每次点击后可见结果，测试浏览器页面不能代替 Tauri 桌面。
- **CSS**：从头按完整规则切，不跨 @media 边界，不引入 @layer，不移动尾部 override。递归按 import 顺序拼回原字节比对；不要删所有空白后比较。逐步导入和最终 build/GUI 都验收。
- **DB/Git/权限**：逐字 SQL、事务边界、参数次序及 guard 生命周期不变。新越权写/路径访问设计不属于拆分任务。

## 状态示例

```json
{
  "id": "B2-commit-broker", "status": "review",
  "parent": "F04", "owner": "contributor-handle", "base_sha": "FULL_SHA",
  "result_sha": "FULL_SHA", "rounds": 1, "tokens_used": null,
  "allowed_files": ["app/src-tauri/src/commit_broker.rs", "app/src-tauri/src/commit_broker/tests.rs"],
  "verify": [{"command": "bash docs/number1/tools/verify.sh app", "exit_code": 0, "log": "evidence/B2-commit-broker/app.log"}],
  "review": {"verdict": "pending", "reviewer": null},
  "next": "独立审核后请求维护者合入任务分支"
}
```

日志可以不提交大文件，但要提供审阅者可访问的 CI/artifact 链接和摘要。成功标准要求证据存在，不能只写 tests pass。

## 历史问题与归属，避免接手后重复开工

- 已修：interactive resume 的 Fatal 错误曾被吞成 continue；已用 Fatal/Turn 区分并补测。不能为了 helper 统一返回值再次改变语义。
- 已修：member_runner、updater、solo_stream 源码守卫曾因迁移变成 fail-open；现在必须保护真实入口与完整终态范围。
- 已修：engine `run_turn` workspace-change 与即时诊断的顺序测试已经加入。不要重开“R6 缺顺序门禁”；只维护它随后续迁移有效。
- 待单独评估：legacy schema 升级永久夹具、单侧 token usage 落账、commit broker HEAD 漂移/用户取消、撤销设备拒绝、真实 AppHandle 的线程/终态接线覆盖。纯文件移动不自动扩张为全部补测；本次引入新的覆盖退化必须修。
- 波次 1 内部孤儿台账已经清零，公共新文档用本目录内部相对链接管理，不要求贡献者寻找未发布的内部台账。维护者回流时同步内部索引与进度。

## 跨仓回流

贡献者 PR 只针对维护者批准的 public 任务分支。维护者按变更路径挑选源码提交回流到主开发树，公共交接/CI 文件单独处理，不用一次 merge 把两套历史和文档树混在一起。每批回流重跑受影响组件及共享消费方验证，记录公共 result SHA 与回流 SHA 的映射。relay 使用其独立仓的路径与基线；两个仓的结果汇总到 CLOSE-ALL，不以 Git SHA 相同作为跨仓内容相同证明。

## 共享编译缓存的验收约束

若维护者复用现有 Cargo 缓存，用 `cargo test --target-dir <cache>` 等子命令选项指定目录，不把 `CARGO_TARGET_DIR` 导出给整套测试。测试会生成临时 crate 并启动 cargo check；继承共享目录会干扰诊断或造成锁等待。新贡献者用默认独立 target 即可。不得把缓存产生的首次失败隐去；修正环境后定向复验并保留两份日志。

公共验收也运行 `python3 -I scripts/test_file_size_gate.py`、`python3 -I scripts/test_check_conventions.py` 与 `python3 -I docs/number1/tools/test_ci_baseline.py`。公开环境测试实际 CI 入口，不依赖仅存在于源仓的导出工作流。Issue 模板已纳入注释扫描范围，不作为豁免。

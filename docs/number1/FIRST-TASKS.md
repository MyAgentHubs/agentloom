# 第一批可执行子单

前提：P0/P1 已完成，BOOT 的真实平台与基线检查成功。以下只分配一个子单给一个 worker；涉及共同 crate/ratchet 的合入串行。维护者先将这些子单写入 state/<id>.json 并绑定 parent，不直接把父 epic 交给 worker 一次改完。

所有子单遵循 [MODEL-POLICY.md](MODEL-POLICY.md)：以下预算已包含实现、独立审查和返工，不为每个角色重复分配。

## 恢复已完成工作

### REC-L2B6：先审查，不重新实现

```yaml
goal: 复核现有 lead runner 补丁，证明收尾/终态/锁语义保持后合入
scope:
  write:
    - app/src-tauri/src/lib.rs
    - app/src-tauri/src/lib/lead_session.rs
    - app/src-tauri/src/lib/lead_session/runner_thread.rs
    - app/src-tauri/src/lib/lead_session/stream_closeout.rs
    - app/src-tauri/src/lib/tests/delivery_order.rs
    - app/src-tauri/src/lib/tests/lead_terminal.rs
    - app/src-tauri/src/lib/tests/resume_pending.rs
    - app/src-tauri/src/lib/tests/run_closeout.rs
    - app/src-tauri/tests/clippy_allow_ratchet.rs
state: docs/number1/state/REC-L2B6.json
worker: {model: gpt-5.6-sol, reasoning_effort: medium, role: independent-reviewer}
verify:
  - git apply --check docs/number1/patches/L2B6.patch
  - bash docs/number1/tools/verify.sh app
  - 逐条给被迁移守卫注入禁止行为，记录断言失败与恢复后通过
budget: {max_rounds: 2, max_minutes: 60, max_tokens: 18000}
stop: {success: 审查通过并合入, no_progress: 缺运行环境或两轮无新证据}
escalate: [早退或清理顺序改变, guard 所有权不明, 需要修改 scope 外文件]
```

先核 SHA-256，再在独立节点分支 `git apply`；already applied 时核文件差异和提交，不重复应用。四个失败块不能只审 helper 本体，须审所有调用点确实经过它。J13 收尾在 `run_lead_runner`，不是原 command。预期 lib.rs whitelist `(1,0)`，`run` 例外保留；其他 cognitive 条目不得顺手删。

### REC-C1：复用三文件外移结果

scope 精确取 `patches/manifest.json` 中 C1 的 7 个文件：三宿主、三 tests.rs、engine 大小 ratchet。模型 Terra medium；最多 2 轮 / 45 分钟 / 12,000 tokens。先 `git apply --check docs/number1/patches/C1.patch`，再应用与 `verify.sh engine`。35/21/25 tests、71/32/45 assert 调用是历史对照，需在当前树核实。宿主应 538/443/527，新的测试文件 598/570/499。门禁与独立 diff 检查通过才合入。C1 完成即更新对应文件父节点，不能再创建一轮同样搬迁。

## W3-B2 拆成三张独立小单

### B2-commit-broker（parent F04）

```yaml
goal: 原内联测试整体外移，commit_broker.rs <=800，测试内容及生产行为不变
scope:
  write: [app/src-tauri/src/commit_broker.rs, app/src-tauri/src/commit_broker/tests.rs]
  read: [app/src-tauri/src/commit_broker.rs, app/src-tauri/tests/clippy_allow_ratchet.rs]
state: docs/number1/state/B2-commit-broker.json
worker: {model: gpt-5.6-terra, reasoning_effort: medium}
verify:
  - bash docs/number1/tools/verify.sh app
  - python3 docs/number1/tools/check_graph.py --target app/src-tauri/src/commit_broker.rs
budget: {max_rounds: 3, max_minutes: 60, max_tokens: 24000}
stop: {success: 宿主与新文件达标且复核通过, no_progress: 两轮无新证据或达到预算}
escalate: [需放宽可见性, 新 allow, 缺测试依赖]
```

仅保留原模块层级，不能因外部文件换成 super::super。分割点找 `#[cfg(test)] mod tests`，不要使用历史行号直接切字节。

### B2-continuation（parent F05；依赖 B2-commit-broker 合入）

同上述合同；scope 仅 `app/src-tauri/src/continuation.rs`、`app/src-tauri/src/continuation/tests.rs`。state 改 `B2-continuation.json`；target 命令改 continuation.rs；同样预算与 stop。搬完整测试块，保留平台 cfg 和私有 helper 可达性。

### B2-github（parent F09；依赖 B2-continuation 合入）

同上述合同；scope 仅 `app/src-tauri/src/github.rs`、`app/src-tauri/src/github/tests.rs`；可只读 `git_ops.rs`。state 改 `B2-github.json`；target 命令改 github.rs。原测试自读源码的 include_str 路径在新目录需核 `../github.rs`、`../git_ops.rs`。验收额外要求：向对应被保护生产范围插入被禁止调用，使原守卫真实变红，精确恢复后变绿。不允许为了过守卫删断言。

## W3-C2 拆成两张单

### C2-openai（parent F43；依赖 REC-C1）

scope：`harness-agent/src/provider/openai_compatible.rs`、新 `provider/openai_compatible/tests.rs`、`harness-agent/tests/file_size_ratchet.rs`；既有 `provider/openai_compatible/tests/image_tests.rs` 只读并保持挂载。goal：外移内联 tests，宿主 <=800 并删除对应大小 ratchet 条目。state：`state/C2-openai.json`；worker Terra medium；verify `verify.sh engine` 与 `check_graph.py --target harness-agent/src/provider/openai_compatible.rs`；预算 3 轮 / 60 分钟 / 24,000 tokens；stop 成功同上，路径冲突或新文件超限必须 escalate。

### C2-probe（parent F37；依赖 C2-openai 合入）

scope：`harness-agent/src/orchestrator/probe_runner.rs`、新 `orchestrator/probe_runner/tests.rs`、`harness-agent/tests/file_size_ratchet.rs`。goal：外移完整 tests、宿主 <=800、新文件同时满足两道大小门。state：`state/C2-probe.json`；worker Terra medium；verify engine profile 与目标检查；预算同上。若搬后恰 800 可按上限成功，但记录零余量；如果超过 800，不删空行凑数，先停止并提出一个精确独立 helper 的小子单（新文件和符号列清楚，由维护者确认）。

## 后续父节点怎样展开

不要执行“拆 lib.rs 全文件”这种任务。示例：lib.rs 父节点的 A0 只读所有 `#[tauri::command]`/invoke guard，选一个低风险、无共享状态的 command，提交明确符号及新路径。维护者确认后仅迁该 command、父模块 use、必要守卫；成功编译和命令名验证后再派 A1。DB 按表域、App.tsx 按 hook 域、CSS 按原序规则块，同样每个子节点写完整八字段合同。父节点 steps 提供切分顺序，不代替子节点 scope 审查。

所有子单还必须满足 EXECUTION.md 的独立审查、真实退出码、warning 不增、必要 GUI/远端消费者验证。没有完成证据时不把状态从 review 改 done。

# CLAUDE.md — AgentLoom

> 给在本仓工作的 Claude 会话 / agent 的项目说明。先读本页 + `GUIDELINES.zh.md`（编码与协作基线）+ `AGENTS.md`（贡献流程与门禁）。维护者可能另有仅本机生效的补充说明，与本页同时加载、只增不减。

## 这是什么

**AgentLoom** = 基于 Tauri 的**会话中心 (session-centric)** 多 LLM coding-agent IDE。统一管理多 GitHub repos（多 account / 多 org），内置多个 LLM agent（Claude / Codex / DeepSeek / Gemini / 本地…）协作。代码在 `app/`（React + TS 前端 / Rust 后端）与 `harness-agent/`（内置 Rust agent 引擎 `myagent`），已进入深度实现阶段；本文件不背进度账，当前进度以维护者的进度文档为准。

## 核心范式（不可回头）

**整个 App 围绕「会话 (session)」转，不围绕「仓库」。**

- repo 切换：低频入口，当前真相是左栏底「项目切换器」（向上 popover）；topbar 不再放 repo list。
- 左侧：当前 repo 的**会话列表**（高频）+ 项目简介菜单 + footer。
- 主区：一次专注一个会话，输入框落底，**无 tab**（左侧列表即切换器）。
- 右面板：Codex 风格，默认收起，tab = Files / Review / Terminal / Side chat（/ Browser）。

## 设计 DNA（当前、已校准）

- **会话拓扑 C′**（取代旧输入区三模式开关）：协作是会话级概念。Solo = 单 agent 会话；Team = 一个 lead + 若干成员；Discussion / Round Table 先灰占位、后续独立做。Solo / Team 的入口是 composer agent 选择器（皇冠设 lead + 成员 toggle），不是旧 `Agent Team` 模式 pill 或 TeamBar。
- **角色 vs 模型解耦**：role 是槽（队长/队员/主持/specialist），LLM 是填槽实例；头像 + role pill 两个独立维度，永不弹窗切换。
- **Agent 运行时** = (provider, model, 能力, 成本) profile（**role 不在 profile**，派单时才定）；DeepSeek「借壳」经 Claude Code 接入；派单池 = enabled agents + 能力标签挡（namespace 级白名单留后续——agents 表无 namespace 外键，旧设想已修正）；多账户 gh（namespace→账户映射 + commit 身份切换）。
- **产品/运行时状态隔离**（硬不变量·勿污染用户项目）：**AgentLoom 自身的状态与运行产物**（会话记忆 / 决策台账 / TaskPack / MemberResult / log / 会话状态 / worktree / 分支·ref / 临时文件）一律落 **app 域**（app 数据目录 DB + `~/.agentloom/` + 仅 `agentloom/*` 命名空间分支·收尾清理），**绝不写进用户 repo 工作树 / 不留非命名空间分支·ref / 不擅自 commit 用户分支**。
  **★ 划清边界（别再误读）**：这条守的是「**AgentLoom 别把自己的运行账本写进用户仓库**」。**agent 对用户项目源码的改动不在此列**——那正是产品要做的事（in-place：agent 直接在用户项目目录干活，像 Claude Code / Codex 一样）。「禁止写用户工作树」≠「禁止 agent 改代码」。与 worktree 隔离同族。**唯一明确例外**：会话**附件**（粘贴 / 拖入 / 对话框选入的文件）落 `<会话工作区>/.agentloom/attachments/` 并经 `.git/info/exclude` 忽略（不碰用户 `.gitignore`、非 git 目录不写）——附件是给 agent 的输入材料，与 Claude Code 把 worktree 放项目内 `.claude/worktrees/` 同款；journal / 日志 / DB / worktree 仍留 app 域。
- **终端** = 右面板 **Terminal tab**（不是「⌘\` 底部抽屉」）；外加 inline tool call card + pinned live process widget。
- **唯一搜索入口** = ⌘K 浮动面板。
- **多模态渲染**为一等公民：diff / thinking 折叠 / tool-command 卡片 / mermaid / 折叠默认 / 全屏分流。
- **i18n / 多语言**贯穿。MVP 只做 GitHub（GitLab 留接口，adapter pattern）。

## 视觉系统

- 暖米底 `#F5F2EC` + 暖橙 accent `#D97757` + **克制**（锚定 Claude Code / Codex desktop）。
- 线性 SVG 图标；**区段标题不用 emoji**（信息架构靠 typography，emoji 只用于真需要图像感处）。
- **折叠默认**（fold-default）：长内容/工具输出/thinking 默认折起，按需展开。

## 文档

设计原型、架构图、进度与规划文档由维护者另行管理，不在本仓公开树内；外部贡献以本文件、`AGENTS.md`、`GUIDELINES.zh.md` 为准。

## 工作约定

- **中文交流**；中文与英文/数字之间**留空格**。
- **URL 用纯文本**，不要包成 markdown 加粗（`**...**` 会渲染成乱码）。
- **少用 emoji**；区段标题完全不用 emoji。
- **找原型真相源：先看原型索引页的状态标记，别 grep 到文件名就信**。标记为「基准 / 已对齐 / 已 review」（含「当前真相」）= 当前可用；「历史 / 归档 / 已被 X 取代」= 勿用；被取代的原型已进 `_archive/`，路径含 `_archive/` 即勿用。
- **维护者明确 review 确认前不要标 ✅**；中间态用 🟡；不替维护者决定是否满意；不要抢跑下一步。
- **context / handoff 时机**：判断交接看**实际剩余 context size**，别把「会话聊得长 / 画了很多」误当「context 满了」。**剩余 30~40% 时才着手准备 handoff**（≈ 已用 60~70%）：落 files-first 真相源再交、不硬撑；剩余 >40% 继续干、不必交接。
- **沟通用大白话、不夹术语黑话**：中文内容里别夹 de-risk / dogfood / roll-up / backcompat / seam / gate 这类英文术语黑话。必须用某个概念时，先用中文把它讲清，需要时再括注原词（如「先把最硬的管子打通、提前排掉风险（de-risk）」）。文档/spec 里的术语可保留，但**对话表达**要让人不查词就懂。
- **【铁律】讲目标先大白话 + 对齐架构和原型（违反即偏航）**：每开一个 fresh session 写 plan / 写 worker prompt 前，**必须先用大白话、对照架构图落点 + 原型屏号把「本轮目标 + 验收标准」讲清楚**（指认架构哪层哪节点 partial/灰、对应原型哪屏的 DOM 形态、怎么算做完），讲清了再动手拆 task / 派单。日常沟通同此：简洁易懂、对齐架构设计和原型的心智模型、不黑话乱说。这是铁律。
- **代码改动分档**：lead（队长）任何代码改动都派 worker，文档/台账/夹具可直落。派 worker = 队长写自包含 brief（落点给到文件:行）派 sonnet 级 worker（codex 有额度时用 codex 级 worker），每单 6 件套复验（diff / 范围 / 门禁 / 对 spec / 用例名只增不减 / 变异证明）。**worker 硬约束**：只 `git add`/`restore` 自己亲手改的精确文件——严禁 `git restore <dir>` / `git add -A` / `git stash`、严禁无参 formatter、`docs/` 不碰、绝不 restore 别人的预存 WIP；并行只用独立 worktree；队长派单前先把主树未提交 WIP 临时 commit 固化。
- **审级阶梯**：默认一个 reviewer，第二个只在风险分级触发时上；agent 工程里测试 + 确定性检查是主力，独立验证一遍即可；每路 reviewer 约 10～20 万 token ≈ 再花一遍 worker 的钱。
  | 级 | 谁审 | 触发条件 |
  |---|---|---|
  | L0 | 只队长 6 件套 | 纯机械：注释清理、文件搬迁（用例只增不减）、CSS、文案、测试微调 |
  | L1 | sonnet 级 reviewer 单路 | 默认：一切逻辑改动、前端、薄接线 |
  | L2 | opus 级 reviewer 单路 | 碰边界（provider 出线 / 文件读写边界 / shell 扫描 / golden 路径）**且** worker 已交付真跑的边界测试 + 变异证明 |
  | L3 | opus + sonnet 双路（各自独立、都后台） | 仅三种：schema 迁移 / SQL 逻辑；不可逆或破坏性操作；并发锁。或 L2 抓到 P1 后升级 |
  省钱规则：reviewer **不重跑全量门禁**（worker 已贴、队长 bash 复核一次即可），只盯 brief 点名的边界维度，**每条 finding 必须带可复现证据**（业界数据：两个 reviewer 被要求出结论时会趋同却不一定对——「虚假一致」，arxiv 2608.18167）；L3 双路各自独立后，队长**显式对照两路分歧点**再裁定，不是简单取并集；L1 抓到非平凡问题才升级，不预设双路；review brief 落文件、reviewer 报告落文件、回主会话只给 ≤ 25 行摘要。
- **token 经济**（对照业界公开实践；来源：Anthropic「Effective context engineering for AI agents」「multi-agent research system」、Claude Code best-practices / costs 文档、Manus「Context Engineering」、arxiv 2608.18167）：① **KV 缓存命中率是第一指标**（Manus：命中价差 10 倍）——agent 定义 / brief 模板 / 系统提示保持前缀稳定，续做同一件事用 SendMessage 续原代理而非新开、不改写历史；② **实勘结论沉淀成事实卡**（链路 / 落点 / 行号 / 一句话结论），brief 直接引用，同一事实不派第二次 Explore（Anthropic 多智能体文：子代理无边界 → 重复探索）；③ **brief 给精确落点**（目标 / 输出格式 / 文件:行 / 边界 / 不做什么），产出文件的活派能写盘的代理（Explore 只读）；④ **门禁只跑一次**：worker 跑并贴原文 → 队长复核数字 → reviewer 不跑但每条 finding 带复现证据；开发期只跑定向测试，全量留收尾与合并后；⑤ **回报限长、长输出落文件**（Anthropic：子代理只回 1～2k token 的压缩摘要；hook 预过滤日志可把万级降到百级）——worker / reviewer ≤ 50～70 行；⑥ **读文件按范围**（`sed -n a,b` / head / tail，不整文件 cat）；**大文件就是 token 债务**，拆分是省钱投资；⑦ **模型分级路由**（Claude Code costs：简单子任务指定 haiku；路由可省 50～60%）——Explore / 机械单用 sonnet 或 haiku，opus 只留裁决与 L2 审；每单在台账记 `subagent_tokens`，按月复盘；⑧ **CLAUDE.md 瘦身**（best-practices：< 200 行，专门知识转 skill / 笔记按需加载）——本文件每个会话与子代理都要读，历史教训与长案例迁到笔记，这里只留规则；⑨ **循环预算三轴**（轮数 / token / 时间）+ **进展检测**：每轮结束核「有没有可衡量推进」，无推进换策略而不是耗完 3 轮；任一目标 3 轮不达标即停、写报告请维护者决策；⑩ 同类小任务合并**仅限弱耦合、验收可拆**的情况（业界无直接支持、且与原子任务纪律有张力）——默认仍按原子单。
- **subagent review 统一后台化**：跑交叉 review（plan-review / 6 件套交叉核）时，**两路都 `run_in_background:true`**（codex CLI 后台 + opus/sonnet Agent 后台），不前台阻塞、不主动轮询，等完成通知到齐再汇总裁定。省 token + 主 session 不卡。
- **每份对齐原型的 plan 必附原型设计文件路径**；**执行完成后必须逐项对照原型核「布局/结构保真」，不得主观认定「做完了」**。opus review 同样要**逐项回原型 DOM 核保真、不只跑测试绿**——保真错（Files-tab / 双 collapse / 顶栏未分段）都是「只对 spec + 测试绿、没回原型核 DOM」漏掉的。
- **plan 涉及 layout / topbar / 左右分栏 / 全局结构类改动时，opus reviewer 必加一步「起 Tauri GUI 肉眼检 layout」**（不是只 grep CSS + 跑测试 + 比 DOM 字符串）。理由：测试 + grep + 原型 DOM 比对验不出「合体 flex 容器只能整段隐」「sidebar 列宽与 topbar 段宽不对齐」「按钮撞红绿灯」这类 layout-only bug——只有真起 GUI 肉眼看才能发现。reviewer 有权在「不便起 GUI」时跳过这一步（如沙箱不能起 Tauri），但**必须在 review 报告里明说原因 + 提示维护者「未做 GUI empirical 验证、GUI 验收时重点核 layout」**。
- **调试 layout / CSS 溢出类 bug：先量、别猜**——截图是症状非测量·不验证前提不接受任何根因结论（自己的 or subagent 的）·横向溢出/flex 不收缩第一假设 = 某 flex 子项缺 `min-width:0`·最快确认 = devtools console 沿祖先链量宽·fix 没生效先验 dev server cwd（多 worktree 坑）。
- **每完成一步实现/计划就及时更新项目进度文档**，别攒着等被问；进度看板只放「我们到哪了」的摘要，长日志不要复制进去。
- **两线分治**：app 线（`app/`）与 cli/engine 线（`harness-agent/`）各管各的真相源；跨线改动先对齐边界，涉及 `harness-agent/CONTRACT.md`、`vocabulary.rs`、`harness-agent/src/plan/**` 或 `plan.*`/`agent.note.delta` 显示语义时先回传 engine 线确认。
- **lead+agents 过程基线**：① 队长把需求拆成**足够细的原子 task**（≤3 文件 · 一条可衡量 acceptance · TDD · 1 commit · 门禁全绿）；② **task 间复验闭环**——每 task 之间队长独立核 `diff / scope / 门禁重跑 / 对 spec / 测试断言强度` 再放行，不攒到最后。codex 级 worker / Agent Team 队长同此基线。这套（主 session=队长 + codex=队员）正是产品 Agent Team 模式的 CLI 级 dogfood。
- 远端：`origin` = github.com/MyAgentHubs/agentloom；主分支 `main`。
- **治理/优化先定真目标**（借鉴 Anthropic 爬坡 + 棘轮）：开工前写清真目标 / 代理指标 / 相关性怎么验证 / 用什么棘轮守；代理指标未验证不进 CI；排序按价值（影响 × 频率）不按规模。

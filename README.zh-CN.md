<div align="center">

# AgentLoom

**多个模型，一个工作台，跑在你自己机器上。**

Claude、Codex、DeepSeek、GLM…… 并排跑，也可以组队干活。
一个开源桌面工作台，把一堆大模型变成一支你说了算的队伍。

[下载](https://github.com/MyAgentHubs/agentloom/releases/latest) · [反馈问题](https://github.com/MyAgentHubs/agentloom/issues)

[![CI](https://github.com/MyAgentHubs/agentloom/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/MyAgentHubs/agentloom/actions/workflows/ci.yml)
[![License: AGPL v3](https://img.shields.io/badge/license-AGPL--3.0-blue.svg)](LICENSE)

[English](README.md) · **简体中文**

![AgentLoom — 队长把一个目标拆成三个任务，派给 Codex、DeepSeek、GLM 三个队员并行干活](docs/screenshots/team.png)

</div>

---

## 为什么是 AgentLoom

大多数 AI 编码工具给你的是：一个 agent、一个编辑器、一个目录、一家厂商。
这套够用 —— 直到活儿大过一个 agent、下午额度用光、
或者你想在留下改动前先看清它到底动了什么。

AgentLoom 的前提正好相反：很多项目、很多模型、好几个 agent 同时在干活，
全都在你自己的机器上，由你说了算。

- **一个 agent 就是瓶颈，那就上一队。** 给一个 agent 戴上皇冠当队长(Claude 或 Codex，它们擅长规划)，
  再配几个便宜模型当队员。队长拆解目标、并行派活、检查交回来的东西、把对不上的地方改掉。
  几个文件同时在推进，而不是一个一个来 —— 而贵价钱你只花在「想」这一步上。

- **不被任何一家卡住。** 下午 Claude 额度用完了？把这个会话切到 GLM、DeepSeek
  或者一个本地模型接着干 —— 同一段对话、同一份上下文，不用复制粘贴。
  换厂商是点一下下拉菜单，不是搬一次家。
  会话聊长了想开新的，AgentLoom 会替你写好交接说明，新会话直接接上。

- **便宜模型也能真把活干完。** AgentLoom 自带一个用 Rust 写的 agent 引擎 `myagent`，
  不装 Claude Code、Codex 或别家 CLI 也能跑。凡是通过这个引擎接入的厂商，都能用上
  跟贵价 CLI 同样的工具循环、plan 模式和 checkpoint —— 所以一把按量付费的 key，
  也有机会真干成事。
  <br>*这句话我们是测过的，不是自己说的 —— 见下面的[跑分](#跑分--便宜模型也能真干活)。*

- **每一步都看得见，每一步都能撤。** 每一条命令、每一次文件改动都是一张能展开的卡片，
  外加一个带文件级台账的 Review 面板。满意的留下，不满意的逐个文件退回去。
  不用你是终端老手才看得懂它干了什么 —— 也不用你闭着眼睛信它，才敢放它干活。

- **数据是你的。** 开源，本地优先。API key 存在你系统的钥匙串里，对话存在你磁盘上的数据库里，
  agent 直接在你自己的仓库里干活。**AgentLoom 不在互联网上运行服务端；它启动的监听器仅限本机回环辅助进程** ——
  离开你机器的只有两样：你自己选的那家模型厂商收到的调用；以及如果你开了联网搜索，
  发往你自己配置的那个搜索后端的查询。
  它自己的运行记录也从不写进你的工作树。

## 跑分 —— 便宜模型也能真干活

> ### 17 / 30 · 56.7%
>
> **SWE-bench Verified 的一个 30 题子集上，解决题数的中位数。** `myagent` 驱动
> `deepseek-v4-pro`，由官方 SWE-bench Docker 判分器评分。八次跑分，区间 16–19。

我们报的是中位数，不是最好的那一次 —— 这个子集单次跑分本身就有 ±3 题的噪声，
拿任何单次成绩(包括我们跑出过的 19/30)说事都会高估你实际能拿到的结果。

| | |
|---|---|
| 引擎 · 模型 | `myagent`(本仓库，Rust) · `deepseek-v4-pro`，temperature 0 |
| 判分 | 官方 `swebench.harness.run_evaluation`，Docker 内 |
| 测试泄漏 | **无。** 引擎拿到的是一个能跑的测试环境，但全程看不到 `FAIL_TO_PASS` 判分测试，运行期间也从不应用 `test.patch` |
| 跑分次数 | 8 次 · 中位数 17 · 均值 17.1 · 区间 16–19(53.3%–63.3%) |
| 关于这八次 | 它们横跨四天的引擎迭代 —— 是对一个移动目标的重复测量，不是同一版本的八次复测 |
| 成本 | 30 题跑完，模型花费约 $3–6 |

**拿去对比之前请先读这段**：这是一个手工挑出来的 30 题子集，**不是完整的 500 题** ——
它与公开的 SWE-bench Verified 排行榜不可直接比较，而且刻意排除了我们本机装不稳的
重 C 扩展仓库。前沿模型的 agent 在**完整**题集上是 60–70%+。这个数字要说明的是
「一个中档的按量付费模型，足够干日常的活」，而不是「我们赢过前沿 agent」。

确切的题目 ID 和完整方法(含各项局限)我们都公开：
[docs/benchmarks.md](docs/benchmarks.md) · [evals/swebench/fair30_ids.json](evals/swebench/fair30_ids.json)。
你如果拿同样这 30 题跑出明显不同的结果，欢迎开 issue 告诉我们。

## 长这样

| 聊到一半换模型接着干 | 图表与流程图内联渲染 | 卡片、review 与逐文件撤销 |
|---|---|---|
| ![问完 GLM，同一条会话交给 DeepSeek 接着答](docs/screenshots/switch.png) | ![会话里内联渲染出的 mermaid 时序图](docs/screenshots/hero-main.png) | ![工具调用卡片 + Review 面板逐文件 diff](docs/screenshots/review.png) |

*会话是干活的基本单位：一段专注的对话，产出代码，带 checkpoint 和撤销。
左栏放着你所有的项目、以及每个项目下面的会话 —— 不用开一排标签页。*

## 功能

- **Agent team** —— 跨厂商配置任意多个 agent(Claude Code、Codex 这类原生 CLI，
  以及 DeepSeek、GLM、Kimi 这类走内置引擎的);设队长、勾队员、派活、看结果回来。
- **以会话为中心** —— 所有项目在一个窗口里(GitHub 仓库和普通本地目录都行)，
  每个项目有自己的会话列表、分组、⌘K 搜索和会话交接。
- **Checkpoint 与撤销** —— 文件级的写入台账，可 review、可挑着撤。
  决定要不要留下之前，先看清楚 agent 到底动了什么。
- **富文本会话渲染** —— mermaid 图、内联图片、diff、可折叠的思考过程、工具调用卡片;
  长输出默认折起，对话不会被刷屏。
- **内置 agent 引擎(`myagent`)** —— 一个 Rust 写的 harness，跑与厂商无关的 agent 循环，
  支持工具调用、plan 模式、checkpoint 和事件流。可以单独当命令行用，也可以交给 AgentLoom 驱动。
- **给每个模型配上联网搜索** —— 不是每个模型都自带搜索;
  AgentLoom 接了第三方后端(DuckDuckGo 零配置，Brave / Exa 用你自己的 key)，让任何 agent 都能查资料。
- **什么都能自己接** —— OpenAI 兼容、Anthropic 兼容接口，自定义 base URL，本地模型。
- **多语言** —— 界面目前支持英文和简体中文，后续会加。
- **目前只有 macOS** —— Apple 芯片和 Intel，已签名并公证。Windows 还在做：app 后端在
  Windows 上还编译不过，发不出来的平台我们不会先写上。Linux 尚未开始。

## 安装

- **macOS** —— 到 [Releases](https://github.com/MyAgentHubs/agentloom/releases/latest) 页
  按你的芯片拿 `.dmg`(Apple 芯片或 Intel)，拖进「应用程序」。已由 Apple 签名并公证，
  打开不会有任何安全警告。
- **Windows** —— 还没有，而且从源码构建也不行：app 后端目前在 Windows 上编译不过。
  `myagent` 引擎本身能编，可以单独当命令行工具用。Windows 支持在路线图上，需要的话
  开个 issue 说一声。

## 从源码构建

前置：Rust(stable)、Node.js ≥ 20、npm。
请安装当前平台的 Tauri 系统依赖：https://tauri.app/start/prerequisites/

```bash
# 1. 编译 myagent 引擎
cd harness-agent
cargo build --release

# 2. 把它放到应用期望的位置
#    下列命令会自动检测平台 target triple
mkdir -p ../app/src-tauri/binaries
cp target/release/myagent ../app/src-tauri/binaries/myagent-$(rustc -vV | sed -n 's/^host: //p')

# 3. 构建 / 运行应用
cd ../app
npm install
npm run tauri dev      # 开发
npm run tauri build    # 打包
```

## 目录结构

```
app/            Tauri 桌面应用(React + TypeScript 前端 / Rust 后端)
harness-agent/  myagent —— 与厂商无关的 agent 引擎 CLI(Rust)
docs/           跑分方法论、截图
evals/          公开跑分题目清单
.github/        issue、PR 模板与 CI 工作流
AGENTS.md       AI agent 贡献规则
```

## 路线图(简版)

- Windows 支持：先让 app 后端在 Windows 上编得过，再出签名安装包
- 接更多厂商，本地模型支持做深
- 更丰富的 agent team 协作模式(讨论 / 圆桌)
- 沿现有 adapter 接口支持 GitLab

## 参与贡献

欢迎提 issue、报 bug、发 PR —— 怎么构建、怎么测、怎么提交见 [CONTRIBUTING.md](CONTRIBUTING.md)。
第一次贡献时需要签一份很轻的 CLA，在 PR 里点一下就好。

## 许可

[AGPL-3.0](LICENSE)。一句话：随便用、随便自建、随便 fork ——
但如果你分发改过的版本、或者拿它对外提供服务，你的改动也得开源。
这样这个工作台对所有人都是公平的。

**AgentLoom** 和 **MyAgentHubs** 的名称与 logo 是 MyAgentHubs 的商标，
**不在**代码许可范围内 —— 见 [TRADEMARK.md](TRADEMARK.md)。

## 联系

- panda@myagenthubs.com

© 2026 MyAgentHubs

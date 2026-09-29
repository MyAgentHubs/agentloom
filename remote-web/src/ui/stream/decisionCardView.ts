// decisionCardView.ts — T6f2 差量返工 · 把 `MilestoneProjection.decisionCards` 折算成
// `@app` 的 `RunLeadTurn` 能直接吃的 `LeadTurnView[]`。
//
// 依据：审查返工 item 2「只读卡片呈现」——DecisionCard（含"交付确认"，M0 协议里两者共用同一对
// `card.created`/`card.resolved` 帧，`kind` 字段区分 "ask"/"dispatch_confirm"，不是两条独立 wire
// 路径，见 `remote-relay/fixtures` 与 `M0` 协议 §2）此前在流屏被整个丢弃——`MessageContent.tsx`
// 自己对 `decision_card` 类型的 block 固定 `return null`（"决策卡经 lead-turn 路径渲·不走 raw
// block 循环"，见该文件路由表），意味着只喂 `msg.completed` 的 blocks 给 `MessageContent` 永远
// 看不到决策卡——必须走桌面自己的 `RunLeadTurn` 链路。
//
// **不用 `@app/lib/leadTurns.ts::buildLeadTurns`**：那个函数要 `ChatMessage[]`（`.id`/
// `.agent_name_snapshot`/`.content` 字段）+ `liveRunsByRun`/`liveCodingByRun`（桌面自己的 team_run/
// coding_task 实时状态跟踪），这些在 C1 的数据模型里都不存在、也不该在这一单里现造一套（`app/` 不
// 能改，桌面这套分组逻辑的输入形状对 C1 来说是过度耦合）。但 `buildLeadTurns` 自己的输出类型
// `LeadTurnView` 允许"只有 decisionCards、其余字段留空/置 null"这种退化形态（`buildLeadTurns` 源码
// 自己的过滤条件：`members.length===0 && !codingTask && !verdict && decisionCards.length===0` 才
// 整条丢弃——反过来说只要 decisionCards 非空，`members:[]`/`codingTask:null`/`verdict:null` 全留空
// 一样合法）。本文件直接手工构造这个退化形态的 `LeadTurnView`，不经 `buildLeadTurns`。
//
// **chooser 传空 = 天然禁用**：`RunLeadTurn` 把 `onDecisionChoose` 转手交给 `DecisionCard` 的
// `onChoose`；`DecisionCard.tsx` 自己的 `disabled = block.status === "submitting" || !onChoose`——
// 不传 `onDecisionChoose` 时全部 disabled，不需要额外发明"只读遮罩"这层（跟 Approval/ScopeChange
// 不一样，那两个组件没有内建的 disabled 机制，见 `SessionStreamScreen.tsx` 里另一套"pointer-events
// 禁 + 视觉禁用 + 提示文案"处理）。

import type { LeadTurnView } from "@app/lib/leadTurns";
import type { SessionStreamDecisionCard } from "./streamSource.ts";

type DecisionCardStatus = "pending" | "chosen" | "submitting" | "failed";
type DecisionCardKind = "ask" | "dispatch_confirm";

const VALID_STATUSES: ReadonlySet<string> = new Set(["pending", "chosen", "submitting", "failed"]);

/** `@app/types/agent` 的 `Extract<Block, {type:"decision_card"}>` 的字面量镜像——本文件不 import
 *  `@app/types/agent` 的运行时值（那是纯类型文件，import type 零成本，但字段名/形状钉在这里方便
 *  一眼核对，不用跳去另一个文件对表）。 */
export interface RenderableDecisionCard {
  type: "decision_card";
  decision_id: string;
  kind: DecisionCardKind;
  question: string;
  options: string[];
  recommended: string | null;
  rationale: string | null;
  payload: unknown | null;
  source_run_id: string;
  status: DecisionCardStatus;
  chosen_option: string | null;
  created_at: number;
}

/**
 * 防御式提取——`raw`（`MilestoneProjection` 里存的 `card.block`）来自 wire，未经深层 schema 校验
 * （同 `parseFrame.ts`/`milestoneProjection.ts` 的既有范围边界）。缺关键字段（`decision_id`/
 * `question`/`options`/`source_run_id`）判"这张卡当前没法渲染"，返回 `null`，调用方跳过——不
 * 崩溃、不硬造假数据。
 */
export function coerceDecisionCardBlock(raw: Record<string, unknown>): RenderableDecisionCard | null {
  const decisionId = raw.decision_id;
  const question = raw.question;
  const options = raw.options;
  const sourceRunId = raw.source_run_id;
  if (
    typeof decisionId !== "string" ||
    typeof question !== "string" ||
    !Array.isArray(options) ||
    !options.every((opt) => typeof opt === "string") ||
    typeof sourceRunId !== "string"
  ) {
    return null;
  }

  const status = typeof raw.status === "string" && VALID_STATUSES.has(raw.status) ? (raw.status as DecisionCardStatus) : "pending";
  const kind = raw.kind === "dispatch_confirm" ? "dispatch_confirm" : "ask";

  return {
    type: "decision_card",
    decision_id: decisionId,
    kind,
    question,
    options,
    recommended: typeof raw.recommended === "string" ? raw.recommended : null,
    rationale: typeof raw.rationale === "string" ? raw.rationale : null,
    payload: raw.payload ?? null,
    source_run_id: sourceRunId,
    status,
    chosen_option: typeof raw.chosen_option === "string" ? raw.chosen_option : null,
    created_at: typeof raw.created_at === "number" ? raw.created_at : 0,
  };
}

/**
 * T6f3 · 答卡激活——本机刚发出的 `input.answer` 在服务器 `card.resolved` 真正落地前的本地展示态
 * （`app/commandChannel.ts::getAnswerOverride()` 的返回类型镜像，本文件不 import 那个模块——保持
 * `decisionCardView.ts` 对 `commandChannel.ts` 零依赖，纯函数只吃调用方传来的 `Map`）。`status` **只有
 * "submitting"/"failed" 两种**，从不是 "chosen"——赢家永远由服务器 `card.resolved` 揭晓，本层不
 * 本地臆断（见 `groupDecisionCardsIntoTurns` 下方合并逻辑的取舍说明）。
 *
 * **返工②第②点新增 `option` 字段**：本机真正点的那个选项，覆盖合并时连同 `chosen_option` 一起
 * 写——不这样做的话，`DecisionCard.tsx`（`app/src/components/DecisionCard.tsx`，本文件不可改）自带
 * 的失败重试按钮会 `onChoose(decision_id, block.chosen_option ?? block.options[0])`：服务器端
 * `chosen_option` 在真正 resolve 前恒为 `null`（还没人真正答完，这本来就是"submitting/failed 覆盖
 * 只在 pending 时生效"这条判断成立的前提），于是重试会静默退回 `options[0]`——如果本机原本点的不是
 * 第一个选项，这是一个会发错指令的真实 bug，不是无害的显示瑕疵。
 */
export interface LocalAnswerOverride {
  status: "submitting" | "failed";
  option: string;
}

/**
 * 按 `source_run_id` 分组、构造退化形态的 `LeadTurnView[]`（见文件头注）——每个 run 一个 turn，
 * `lead: null`（`RunLeadTurn` 自己会落回 `t("runLeadTurn.fallbackLeadName")` 兜底文案，不是空白）、
 * `members: []`、`codingTask: null`、`verdict: null`、`phase: "live"`（没有终态判据，只读呈现不需要
 * 区分 terminal/live）、`showProcessFold: false`（没有 members/codingTask 可折，这个开关本就没
 * 意义）。分组顺序 = 卡片数组自身顺序（调用方——`streamSource.ts::deriveSessionStreamProps`——已经
 * 按 `MilestoneProjection.decisionCards` 的 Map 插入顺序给出，即卡片到达顺序，天然是时间序）。
 */
export function groupDecisionCardsIntoTurns(
  cards: SessionStreamDecisionCard[],
  localOverrides?: ReadonlyMap<string, LocalAnswerOverride>,
): LeadTurnView[] {
  const byRun = new Map<string, RenderableDecisionCard[]>();
  const runOrder: string[] = [];

  for (const card of cards) {
    // block undefined = 只见过 card.resolved、没见过 card.created（见 SessionStreamDecisionCard
    // 头注的已知缺口记档）——没有可渲染的卡片内容，跳过。
    if (!card.block) continue;
    let coerced = coerceDecisionCardBlock(card.block);
    if (!coerced) continue;
    // T6f3：本地覆盖只在服务器状态仍是 "pending"（还没人真正答完）时生效——一旦服务器
    // `card.resolved` 到达、状态翻成别的值（不论赢家是不是本机），覆盖值天然被这个判断挡在外面，
    // 不需要另外一步"清除本地覆盖"的动作（`app/commandChannel.ts::getAnswerOverride` 头注同款
    // 取舍说明）。这正是"CAS 输家显示赢家由 card.resolved 自然到达·不本地臆断"的落地方式。
    // 返工②第②点：同时覆盖 `chosen_option`（见 `LocalAnswerOverride` 类型注释——不这样做，
    // `DecisionCard.tsx` 自带的失败重试按钮会退回 `options[0]`）。
    const override = localOverrides?.get(coerced.decision_id);
    if (override && coerced.status === "pending") {
      coerced = { ...coerced, status: override.status, chosen_option: override.option };
    }
    let list = byRun.get(coerced.source_run_id);
    if (!list) {
      list = [];
      byRun.set(coerced.source_run_id, list);
      runOrder.push(coerced.source_run_id);
    }
    list.push(coerced);
  }

  return runOrder.map((runId) => {
    const decisionCards = byRun.get(runId) ?? [];
    return {
      kind: "run",
      runId,
      lead: null,
      codingTask: null,
      // LeadTurnView 的 decisionCards 字段类型来自 @app/types/agent 的判别式联合；本文件的
      // RenderableDecisionCard 逐字段镜像该形状（含 `type: "decision_card"` 判别式字面量），
      // 结构等价，这里做一次显式类型断言把"两个不同源文件里结构相同的类型"接起来。
      decisionCards: decisionCards as unknown as LeadTurnView["decisionCards"],
      members: [],
      verdict: null,
      phase: "live",
      outcome: "running",
      showProcessFold: false,
    } satisfies LeadTurnView;
  });
}

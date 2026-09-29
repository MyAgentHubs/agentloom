// ackOutcome.ts — `input.ack` 的 `outcome` 兜底规范化（M0 v1.8.11 ③ 记档）。
//
// relay 对桌面发出的 `input.ack` `outcome` 字段零校验、原样转发——`remote-relay/src/room-do.js`
// 的 `input.ack` 分支只看 `command_id` 是否为字符串来决定删不删 `pending_input` 行，`outcome`
// 取值完全不影响转发（DP-1 fixture coverage 表 `input.ack` 项的 gap 记档原文：「relay 对 outcome
// 字段零校验，只要 command_id 是字符串就无条件转发」）。桌面理论上只会发三个合法值
// （ok/queued/failed，M0 §3 v1.7.3 ack 映射表），但协议本身没把它收紧成 relay 强制的白名单，
// 远端必须对"不认识的 outcome 字符串"有兜底，而不是崩溃或误判成更糟的状态。
//
// **未知值不得冒充 "ok"（审查返工·2026-08 校准）**：早前版本把未知值的 `effective` 兜底成
// `"ok"`，理由是"三个合法值都意味着桌面已持久接管"——但这个推理只证明了"未知值大概率不是
// `ok`/`queued`/`failed` 里那种精确语义"，不能反过来证明"所以按 `ok` 显示是安全的"：`effective:
// "ok"` 这个值本身就是"指令已成功执行"的强承诺（消费方/UI 完全可能据此把这条指令从"进行中"划
// 掉、当作确定成功处理），而我们其实**不知道**桌面葫芦里卖的是什么药——用一个更强的已知态去
// 冒充一个未知态，是把不确定性伪装成确定性，比"显示态不够精确"更危险。
//
// 正确兜底 = 给一个**独立的中性态** `"taken_over"`（不等于 `KnownAckOutcome` 任何一员）：
// 语义只到"桌面已经把这条指令从 relay 的待发队列里接管走了"这一步为止（这一步确实是三个合法值
// 的共同交集——ok/queued/failed 都意味着 relay 会删掉这行 pending_input），但不冒充"结果是
// 成功还是失败还是仍在排队"这类我们没有证据支持的具体结论。`recognized:false` 仍然保留，供 UI
// 决定要不要额外提示"协议 outcome 未知"。
//
// **fixture 边界（如实）**：data-plane-v1.json 的 `input.ack` 两张样张只覆盖
// "outcome:ok + 合法 command_id"与"缺 command_id"——fixture 自己的 coverage 注释明确说明
// "『未知 status』反样张未出：…按字面造『未知 status → 不删行』反样张会与生产行为不符（现状是
// 仍会删），故遵铁律 4 不出"。所以下面这个函数没有 DP-1 真样张可消费，是本单按 M0 v1.8.11 ③
// 条文自行编写的防御性实现——`ackOutcome.test.ts` 的测试用例是自造的，不是 fixture 驱动。

const KNOWN_OUTCOMES = ["ok", "queued", "failed"] as const;
export type KnownAckOutcome = (typeof KNOWN_OUTCOMES)[number];

/** 未知 outcome 的中性兜底态——不是三个合法值之一，故意与它们区分开，见文件顶注。 */
export const TAKEN_OVER = "taken_over" as const;
export type AckDisplayOutcome = KnownAckOutcome | typeof TAKEN_OVER;

export interface NormalizedAckOutcome {
  /** 原始字符串，未经改动——供日志/调试用，不做显示语义判断。 */
  raw: string;
  /** true = outcome 命中三个合法值之一；false = 未知值，`effective` 是中性兜底。 */
  recognized: boolean;
  /**
   * 显示语义：`recognized` 时就是 `raw` 本身（`ok`/`queued`/`failed` 各自的精确语义）；未知时
   * 兜底为独立的 `"taken_over"`（"桌面已接管·结果未知"——只到"这条指令已经从 relay 待发队列里
   * 被接管走"为止，不冒充成功/失败/仍排队里任何一个具体结论）。
   */
  effective: AckDisplayOutcome;
}

export function normalizeInputAckOutcome(raw: string): NormalizedAckOutcome {
  if ((KNOWN_OUTCOMES as readonly string[]).includes(raw)) {
    return { raw, recognized: true, effective: raw as KnownAckOutcome };
  }
  return { raw, recognized: false, effective: TAKEN_OVER };
}

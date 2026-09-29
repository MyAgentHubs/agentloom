// activitySummary.ts — msgfix2 U3 · L1 活动摘要 chip 的分级判定（M0 §10.11 / 设计稿 v4.1 §4.1）。
//
// **单点定义**（brief §3a "L0 保护：……分级判定函数单点定义 + 反向测试锁死"）——
// `SessionStreamScreen.tsx` 的 `MessageRow` 只调用这里的 `extractActivitySummary()` 来决定"这条
// 消息该不该折叠成一行 chip"，不会在别处另开一份重复的判定逻辑。
//
// **L0 保护**：approval/decision_card/scope_change 是需要用户行动的块——即便某条消息的 blocks
// 数组里同时出现这类块和一个 activity_summary 形状的块（协议不会这样发：§10.11 明确"blocks 数组
// 唯一元素是 activity_summary 块"——这里防的是这条不变量未来被放宽、或上游 bug 把两类块混进同一条
// 消息这类"不该发生但纵深防御"的情形，同本仓 `milestoneProjection.ts` 头注一贯的取向），也绝不当作
// 可折叠摘要处理——`extractActivitySummary()` 对这种混杂输入恒返回 `null`，调用方据此回落到正常的
// `MessageContent` 渲染路径（`SessionStreamScreen.tsx::hasRestrictedBlock` 的只读遮罩仍然照常
// 生效，两条判定各自独立，互不依赖）。
//
// **不承诺 per-tool 逐工具明细**：`activity_summary` 块（M0 §10.11）只带聚合计数
// （tool_calls/mcp_calls/permission_prompts/failed）与一个 `state` 字符串——协议明文"不支持
// `msg.fetch` 定向拉取明细……判过度设计不做"。这里"展开"能给出的最细粒度就是按这四个既有计数
// 分类别列出，不是逐条工具调用的原文。

export interface ActivitySummaryBlock {
  type: "activity_summary";
  run_id: string;
  tool_calls: number;
  failed: number;
  mcp_calls: number;
  permission_prompts: number;
  state: string;
}

/** 需要用户行动的块类型——同 `SessionStreamScreen.tsx::hasRestrictedBlock` 判定的是同一份协议
 *  类型集合，但两处各自独立定义（不共享一个符号）：那边判定"要不要包只读遮罩"，这里判定"要不要
 *  折叠成摘要 chip"，是两个不同的问题，即便集合恰好相同也不该强行耦合——未来两者的类型集合完全
 *  可能分道扬镳。 */
const ACTIONABLE_BLOCK_TYPES = new Set(["approval", "decision_card", "scope_change"]);

function isActionableBlock(block: unknown): boolean {
  if (typeof block !== "object" || block === null) return false;
  const type = (block as { type?: unknown }).type;
  return typeof type === "string" && ACTIONABLE_BLOCK_TYPES.has(type);
}

/**
 * 识别一条消息的 `blocks` 是不是一条 L1 活动摘要——返回解析出的摘要字段供渲染层使用；不是就返回
 * `null`（形状不对/字段类型不对/掺了 actionable 块……全部一视同仁回落到正常渲染路径，不崩溃）。
 */
export function extractActivitySummary(blocks: unknown[]): ActivitySummaryBlock | null {
  if (blocks.some(isActionableBlock)) return null; // L0 保护——见文件头注，恒最先判定。
  if (blocks.length !== 1) return null; // M0 §10.11："blocks 数组唯一元素"。
  const block = blocks[0];
  if (typeof block !== "object" || block === null) return null;
  const b = block as Record<string, unknown>;
  if (b.type !== "activity_summary") return null;
  if (typeof b.run_id !== "string") return null;
  if (typeof b.tool_calls !== "number") return null;
  if (typeof b.failed !== "number") return null;
  if (typeof b.mcp_calls !== "number") return null;
  if (typeof b.permission_prompts !== "number") return null;
  if (typeof b.state !== "string") return null;
  return {
    type: "activity_summary",
    run_id: b.run_id,
    tool_calls: b.tool_calls,
    failed: b.failed,
    mcp_calls: b.mcp_calls,
    permission_prompts: b.permission_prompts,
    state: b.state,
  };
}

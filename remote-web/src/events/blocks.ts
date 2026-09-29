// blocks.ts — 三种由 live delta 归约"新造"出来的块形状（与桌面 db::Block 的 Text/Thinking/Tool
// 变体 JSON 同构，tag="type" snake_case——app/src-tauri/src/db.rs:54-75 `pub enum Block`）。
//
// **范围边界（有意收窄）**：这三个形状是本层"接续归约"（liveReducer.ts）唯一需要精确构造的
// 类型——因为它们是从零拼出来的新内容。`msg.completed`/`card.created`/`snapshot.partial_msg`
// 里收到的是桌面已经拼好的**成形**块（含 DecisionCard/RunCard/TeamRun/DispatchCard/LeadSummary
// 等本文件不认识的类型），那些在 parseFrame.ts 里一律按 `unknown[]` 透传——深挖每个块内部的
// 精确 schema 是桌面 serde 的权威（db.rs 的 `Block` enum），渲染层的事，不是本单（C1 事件内核）
// 的范围。

export type ToolCardKind = "command" | "compact";
export type ToolStatus = "running" | "ok" | "failed" | "interrupted";

export interface TextBlock {
  type: "text";
  text: string;
}

export interface ThinkingBlock {
  type: "thinking";
  text: string;
}

export interface ToolBlock {
  type: "tool";
  id: string;
  tool: string;
  summary: string;
  card: ToolCardKind;
  status: ToolStatus;
  exit_code: number | null;
  output: string | null;
}

export type ReducedBlock = TextBlock | ThinkingBlock | ToolBlock;

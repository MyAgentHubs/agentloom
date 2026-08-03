import type { Block } from "../types/agent";

export type ToolBlock = Extract<Block, { type: "tool" }>;

// 连续「成功完成」(status==="ok") 的工具卡一律折叠成组；失败/运行中/非工具块
// 会打断分组。失败/运行中卡永远单独成 block，不会被折进组
// （F2：agent-team-runtime-lead-centric.html「执行了 N 步」形态）。
const FOLD_THRESHOLD = 1;

export type StreamItem =
  | { kind: "block"; block: Block }
  | { kind: "toolgroup"; blocks: ToolBlock[]; isLatest: boolean };

// 队长编排/内部工具：通用「工具运行卡」对它们是错抽象（architecture-v2「ToolSearch/finish 隐藏」·
// 两条通道：输出渲染 / 能力工具）。dispatch_worker → 任务条；ask_user/propose_verifier → 决策卡；
// finish/ToolSearch/memory_* → 内部管线不渲。
// 用前缀语义判（与后端归约器 display_reduce.rs::is_hidden_orchestration_tool 同款）：
// mcp__agentloom__ 下全部是编排/能力工具，逐名单枚举会漏新增工具（如 memory_set_extra）。
//
// F1 例外（2026-07-25）：交付四件套（commit/push/create_pr/publish）是用户真正关心的
// 「发生了什么」，从隐藏名单里拎出来显示（人话映射见 lib/toolLabel.ts）；ToolSearch 与其余
// mcp__agentloom__ 编排工具（ask_user/finish/memory_* 等）继续隐藏。
const DELIVERY_TOOLS: ReadonlySet<string> = new Set([
  "mcp__agentloom__commit",
  "mcp__agentloom__push",
  "mcp__agentloom__create_pr",
  "mcp__agentloom__publish",
]);

export function isHiddenTool(tool: string): boolean {
  if (DELIVERY_TOOLS.has(tool)) return false;
  return tool === "ToolSearch" || tool.startsWith("mcp__agentloom__");
}

// F2：旧噪声折叠（低价值命令白名单）被统一分组取代——不再区分「低价值命令」
// vs「有意义命令」，一律按「连续成功完成」分组。
export function groupToolBlocks(blocks: Block[]): StreamItem[] {
  const items: StreamItem[] = [];
  let bucket: ToolBlock[] = [];
  const flush = () => {
    if (bucket.length >= FOLD_THRESHOLD) {
      items.push({ kind: "toolgroup", blocks: bucket, isLatest: false });
    } else {
      for (const block of bucket) items.push({ kind: "block", block });
    }
    bucket = [];
  };
  for (const block of blocks) {
    if (block.type === "tool" && isHiddenTool(block.tool)) continue;
    if (block.type === "tool" && block.status === "ok") {
      bucket.push(block);
      continue;
    }
    flush();
    items.push({ kind: "block", block });
  }
  flush();
  for (let i = items.length - 1; i >= 0; i -= 1) {
    const item = items[i];
    if (item.kind !== "toolgroup") continue;
    item.isLatest = true;
    break;
  }
  return items;
}

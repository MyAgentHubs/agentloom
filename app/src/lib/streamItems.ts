import type { Block } from "../types/agent";
import { collectImageArtifacts } from "./imageArtifacts";

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

// ─────────────────────────────────────────────────────────────────────────
// V1（2026-08-26·刀②「桌面 chat verbose 分级」）：blockTier / foldByVerbosity。
// 设计稿 desktop-verbose-design §2A/§2B
// ─────────────────────────────────────────────────────────────────────────

export type Verbosity = "full" | "summary" | "minimal";

// 过程块 = thinking / tool / 已处理完的 approval；pending approval 仍是 L0，确保
// 等用户操作的卡片任何档位都不被藏。其余全部 L0（未知类型默认 L0——宁多显不误藏，
// 与后端「分类错误不得落入可隐藏层」同原则）。
export function blockTier(block: Block): "process" | "l0" {
  if (block.type === "thinking" || block.type === "tool") return "process";
  if (block.type === "approval" && block.status !== "pending") {
    return "process";
  }
  return "l0";
}

export type ActivityCounts = {
  tools: number;
  failed: number;
  interrupted: number;
  rejected: number;
  thinking: number;
};

export type Segment =
  | { kind: "pass"; blocks: Block[]; sourceStartIndex: number }
  | {
      kind: "activity_fold";
      blocks: Block[];
      sourceStartIndex: number;
      counts: ActivityCounts;
      live?: { tool: string; summary: string } | null;
    }
  | { kind: "artifacts"; imagePaths: string[]; sourceStartIndex: number };

function computeCounts(blocks: Block[]): ActivityCounts {
  const counts: ActivityCounts = {
    tools: 0,
    failed: 0,
    interrupted: 0,
    rejected: 0,
    thinking: 0,
  };
  for (const block of blocks) {
    if (block.type === "tool") {
      counts.tools += 1;
      if (block.status === "failed") counts.failed += 1;
      if (block.status === "interrupted") counts.interrupted += 1;
    } else if (block.type === "thinking") {
      counts.thinking += 1;
    } else if (
      block.type === "approval" &&
      (block.status === "rejected" || block.status === "cancelled")
    ) {
      counts.rejected += 1;
    }
  }
  return counts;
}

type VisibleItem = { block: Block; index: number };

// 连续同层（process/l0）块切成一段，段内保留原始 blocks 下标（供 sourceStartIndex）。
function splitByTier(
  visible: VisibleItem[],
): { tier: "process" | "l0"; items: VisibleItem[] }[] {
  const segments: { tier: "process" | "l0"; items: VisibleItem[] }[] = [];
  for (const item of visible) {
    const tier = blockTier(item.block);
    const last = segments[segments.length - 1];
    if (last && last.tier === tier) {
      last.items.push(item);
    } else {
      segments.push({ tier, items: [item] });
    }
  }
  return segments;
}

// full 档：恒返回单个 pass 段（含空 blocks 时也是单个空 pass 段），图片继续走
// MessageContent 现状路径渲染、不生成 artifacts 段——现状路径零变化。
// summary/minimal 档：先过 isHiddenTool（命中的块不计数、不进段）；对剩余可见块
// 整条消息跑一次 collectImageArtifacts 抽图去重，按首现 tool 块所属的过程段分配
// 出一个紧随其后的 artifacts 段（不依附 fold，过程段被丢弃/收窄时 artifacts 段仍在）；
// 再按 blockTier 切连续段：l0 → pass；process → summary 恒 fold，minimal 恒丢弃
// 除非 failed/interrupted/rejected/cancelled>0（只留这几类块）；streaming 为真时最后一个过程段
// （不论档位、不论有没有失败块）整段保留为 fold 并带 live。
export function foldByVerbosity(
  blocks: Block[],
  verbosity: Verbosity,
  streaming: boolean,
): Segment[] {
  if (verbosity === "full") {
    return [{ kind: "pass", blocks, sourceStartIndex: 0 }];
  }

  const visible: VisibleItem[] = [];
  blocks.forEach((block, index) => {
    if (block.type === "tool" && isHiddenTool(block.tool)) return;
    visible.push({ block, index });
  });

  const artifacts = collectImageArtifacts(visible.map((v) => v.block));
  const rawSegments = splitByTier(visible);
  const lastProcessRawIndex = (() => {
    for (let i = rawSegments.length - 1; i >= 0; i -= 1) {
      if (rawSegments[i].tier === "process") return i;
    }
    return -1;
  })();

  const segments: Segment[] = [];
  let localOffset = 0;

  rawSegments.forEach((raw, rawIndex) => {
    const segStartLocal = localOffset;
    const segEndLocal = localOffset + raw.items.length;
    localOffset = segEndLocal;
    const sourceStartIndex = raw.items[0].index;
    const segBlocks = raw.items.map((v) => v.block);

    if (raw.tier === "l0") {
      segments.push({ kind: "pass", blocks: segBlocks, sourceStartIndex });
    } else {
      const isLastProcess = rawIndex === lastProcessRawIndex;
      const isLiveSegment = streaming && isLastProcess;

      if (isLiveSegment) {
        const lastBlock = segBlocks[segBlocks.length - 1];
        const live =
          lastBlock &&
          lastBlock.type === "tool" &&
          lastBlock.status === "running"
            ? { tool: lastBlock.tool, summary: lastBlock.summary ?? "" }
            : null;
        segments.push({
          kind: "activity_fold",
          blocks: segBlocks,
          sourceStartIndex,
          counts: computeCounts(segBlocks),
          live,
        });
      } else if (verbosity === "summary") {
        segments.push({
          kind: "activity_fold",
          blocks: segBlocks,
          sourceStartIndex,
          counts: computeCounts(segBlocks),
        });
      } else {
        // minimal，非直播段：只保留失败/中断工具与被拒/取消审批。
        const keptBlocks = segBlocks.filter(
          (block) =>
            (block.type === "tool" &&
              (block.status === "failed" || block.status === "interrupted")) ||
            (block.type === "approval" &&
              (block.status === "rejected" || block.status === "cancelled")),
        );
        if (keptBlocks.length > 0) {
          segments.push({
            kind: "activity_fold",
            blocks: keptBlocks,
            sourceStartIndex,
            counts: computeCounts(keptBlocks),
          });
        }
      }

      // artifacts 段不依附 fold 是否保留——该过程段范围内首现的图片照样紧随渲染。
      const imagePaths: string[] = [];
      let artifactsSourceStartIndex: number | null = null;
      for (let local = segStartLocal; local < segEndLocal; local += 1) {
        const paths = artifacts.byBlockIndex.get(local);
        if (!paths || paths.length === 0) continue;
        if (artifactsSourceStartIndex === null) {
          artifactsSourceStartIndex = visible[local].index;
        }
        imagePaths.push(...paths);
      }
      if (imagePaths.length > 0 && artifactsSourceStartIndex !== null) {
        segments.push({
          kind: "artifacts",
          imagePaths,
          sourceStartIndex: artifactsSourceStartIndex,
        });
      }
    }
  });

  return segments;
}

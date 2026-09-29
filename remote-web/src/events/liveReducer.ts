// liveReducer.ts — 手机端"接续归约"：把一串 live delta 帧折成一份可显示的 `blocks` 数组
// （`snapshot.partial_msg.blocks` 同构，M0 §3 v1.8.10）。
//
// 权威参照（只读对照，未改动）：app/src-tauri/src/display_reduce.rs `DisplayReducer::append_prose`
// ——"末块同类才续写，否则另起"。M0 §2 原文注释这条规则"与前端 live 同构"，即它本就不是桌面
// 独有的内部实现细节，而是三端（桌面 DisplayReducer / 现有前端 live streamBlocks.ts / 本文件）
// 共同遵守的显示合并不变量，所以这里复刻它是刻意对齐，不是巧合撞车。
//
// **范围边界（有意收窄）**：live wire 只有四种 delta（text_delta/thinking_delta/
// tool_output_delta/usage_delta，M0 §2），没有 `ToolStarted` 的 wire 等价物越过网络——所以本
// 归约器无法像桌面 `DisplayReducer` 那样在 tool 卡一开始就知道 tool 名/summary/card kind
// （那些字段只在 `AgentEvent::ToolStarted` 里，从不作为 live 帧广播）。
//
// **`tool_output_delta` 对齐桌面行为，不造可见占位块（审查返工·2026-08 校准，对齐
// display_reduce.rs:289 `AgentEvent::ToolOutputDelta` 分支）**：桌面那条分支只把文本累积进
// `tool_output`（一个独立的 id→string 缓冲区），**从不**往 `blocks` 里 push 新块——可见的
// `Block::Tool` 只能来自 `ToolStarted`（`self.blocks.push(Block::Tool{...})`）或已存在的
// 快照基线，`ToolOutputDelta`/`ToolCompleted` 都只更新/合并**已存在**的块（`lookup(&self.
// tool_index, id)` 找不到就不写）。本归约器镜像同一条规则：
//   - 命中一个**已存在**的可见 Tool 块（来自 snapshot 基线——那条块本就代表桌面已经把
//     `ToolStarted` 之类的信息揉进了 `partial_msg.blocks`）→ 直接续写它的 `output`。
//   - 命中一个**从未见过**的 id（没有快照基线、也没有 wire 等价的 ToolStarted 告诉我们它的
//     名字/摘要）→ **只缓存**到 `toolOutputBuffer`（不产生任何可见块）。早前版本在这里造一个
//     `tool:""`/`summary:""` 的占位可见块——那不是"降级复刻桌面行为"，是凭空发明了桌面自己都
//     不会产生的一种可见状态（一张没有名字的"幽灵"工具卡）。真正让它可见的时机是对应的
//     `tool.completed` 里程碑（另一条帧类型，带 `id`/`tool`/`status`/`exit_code`/`output`
//     完整字段，由调用方按 `id` 匹配、决定要不要把缓存的输出接上去——不在本模块范围内）。
//
// `usage_delta` 不产可显示块——与桌面 `DisplayReducer` 对 `AgentEvent::UsageDelta` 的处理一致
// （display_reduce.rs:414 该分支为空，只标记"事件已发生"不出块；DP-1 fixture 的
// `snapshot_response_running_no_partial` 样张正是这个语义的真实样张：喂一条 UsageDelta 后
// `partial_msg` 仍是 null）。

import type { ReducedBlock, TextBlock, ThinkingBlock, ToolBlock } from "./blocks.ts";
import type { LiveFrame } from "./parseFrame.ts";

export class LiveBlockReducer {
  private blocks: ReducedBlock[];
  /** id → 累积的未落地输出文本；只在没有对应可见 Tool 块时使用，见文件顶注。 */
  private toolOutputBuffer = new Map<string, string>();

  constructor(seedBlocks: ReducedBlock[] = []) {
    this.blocks = seedBlocks.map((block) => ({ ...block }));
  }

  /** 原子只读克隆——与桌面 `snapshot_blocks()` 同语义，调用方不得就地改动返回值。 */
  snapshotBlocks(): ReducedBlock[] {
    return this.blocks.map((block) => ({ ...block }));
  }

  feed(frame: LiveFrame): void {
    switch (frame.t) {
      case "text_delta":
        this.appendProse(frame.text, false);
        return;
      case "thinking_delta":
        this.appendProse(frame.text, true);
        return;
      case "tool_output_delta":
        this.appendToolOutput(frame.id, frame.text);
        return;
      case "usage_delta":
        return; // 不产可显示块——见文件顶注。
    }
  }

  private appendProse(chunk: string, thinking: boolean): void {
    if (chunk.length === 0) return;
    const last = this.blocks[this.blocks.length - 1];
    if (!thinking && last?.type === "text") {
      (last as TextBlock).text += chunk;
      return;
    }
    if (thinking && last?.type === "thinking") {
      (last as ThinkingBlock).text += chunk;
      return;
    }
    this.blocks.push(thinking ? { type: "thinking", text: chunk } : { type: "text", text: chunk });
  }

  private appendToolOutput(id: string, chunk: string): void {
    const existing = this.blocks.find((block): block is ToolBlock => block.type === "tool" && block.id === id);
    if (existing) {
      existing.output = (existing.output ?? "") + chunk;
      return;
    }
    // 未见过的 id、没有可见块可续写——只缓存，不造占位可见块（对齐桌面 display_reduce.rs:289）。
    const buffered = this.toolOutputBuffer.get(id) ?? "";
    this.toolOutputBuffer.set(id, buffered + chunk);
  }

  /**
   * 未挂到任何可见块上的累积输出——仅供消费方在收到对应 `tool.completed` 里程碑时参考（例如
   * 桌面自己的 `merged = output.as_deref().filter(...).unwrap_or(acc)` 那种"里程碑自带 output
   * 优先，缺省时退回累积增量"逻辑，`acc` 就是这里的东西）。不在 `snapshotBlocks()` 里出现。
   */
  bufferedToolOutput(id: string): string | undefined {
    return this.toolOutputBuffer.get(id);
  }
}

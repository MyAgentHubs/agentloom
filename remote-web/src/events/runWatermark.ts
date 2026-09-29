// runWatermark.ts — snapshot/live 竞速归约（M0 §3 v1.8.12 契约·brief §2 第四条）。
//
// 权威参照（只读对照）：
// The passage on how「snapshot replies flow through the event
// 通道」段的"水印语义（v1.8.12 订正）"与"形状不变量（v1.8.12 订正）"两小段——本文件每条分支
// 都能对回那两段原文，改动前务必回读全文，别凭记忆改。
//
// **两条水位是分开的概念，别混**：
//   - `through_run_seq`（本文件）—— run 内 seq 水位，只在这个模块里活，决定"这条 live 帧还
//     要不要"。run 一结束这个水位就作废（下一个 run 从新会话重新开始，不延续）。
//   - 房间级 `envelope.seq`（`store/indexeddbEventStore.ts`，仅 `kind=event` 有）—— 决定重连
//     `?last_seq=` 从哪续。彼此独立，不共用同一个"水位"变量，也不能互相替代。
//
// **本地"在跟的 run"只由已接受的 snapshot（或调用方显式初始化，如未来接 `run.status` 里程碑）
// 设立，不由裸 live 帧设立**——但**从 idle（`runId===null`）状态收到的第一条 live 帧例外**：
// M0 §3 原文明说"里程碑先于 live 出线、快照超车已入队 live 帧是常态"，也就是 snapshot 应答
// 本身可能被同一个 run 更早广播的 live 帧"抢跑"——客户端在收到 snapshot 之前就已经开始收到该
// run 的 live 帧，是正常时序而非异常。所以 idle 时的第一条 live 帧按"引导"处理：接受它、以它
// 的 seq 建立一个"临时水位"（`throughRunSeq` 从 null 起步，不是 wire snapshot 帧那个 `>=1`
// 不变量——那个不变量只约束"snapshot 帧本身长什么样"，不约束客户端这个纯本地的过渡态）。
// 之后无论是同一个 run 的后续 live 帧、还是姗姗来迟的真正 snapshot，都按下面的水位比较规则
// 走：snapshot 因为携带的是"服务端已经把这之前一切都揉进 partial_msg.blocks 的完整基线"，
// 会正确地把本地临时水位之前攒的东西整体替换掉（不是追加、不需要重放缓冲区）——这正是"gap
// 幂等"在客户端这一侧最简单也最正确的解法：不追不补，等下一份更高水位的权威快照来纠正即可。

import { LiveBlockReducer } from "./liveReducer.ts";
import type { ReducedBlock } from "./blocks.ts";
import type { LiveFrame, SnapshotResponseFrame } from "./parseFrame.ts";

export interface RunTrackState {
  runId: string | null;
  /** run 内已归约到的最后 seq；`runId===null` 时恒为 null（本地 idle 镜像）。 */
  throughRunSeq: number | null;
  reducer: LiveBlockReducer;
}

export function idleRunTrackState(): RunTrackState {
  return { runId: null, throughRunSeq: null, reducer: new LiveBlockReducer() };
}

export type SnapshotOutcome =
  | { accepted: true; next: RunTrackState }
  | { accepted: false; reason: "run_id_mismatch" | "stale_watermark" };

/**
 * 收 snapshot 应答后的接受/丢弃判定（M0 §3 v1.8.12）：
 *   1. `run_id === null`（idle 应答）且本地**已经**是 idle（`current.runId === null`）→
 *      接受（no-op，重新确认 idle）。
 *   1'. `run_id === null` 但本地**正在跟某个 run**（`current.runId !== null`）→ **丢弃**
 *      （"run_id_mismatch"·审查返工·2026-08 校准）。idle 快照的三个字段全是 null，没有
 *      `through_run_seq` 可比对新鲜度——无法区分"这份应答确实反映了此刻已转 idle"和"这是一份
 *      迟到/重放的陈旧应答，抓拍于新 run 开始之前，此刻其实又有新 run 在跑"。M0 §3 原文：
 *      真正的 idle 转换由 `msg.completed`/`run.status` 里程碑收敛，不归这个函数管——这个函数
 *      只根据"能证明新鲜度的水位"做取舍，idle 应答恰好没有这个凭证，所以在本地已经认定某个
 *      run 在跑时，宁可保留（可能过期的）现有状态，也不能被一份无法验真伪的 idle 应答清空。
 *   2. `run_id` 非 null：若本地已在跟某个**不同**的 run（`current.runId !== null` 且
 *      `!== snapshot.run_id`）→ 丢弃（"与本地在跟 run 不符"）。`current.runId === null`
 *      （idle 或刚被 live 帧引导出的临时态，视 `throughRunSeq` 而定）视作"尚无预期"，不算
 *      不符——见文件顶注的引导语义。
 *   3. `through_run_seq < 本地已归约水位` → 丢弃（水位回退的陈旧快照·FIX2 P1-4 订正：严格小于
 *      才丢，不再连"等于"一并丢）。`本地已归约水位` 在此刻可能来自更早接受的 snapshot，也可能来自
 *      idle 引导阶段已经吃进去的 live 帧——两种来源统一比较，规则不因来源分叉。
 *      **等值不再当陈旧丢弃，接受并整体替换基线**（M0 §3 原文"低于……丢弃"，字面只覆盖"低于"，
 *      不含"等于"）：idle 引导阶段由裸 live 帧临时建立的水位，归约出来的画面可能是残缺的（只见过
 *      "超车"到达的那几条 delta，没见过这个 run 更早期的内容——文件顶注"引导"段）；姗姗来迟、
 *      `through_run_seq` 恰好等于这个临时水位的 snapshot，正是**唯一能补齐这段前文的权威基线**，
 *      不是过期重复——继续当陈旧丢弃只会让这个引导态残缺画面永远定格，永远等不到补齐的机会。
 *   4. 否则接受：**reducer 整体替换**为 snapshot 携带的 `partial_msg.blocks`（服务端已经把
 *      `through_run_seq` 及之前的一切都揉进这份 blocks——是"新基线"不是"追加"），之后任何
 *      `seq <= through_run_seq` 的 live 帧（含此后迟到的）一律再丢弃，见 `applyLiveFrame`。
 */
export function applySnapshot(current: RunTrackState, snapshot: SnapshotResponseFrame): SnapshotOutcome {
  if (snapshot.run_id === null) {
    if (current.runId !== null) {
      return { accepted: false, reason: "run_id_mismatch" };
    }
    return { accepted: true, next: idleRunTrackState() };
  }
  if (current.runId !== null && current.runId !== snapshot.run_id) {
    return { accepted: false, reason: "run_id_mismatch" };
  }
  const throughRunSeq = snapshot.through_run_seq;
  if (throughRunSeq === null) {
    // parseFrame 的形状不变量已经保证 run_id 非 null 时 through_run_seq 非 null 且 >=1——
    // 这里只是让 TS 控制流知道，真到这条分支说明上游校验被绕过了，按异形帧拒。
    return { accepted: false, reason: "stale_watermark" };
  }
  // FIX2 P1-4：`<` 而不是 `<=`——等值不再当陈旧丢弃，见上方方法注释第 3 条。**live 帧的
  // `applyLiveFrame()` 那条 `<=` 丢弃判据不动，是另一条独立契约**（那边判的是"这条 live 帧的
  // 内容是不是已经追加进 reducer 过"的幂等性，这里判的是"这份 snapshot 携带的整体基线是不是比
  // 本地已知的更新"——语义不同，不能因为这条订正就顺手改掉那边）。
  if (current.throughRunSeq !== null && throughRunSeq < current.throughRunSeq) {
    return { accepted: false, reason: "stale_watermark" };
  }
  const seedBlocks = (snapshot.partial_msg?.blocks ?? []) as unknown as ReducedBlock[];
  return {
    accepted: true,
    next: { runId: snapshot.run_id, throughRunSeq, reducer: new LiveBlockReducer(seedBlocks) },
  };
}

export type LiveFrameOutcome =
  | { accepted: true; next: RunTrackState }
  | { accepted: false; reason: "run_id_mismatch" | "at_or_before_watermark" };

/**
 * live 帧接受/丢弃：
 *   - `current.runId === null`（idle）→ 引导：无条件接受，建立临时 run 跟踪（见文件顶注）。
 *   - `current.runId !== incoming run_id`（正在跟别的 run）→ 丢弃（"run_id_mismatch"）——
 *     不缓冲、不重放：正在跟踪的 run 是权威状态，混进另一个 run 的内容有把两条会话的显示糊在
 *     一起的风险，比"漏一条不相关 run 的 live 更新"更危险。
 *   - `seq <= 本地水位` → 丢弃（"at_or_before_watermark"）——**判据是纯 seq 比较，不依赖
 *     到达时间**：无论这条帧是"当下缓冲批"还是"snapshot 接受之后才姗姗来迟"，只要 seq 落在
 *     水位以内就一样丢，M0 §3 原文"不只当下缓冲批"这句就是指这条判据的时间无关性。同一判据也
 *     顺带防住乱序重投（同一个 run 内更早的 seq 在更晚的水位之后到达）。
 *   - 否则接受：喂进 reducer，水位推进到该帧的 seq。
 */
export function applyLiveFrame(current: RunTrackState, runId: string, frame: LiveFrame): LiveFrameOutcome {
  if (current.runId === null) {
    current.reducer.feed(frame);
    return { accepted: true, next: { runId, throughRunSeq: frame.seq, reducer: current.reducer } };
  }
  if (current.runId !== runId) {
    return { accepted: false, reason: "run_id_mismatch" };
  }
  if (current.throughRunSeq !== null && frame.seq <= current.throughRunSeq) {
    return { accepted: false, reason: "at_or_before_watermark" };
  }
  current.reducer.feed(frame);
  return { accepted: true, next: { runId: current.runId, throughRunSeq: frame.seq, reducer: current.reducer } };
}

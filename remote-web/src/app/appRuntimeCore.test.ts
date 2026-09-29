// appRuntimeCore.test.ts — INT1b · createAppRuntimeCore()/applyDecryptedMilestoneFrame()/
// applyDecryptedLiveFrame() 覆盖：逐会话路由、跨会话不串号、live 帧 run 归属查找、防御性丢弃。

import { describe, expect, it } from "vitest";
import type { ParsedFrame } from "../events/parseFrame.ts";
import {
  applyDecryptedLiveFrame,
  applyDecryptedHistoryFrame,
  applyDecryptedMilestoneFrame,
  checkFrameRouting,
  createAppRuntimeCore,
  getFrameDiagnostics,
  recordFrameApplied,
  recordFrameSeen,
  recordProcessFrameDrop,
  recordRoutingRejection,
} from "./appRuntimeCore.ts";

const ROOM = "0123456789abcdef0123456789abcdef";
const OTHER_ROOM = "fedcba9876543210fedcba9876543210";

describe("applyDecryptedMilestoneFrame() · session.index 路由到房间级 indexProjection", () => {
  it("session=null 的 session.index full 帧更新 indexProjection.sessions", () => {
    const core = createAppRuntimeCore();
    const frame: ParsedFrame = {
      t: "session.index",
      full: true,
      sessions: [{ id: "s-1", title: "T", repo_id: "r", archived: false, status: "running", run_id: "run-1", updated_at: 100 }],
    };
    applyDecryptedMilestoneFrame(core, null, frame);
    expect(core.indexProjection.sessions.get("s-1")?.title).toBe("T");
  });
});

describe("applyDecryptedMilestoneFrame() · 逐会话独立路由，不跨会话串号", () => {
  it("msg.completed 落到 envelopeSession 对应的 sessionProjection，不写进 indexProjection 或另一个会话", () => {
    const core = createAppRuntimeCore();
    const frame: ParsedFrame = { t: "msg.completed", message_id: 1, role: "assistant", blocks: [{ type: "text", text: "hi" }] };
    applyDecryptedMilestoneFrame(core, "s-1", frame);

    expect(core.sessionProjections.get("s-1")?.messages.get(1)?.role).toBe("assistant");
    expect(core.sessionProjections.has("s-2")).toBe(false);
    expect(core.indexProjection.messages.size).toBe(0);
  });

  it("两个会话各自的 message_id=1 互不覆盖（这是本文件存在的核心理由——共用一份 Projection 会撞号）", () => {
    const core = createAppRuntimeCore();
    applyDecryptedMilestoneFrame(core, "s-1", { t: "msg.completed", message_id: 1, role: "assistant", blocks: [{ type: "text", text: "from s-1" }] });
    applyDecryptedMilestoneFrame(core, "s-2", { t: "msg.completed", message_id: 1, role: "assistant", blocks: [{ type: "text", text: "from s-2" }] });

    const s1 = core.sessionProjections.get("s-1")!.messages.get(1);
    const s2 = core.sessionProjections.get("s-2")!.messages.get(1);
    expect((s1?.blocks[0] as { text: string }).text).toBe("from s-1");
    expect((s2?.blocks[0] as { text: string }).text).toBe("from s-2");
  });

  it("msg.completed/card.created/card.resolved/run.status/tool.completed 缺 envelopeSession(null) 时防御性丢弃，不崩溃、不误落进任何会话", () => {
    const core = createAppRuntimeCore();
    const frames: ParsedFrame[] = [
      { t: "msg.completed", message_id: 1, role: "assistant", blocks: [] },
      { t: "card.created", block: { decision_id: "d-1" } },
      { t: "card.resolved", decision_id: "d-1", status: "resolved", chosen_option: "a" },
      { t: "run.status", session_id: "s-1", status: "running", run_id: "run-1" },
      { t: "tool.completed", id: "t-1", tool: "shell", status: "ok", exit_code: 0, output: null },
    ];
    for (const frame of frames) {
      expect(() => applyDecryptedMilestoneFrame(core, null, frame)).not.toThrow();
    }
    expect(core.sessionProjections.size).toBe(0);
    expect(getFrameDiagnostics(core).milestoneSessionNull).toBe(5);
    expect(getFrameDiagnostics(core).lastDroppedFrameT).toBe("tool.completed");
  });

  it("snapshot 应答按帧自带的 session 字段路由 runTracks（不依赖 envelopeSession 参数——envelope.session 与 payload.session 对同一条 snapshot 应答应恒相等，但 runWatermark 的既有契约就是吃 frame.session）", () => {
    const core = createAppRuntimeCore();
    const frame: ParsedFrame = {
      t: "snapshot",
      session: "s-1",
      run_id: "run-1",
      through_run_seq: 5,
      partial_msg: { role: "assistant", blocks: [{ type: "text", text: "partial" }] },
    };
    applyDecryptedMilestoneFrame(core, "s-1", frame);
    const track = core.runTracks.get("s-1");
    expect(track?.runId).toBe("run-1");
    expect(track?.throughRunSeq).toBe(5);
  });

  it("snapshot 被 run 代次/水位规则拒收时计入 snapshotRejected", () => {
    const core = createAppRuntimeCore();
    applyDecryptedMilestoneFrame(core, "s-1", {
      t: "snapshot",
      session: "s-1",
      run_id: "run-1",
      through_run_seq: 5,
      partial_msg: null,
    });

    expect(
      applyDecryptedMilestoneFrame(core, "s-1", {
        t: "snapshot",
        session: "s-1",
        run_id: "run-1",
        through_run_seq: 4,
        partial_msg: null,
      }),
    ).toBe(false);
    expect(getFrameDiagnostics(core).snapshotRejected).toBe(1);
  });

  it("unknown/未预期到达的帧类型（如 replay.head 误经这条路径）防御性丢弃", () => {
    const core = createAppRuntimeCore();
    const frame: ParsedFrame = { t: "replay.head", epoch: 1, headSeq: 2 };
    expect(() => applyDecryptedMilestoneFrame(core, null, frame)).not.toThrow();
    expect(getFrameDiagnostics(core).defensiveDefault).toBe(1);
  });
});

describe("applyDecryptedLiveFrame() · run 归属查找 + 逐会话独立归约", () => {
  it("live 帧在会话有已知 run(runStatusBySession) 时被接受并归约进该会话的 runTrack", () => {
    const core = createAppRuntimeCore();
    applyDecryptedMilestoneFrame(core, "s-1", { t: "run.status", session_id: "s-1", status: "running", run_id: "run-1" });

    applyDecryptedLiveFrame(core, "s-1", { t: "text_delta", seq: 1, text: "hello " });
    applyDecryptedLiveFrame(core, "s-1", { t: "text_delta", seq: 2, text: "world" });

    const track = core.runTracks.get("s-1");
    expect(track?.runId).toBe("run-1");
    expect(track?.reducer.snapshotBlocks()).toEqual([{ type: "text", text: "hello world" }]);
  });

  it("live 帧在该会话尚无 run.status 记录时被丢弃（不猜归属），不创建 runTrack", () => {
    const core = createAppRuntimeCore();
    applyDecryptedLiveFrame(core, "s-1", { t: "text_delta", seq: 1, text: "hello" });
    expect(core.runTracks.has("s-1")).toBe(false);
    expect(getFrameDiagnostics(core).liveDroppedNoRun).toBe(1);
    expect(getFrameDiagnostics(core).lastDroppedKind).toBe("live");
  });

  it("envelopeSession=null 时防御性丢弃", () => {
    const core = createAppRuntimeCore();
    expect(() => applyDecryptedLiveFrame(core, null, { t: "text_delta", seq: 1, text: "x" })).not.toThrow();
    expect(core.runTracks.size).toBe(0);
    expect(getFrameDiagnostics(core).liveSessionNull).toBe(1);
  });

  it("非 live 帧类型（如 msg.completed 误经这条路径）防御性丢弃，甚至不创建 runTrack", () => {
    const core = createAppRuntimeCore();
    applyDecryptedMilestoneFrame(core, "s-1", { t: "run.status", session_id: "s-1", status: "running", run_id: "run-1" });
    applyDecryptedLiveFrame(core, "s-1", { t: "msg.completed", message_id: 1, role: "assistant", blocks: [] });
    expect(core.runTracks.has("s-1")).toBe(false);
    expect(getFrameDiagnostics(core).liveUnknownDelta).toBe(1);
  });

  it("live 帧被当前 run 水位拒收时计入 liveWatermarkRejected", () => {
    const core = createAppRuntimeCore();
    applyDecryptedMilestoneFrame(core, "s-1", { t: "run.status", session_id: "s-1", status: "running", run_id: "run-1" });
    expect(applyDecryptedLiveFrame(core, "s-1", { t: "text_delta", seq: 2, text: "new" })).toBe(true);
    expect(applyDecryptedLiveFrame(core, "s-1", { t: "text_delta", seq: 1, text: "stale" })).toBe(false);
    expect(getFrameDiagnostics(core).liveWatermarkRejected).toBe(1);
  });

  it("两个会话的 live 归约互不干扰", () => {
    const core = createAppRuntimeCore();
    applyDecryptedMilestoneFrame(core, "s-1", { t: "run.status", session_id: "s-1", status: "running", run_id: "run-1" });
    applyDecryptedMilestoneFrame(core, "s-2", { t: "run.status", session_id: "s-2", status: "running", run_id: "run-2" });

    applyDecryptedLiveFrame(core, "s-1", { t: "text_delta", seq: 1, text: "session one" });
    applyDecryptedLiveFrame(core, "s-2", { t: "text_delta", seq: 1, text: "session two" });

    expect(core.runTracks.get("s-1")?.reducer.snapshotBlocks()).toEqual([{ type: "text", text: "session one" }]);
    expect(core.runTracks.get("s-2")?.reducer.snapshotBlocks()).toEqual([{ type: "text", text: "session two" }]);
  });
});

describe("U2 根修：run 终态清残留 runTrack（症状①手机端 typing 恒显 · 成因 2）", () => {
  it("msg.completed 到达时，若该会话已有 runTrack（还带着上一条 partial），重置为 idle——runId/水位/reducer 全部清空", () => {
    const core = createAppRuntimeCore();
    applyDecryptedMilestoneFrame(core, "s-1", { t: "run.status", session_id: "s-1", status: "running", run_id: "run-1" });
    applyDecryptedLiveFrame(core, "s-1", { t: "text_delta", seq: 1, text: "partial text" });
    expect(core.runTracks.get("s-1")?.reducer.snapshotBlocks()).toEqual([{ type: "text", text: "partial text" }]);

    applyDecryptedMilestoneFrame(core, "s-1", {
      t: "msg.completed",
      message_id: 1,
      role: "assistant",
      blocks: [{ type: "text", text: "final message" }],
    });

    const track = core.runTracks.get("s-1");
    expect(track?.runId).toBeNull();
    expect(track?.throughRunSeq).toBeNull();
    expect(track?.reducer.snapshotBlocks()).toEqual([]);
  });

  it("msg.completed 到达但该会话从未有过 runTrack 条目（从未收到 snapshot/live）时，不凭空创建一条——保持既有不变量", () => {
    const core = createAppRuntimeCore();
    applyDecryptedMilestoneFrame(core, "s-1", {
      t: "msg.completed",
      message_id: 1,
      role: "assistant",
      blocks: [{ type: "text", text: "final message" }],
    });
    expect(core.runTracks.has("s-1")).toBe(false);
  });

  it("run.status 转非 running（status!==\"running\"）时，若该会话已有 runTrack，重置为 idle——判据是 status，不是 run_id：桌面侧 refresh 发布的终态帧刻意沿用旧 run_id（db.rs:13168：\"refresh 发布必须沿用写前读到的 run_id\"），生产实际形态是 `{status:\"idle\", run_id:\"run-1\"}`，从不是 `run_id:null`", () => {
    const core = createAppRuntimeCore();
    applyDecryptedMilestoneFrame(core, "s-1", { t: "run.status", session_id: "s-1", status: "running", run_id: "run-1" });
    applyDecryptedLiveFrame(core, "s-1", { t: "text_delta", seq: 1, text: "partial text" });
    expect(core.runTracks.get("s-1")?.runId).toBe("run-1");

    applyDecryptedMilestoneFrame(core, "s-1", { t: "run.status", session_id: "s-1", status: "idle", run_id: "run-1" });

    const track = core.runTracks.get("s-1");
    expect(track?.runId).toBeNull();
    expect(track?.throughRunSeq).toBeNull();
    expect(track?.reducer.snapshotBlocks()).toEqual([]);
  });

  it("run.status 仍是 running 时不动 runTrack——只有转非 running 才重置", () => {
    const core = createAppRuntimeCore();
    applyDecryptedMilestoneFrame(core, "s-1", { t: "run.status", session_id: "s-1", status: "running", run_id: "run-1" });
    applyDecryptedLiveFrame(core, "s-1", { t: "text_delta", seq: 1, text: "partial text" });

    // 同一个 run 内再来一条 run.status（例如状态字段变化但 run_id 不变），不该清掉正在跟的 partial。
    applyDecryptedMilestoneFrame(core, "s-1", { t: "run.status", session_id: "s-1", status: "running", run_id: "run-1" });

    const track = core.runTracks.get("s-1");
    expect(track?.runId).toBe("run-1");
    expect(track?.reducer.snapshotBlocks()).toEqual([{ type: "text", text: "partial text" }]);
  });

  it("solo 开跑帧 {status:\"running\", run_id:null} 不触发重置——这是唯一发 run_id:null 的生产帧，旧判据 run_id===null 曾把它错当终态帧、开跑瞬间就误清了 runTrack", () => {
    const core = createAppRuntimeCore();
    applyDecryptedMilestoneFrame(core, "s-1", { t: "run.status", session_id: "s-1", status: "running", run_id: "run-1" });
    applyDecryptedLiveFrame(core, "s-1", { t: "text_delta", seq: 1, text: "partial text" });

    applyDecryptedMilestoneFrame(core, "s-1", { t: "run.status", session_id: "s-1", status: "running", run_id: null });

    const track = core.runTracks.get("s-1");
    expect(track?.runId).toBe("run-1");
    expect(track?.reducer.snapshotBlocks()).toEqual([{ type: "text", text: "partial text" }]);
  });

  it("run.status 转非 running 但该会话从未有过 runTrack 条目时，不凭空创建一条", () => {
    const core = createAppRuntimeCore();
    applyDecryptedMilestoneFrame(core, "s-1", { t: "run.status", session_id: "s-1", status: "idle", run_id: "run-1" });
    expect(core.runTracks.has("s-1")).toBe(false);
  });
});

describe("applyDecryptedHistoryFrame() · 历史分页合并", () => {
  it("无 runId 也能应用；新键插入、已有实时键不覆盖，并推进游标/耗尽态与诊断", () => {
    const core = createAppRuntimeCore();
    applyDecryptedMilestoneFrame(core, "s-1", {
      t: "msg.completed",
      message_id: 2,
      role: "assistant",
      blocks: [{ type: "text", text: "live wins" }],
    });

    expect(applyDecryptedHistoryFrame(core, "s-1", {
      t: "history",
      session: "s-1",
      before_message_id: null,
      messages: [
        { message_id: 1, role: "user", blocks: [{ type: "text", text: "older" }] },
        { message_id: 2, role: "assistant", blocks: [{ type: "text", text: "stale history" }] },
      ],
      next_before: 1,
    })).toBe(true);

    const projection = core.sessionProjections.get("s-1")!;
    expect((projection.messages.get(1)?.blocks[0] as { text: string }).text).toBe("older");
    expect((projection.messages.get(2)?.blocks[0] as { text: string }).text).toBe("live wins");
    expect(projection.historyCursor).toBe(1);
    expect(projection.historyExhausted).toBe(false);
    expect(core.runTracks.has("s-1")).toBe(false);
    expect(getFrameDiagnostics(core).historyApplied).toBe(1);

    expect(applyDecryptedHistoryFrame(core, "s-1", {
      t: "history",
      session: "s-1",
      before_message_id: 1,
      messages: [],
      next_before: null,
    })).toBe(true);
    expect(projection.historyCursor).toBeNull();
    expect(projection.historyExhausted).toBe(true);
  });

  it("拒绝缺 session/错误类型并计 historyRejected", () => {
    const core = createAppRuntimeCore();
    expect(applyDecryptedHistoryFrame(core, null, {
      t: "history", session: "s-1", before_message_id: null, messages: [], next_before: null,
    })).toBe(false);
    expect(applyDecryptedHistoryFrame(core, "s-1", {
      t: "text_delta", seq: 1, text: "x",
    })).toBe(false);
    expect(getFrameDiagnostics(core).historyRejected).toBe(2);
  });

  it("迟到页仍补消息，但 advanceCursor=false 时不回退当前游标", () => {
    const core = createAppRuntimeCore();
    applyDecryptedHistoryFrame(core, "s-1", {
      t: "history", session: "s-1", before_message_id: null, messages: [], next_before: 100,
    });
    applyDecryptedHistoryFrame(core, "s-1", {
      t: "history",
      session: "s-1",
      before_message_id: 999,
      messages: [{ message_id: 50, role: "user", blocks: [] }],
      next_before: 50,
    }, { advanceCursor: false });

    const projection = core.sessionProjections.get("s-1")!;
    expect(projection.messages.has(50)).toBe(true);
    expect(projection.historyCursor).toBe(100);
    expect(projection.historyExhausted).toBe(false);
  });
});

describe("checkFrameRouting() · 入库/归约前的强制路由校验（INT1c 审查返工·P0）", () => {
  it("负例①：信封 room 与当前配对房间不符 → room_mismatch", () => {
    const frame: ParsedFrame = { t: "msg.completed", message_id: 1, role: "assistant", blocks: [] };
    const result = checkFrameRouting(ROOM, { room: OTHER_ROOM, session: "s-1" }, frame);
    expect(result).toEqual({ accepted: false, reason: "room_mismatch" });
  });

  it("负例②：session.index 帧外层 session 非 null → session_index_outer_session_not_null", () => {
    const frame: ParsedFrame = { t: "session.index", full: true, sessions: [] };
    const result = checkFrameRouting(ROOM, { room: ROOM, session: "s-1" }, frame);
    expect(result).toEqual({ accepted: false, reason: "session_index_outer_session_not_null" });
  });

  it("负例③a：snapshot 帧内层 session 与外层信封 session 不等 → inner_outer_session_mismatch", () => {
    const frame: ParsedFrame = { t: "snapshot", session: "s-1", run_id: "run-1", through_run_seq: 1, partial_msg: null };
    const result = checkFrameRouting(ROOM, { room: ROOM, session: "s-2" }, frame);
    expect(result).toEqual({ accepted: false, reason: "inner_outer_session_mismatch" });
  });

  it("负例③b：run.status 帧内层 session_id 与外层信封 session 不等 → inner_outer_session_mismatch", () => {
    const frame: ParsedFrame = { t: "run.status", session_id: "s-1", status: "running", run_id: "run-1" };
    const result = checkFrameRouting(ROOM, { room: ROOM, session: "s-2" }, frame);
    expect(result).toEqual({ accepted: false, reason: "inner_outer_session_mismatch" });
  });

  it("负例③c：history 帧内层 session 与外层信封 session 不等 → inner_outer_session_mismatch", () => {
    const frame: ParsedFrame = { t: "history", session: "s-1", before_message_id: null, messages: [], next_before: null };
    expect(checkFrameRouting(ROOM, { room: ROOM, session: "s-2" }, frame)).toEqual({
      accepted: false,
      reason: "inner_outer_session_mismatch",
    });
  });

  it("正例：room 匹配 + session 关系全部合法时接受——即便 envelope 没带 epoch 字段，epoch 从不是这里的校验维度（回归：别把 epoch 也当路由维度）", () => {
    const msgFrame: ParsedFrame = { t: "msg.completed", message_id: 1, role: "assistant", blocks: [] };
    expect(checkFrameRouting(ROOM, { room: ROOM, session: "s-1" }, msgFrame)).toEqual({ accepted: true });

    const indexFrame: ParsedFrame = { t: "session.index", full: true, sessions: [] };
    expect(checkFrameRouting(ROOM, { room: ROOM, session: null }, indexFrame)).toEqual({ accepted: true });

    const snapshotFrame: ParsedFrame = { t: "snapshot", session: "s-1", run_id: "run-1", through_run_seq: 1, partial_msg: null };
    expect(checkFrameRouting(ROOM, { room: ROOM, session: "s-1" }, snapshotFrame)).toEqual({ accepted: true });

    const runStatusFrame: ParsedFrame = { t: "run.status", session_id: "s-1", status: "running", run_id: "run-1" };
    expect(checkFrameRouting(ROOM, { room: ROOM, session: "s-1" }, runStatusFrame)).toEqual({ accepted: true });

    // 同一房间、"新 epoch"下到达的合法帧同样接受——checkFrameRouting 的签名压根不接收 epoch，
    // 这条用例钉死"以后别有人手滑把 epoch 也塞进这个校验维度"的回归。
    const laterEpochFrame: ParsedFrame = { t: "msg.completed", message_id: 2, role: "assistant", blocks: [] };
    expect(checkFrameRouting(ROOM, { room: ROOM, session: "s-1" }, laterEpochFrame)).toEqual({ accepted: true });
  });

  it("session.index 帧外层 session 恰为 null（正常形态）不触发任何拒绝理由", () => {
    const frame: ParsedFrame = { t: "session.index", full: true, sessions: [] };
    expect(checkFrameRouting(ROOM, { room: ROOM, session: null }, frame)).toEqual({ accepted: true });
  });
});

describe("recordRoutingRejection() · 拒绝计数不静默", () => {
  it("同一 reason 累加计数；不同 reason 分开计数", () => {
    const core = createAppRuntimeCore();
    expect(core.routingRejections.size).toBe(0);

    recordRoutingRejection(core, "room_mismatch");
    recordRoutingRejection(core, "room_mismatch");
    recordRoutingRejection(core, "inner_outer_session_mismatch");

    expect(core.routingRejections.get("room_mismatch")).toBe(2);
    expect(core.routingRejections.get("inner_outer_session_mismatch")).toBe(1);
    expect(core.routingRejections.get("session_index_outer_session_not_null")).toBeUndefined();
    expect(getFrameDiagnostics(core).routingRejected).toBe(3);
  });
});

describe("FrameDiagnostics · processFrame 分级诊断", () => {
  it("逐闸独立累加，并保留最后一次 parse reason 与最后一次丢弃的 t/kind", () => {
    const core = createAppRuntimeCore();

    recordFrameSeen(core);
    recordFrameSeen(core);
    recordProcessFrameDrop(core, "kindSkipped", { kind: "presence", t: null });
    recordProcessFrameDrop(core, "decryptFailed", { kind: "event", t: null });
    recordProcessFrameDrop(core, "parseFailed", { kind: "event", t: "future.frame", parseReason: "unknown_t" });
    recordProcessFrameDrop(core, "missingIds", { kind: "event", t: "msg.completed" });
    recordProcessFrameDrop(core, "storeDuplicate", { kind: "event", t: "session.index" });
    recordFrameApplied(core);

    expect(getFrameDiagnostics(core)).toMatchObject({
      framesSeen: 2,
      kindSkipped: 1,
      decryptFailed: 1,
      parseFailed: 1,
      lastParseFailedReason: "unknown_t",
      missingIds: 1,
      storeError: 0,
      storeDuplicate: 1,
      applied: 1,
      lastDroppedFrameT: "session.index",
      lastDroppedKind: "event",
      lastDroppedErrorMessage: null,
    });
  });

  it("初始值全为零/null；routingRejected 动态复用分类 Map 汇总", () => {
    const core = createAppRuntimeCore();
    expect(getFrameDiagnostics(core)).toEqual({
      framesSeen: 0,
      plaintextControl: 0,
      kindSkipped: 0,
      decryptFailed: 0,
      parseFailed: 0,
      lastParseFailedReason: null,
      routingRejected: 0,
      missingIds: 0,
      storeError: 0,
      storeDuplicate: 0,
      applied: 0,
      milestoneSessionNull: 0,
      snapshotRejected: 0,
      defensiveDefault: 0,
      liveSessionNull: 0,
      liveUnknownDelta: 0,
      liveDroppedNoRun: 0,
      liveWatermarkRejected: 0,
      historyApplied: 0,
      historyRejected: 0,
      lastDroppedFrameT: null,
      lastDroppedKind: null,
      lastDroppedErrorMessage: null,
    });

    recordRoutingRejection(core, "room_mismatch", { kind: "event", t: "msg.completed" });
    expect(getFrameDiagnostics(core)).toMatchObject({
      routingRejected: 1,
      lastDroppedFrameT: "msg.completed",
      lastDroppedKind: "event",
    });
  });
});

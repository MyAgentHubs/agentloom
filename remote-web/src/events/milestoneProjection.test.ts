// milestoneProjection.test.ts — TDD 覆盖 src/events/milestoneProjection.ts（业务层二次去重，
// brief §2 项 2 后半）。
//
// 消费 data-plane-v1.json 的里程碑样张（session.index ×5 / msg.completed ×1 / card.created ×1 /
// card.resolved ×1 / run.status ×2 / tool.completed ×1 = 11 张）——parseFrame.test.ts 验的是
// "这些帧能不能被正确解析"，这里验的是下一层："解析出来的帧折进 MilestoneProjection 后，本地
// 状态对不对、且重复应用是否真的幂等（message_id/decision_id 二次去重）"。

import { describe, expect, it } from "vitest";
import { loadFixture } from "../test-support/fixtures.ts";
import { parseFrame } from "./parseFrame.ts";
import { MilestoneProjection } from "./milestoneProjection.ts";

interface DataPlaneCase {
  name: string;
  frame: unknown;
}
interface DataPlaneFixture {
  cases: DataPlaneCase[];
}

const fixture = loadFixture<DataPlaneFixture>("data-plane-v1.json");
const byName = new Map(fixture.cases.map((entry) => [entry.name, entry]));

function parsed(name: string) {
  const entry = byName.get(name);
  if (!entry) throw new Error(`missing fixture case: ${name}`);
  const result = parseFrame(entry.frame);
  if (!result.ok)
    throw new Error(`fixture case ${name} failed to parse: ${result.reason}`);
  return result.frame;
}

describe("MilestoneProjection · fixture-driven (data-plane-v1.json milestone samples)", () => {
  it("applyMsgCompleted (msg_completed): records the message keyed by message_id", () => {
    const projection = new MilestoneProjection();
    const frame = parsed("msg_completed");
    if (frame.t !== "msg.completed") throw new Error("expected msg.completed");
    projection.applyMsgCompleted(frame);
    expect(projection.messages.get(101)).toEqual({
      messageId: 101,
      role: "assistant",
      blocks: frame.blocks,
      agent: "Claude",
      revision: 1,
    });
  });

  it("applyMsgCompleted (activity_summary_first_send → activity_summary_revision_update): top-level revision from real fixture samples drives 高者胜, not self-authored literals", () => {
    // msgfix2 U1（M0 §10.11）：activity_summary 不是新帧型，仍是 msg.completed——这里用真实的
    // 两张样张（revision 1→2）驱动 applyMsgCompleted，证明顶层 revision 读取路径接的是桌面
    // real fixture wire samples, not the hand-authored literals from the describe block above.
    const projection = new MilestoneProjection();
    const firstSend = parsed("activity_summary_first_send");
    if (firstSend.t !== "msg.completed") throw new Error("expected msg.completed");
    projection.applyMsgCompleted(firstSend);
    expect(projection.messages.get(501)).toEqual({
      messageId: 501,
      role: "assistant",
      blocks: firstSend.blocks,
      revision: 1,
    });
    expect(firstSend.blocks).toEqual([
      {
        type: "activity_summary",
        run_id: "run-9",
        tool_calls: 3,
        failed: 0,
        mcp_calls: 1,
        permission_prompts: 0,
        state: "running",
      },
    ]);

    const revisionUpdate = parsed("activity_summary_revision_update");
    if (revisionUpdate.t !== "msg.completed") throw new Error("expected msg.completed");
    projection.applyMsgCompleted(revisionUpdate);
    expect(projection.messages.get(501)?.revision).toBe(2);
    expect(projection.messages.get(501)?.blocks).toEqual(revisionUpdate.blocks);

    // 高者胜：同一 message_id 的旧 revision 到达（迟到/乱序）不得覆盖已知的更高 revision。
    projection.applyMsgCompleted(firstSend);
    expect(projection.messages.get(501)?.revision).toBe(2);
  });

  it("applyCardCreated (card_created): records the decision card keyed by decision_id", () => {
    const projection = new MilestoneProjection();
    const frame = parsed("card_created");
    if (frame.t !== "card.created") throw new Error("expected card.created");
    projection.applyCardCreated(frame);
    const card = projection.decisionCards.get("d-1");
    expect(card?.status).toBe("pending");
    expect(card?.chosenOption).toBeNull();
    expect(card?.block).toEqual(frame.block);
  });

  it("applyCardResolved (card_resolved): updates status/chosen_option, preserves prior block", () => {
    const projection = new MilestoneProjection();
    const createdFrame = parsed("card_created");
    if (createdFrame.t !== "card.created")
      throw new Error("expected card.created");
    projection.applyCardCreated(createdFrame);

    const resolvedFrame = parsed("card_resolved");
    if (resolvedFrame.t !== "card.resolved")
      throw new Error("expected card.resolved");
    projection.applyCardResolved(resolvedFrame);

    const card = projection.decisionCards.get("d-1");
    expect(card?.status).toBe("resolved");
    expect(card?.chosenOption).toBe("yes");
    expect(card?.block).toEqual(createdFrame.block); // block content preserved from card.created
  });

  it("applyRunStatus (run_status_running then run_status_idle): last-write-wins per session_id", () => {
    const projection = new MilestoneProjection();
    const runningFrame = parsed("run_status_running");
    if (runningFrame.t !== "run.status") throw new Error("expected run.status");
    projection.applyRunStatus(runningFrame);
    expect(projection.runStatusBySession.get("sess-1")).toEqual({
      status: "running",
      runId: "run-7",
    });

    const idleFrame = parsed("run_status_idle");
    if (idleFrame.t !== "run.status") throw new Error("expected run.status");
    projection.applyRunStatus(idleFrame);
    expect(projection.runStatusBySession.get("sess-1")).toEqual({
      status: "idle",
      runId: null,
    });
  });

  it("applyToolCompleted (tool_completed): records by tool id", () => {
    const projection = new MilestoneProjection();
    const frame = parsed("tool_completed");
    if (frame.t !== "tool.completed")
      throw new Error("expected tool.completed");
    projection.applyToolCompleted(frame);
    expect(projection.toolCompletions.get("tool-1")).toEqual({
      id: "tool-1",
      tool: "shell",
      status: "ok",
      exitCode: 0,
      output: "build succeeded",
    });
  });

  it("applySessionIndex full snapshot replaces the whole sessions map", () => {
    const projection = new MilestoneProjection();
    const frame = parsed("session_index_full");
    if (frame.t !== "session.index") throw new Error("expected session.index");
    projection.applySessionIndex(frame);
    expect(projection.sessions.size).toBe(2);
    expect(projection.sessions.get("sess-1")).toMatchObject({
      title: "Fix login bug",
      status: "running",
    });
    expect(projection.sessions.get("sess-2")).toMatchObject({
      title: "Update docs",
      status: null,
    });
  });

  it("applySessionIndex created/renamed/archived/deleted increments apply on top of a full baseline", () => {
    const projection = new MilestoneProjection();
    const fullFrame = parsed("session_index_full");
    if (fullFrame.t !== "session.index")
      throw new Error("expected session.index");
    projection.applySessionIndex(fullFrame);

    const createdFrame = parsed("session_index_created");
    if (createdFrame.t !== "session.index")
      throw new Error("expected session.index");
    projection.applySessionIndex(createdFrame);
    expect(projection.sessions.get("sess-3")).toMatchObject({
      title: "New session",
      archived: false,
    });

    const renamedFrame = parsed("session_index_renamed");
    if (renamedFrame.t !== "session.index")
      throw new Error("expected session.index");
    projection.applySessionIndex(renamedFrame);
    expect(projection.sessions.get("sess-1")?.title).toBe("Renamed title");

    const archivedFrame = parsed("session_index_archived");
    if (archivedFrame.t !== "session.index")
      throw new Error("expected session.index");
    projection.applySessionIndex(archivedFrame);
    expect(projection.sessions.get("sess-1")?.archived).toBe(true);
    expect(projection.sessions.get("sess-2")?.archived).toBe(true);

    // unarchived 增量把 archived 标志翻回 false——同一条 op 分支
    // （`applySessionIndexArchived`）由 `archived: frame.op === "archived"` 派生，不是只有
    // "archived" 这一路会被覆盖测到。样张目录里没有单独的 session_index_unarchived case（Rust
    // 侧 `build_session_index_archived_payload(ids, false)` 已由
    // `builds_session_index_archived_and_unarchived_payloads` 覆盖过这个 op 的形状），这里按同一
    // 协议形状手拼、只翻转 op 值。
    const unarchivedResult = parseFrame({
      t: "session.index",
      op: "unarchived",
      full: false,
      ids: ["sess-1"],
    });
    if (!unarchivedResult.ok || unarchivedResult.frame.t !== "session.index") {
      throw new Error("expected session.index");
    }
    projection.applySessionIndex(unarchivedResult.frame);
    expect(projection.sessions.get("sess-1")?.archived).toBe(false);
    expect(projection.sessions.get("sess-2")?.archived).toBe(true);

    const deletedFrame = parsed("session_index_deleted");
    if (deletedFrame.t !== "session.index")
      throw new Error("expected session.index");
    // sess-9 doesn't exist in the baseline — deleting a never-seen id must not throw.
    expect(() => projection.applySessionIndex(deletedFrame)).not.toThrow();
    expect(projection.sessions.has("sess-9")).toBe(false);
  });

  it("applySessionIndexFull stores the top-level repo summary (M2-4x); defaults to null before any full snapshot", () => {
    const projection = new MilestoneProjection();
    expect(projection.activeRepo).toBeNull();

    // 样张目录里的 session_index_full case 的 repo 字段是 null（builder 层单测场景，见样张
    // desc）——先证 null 这一路真的落地。
    const fullFrame = parsed("session_index_full");
    if (fullFrame.t !== "session.index")
      throw new Error("expected session.index");
    projection.applySessionIndex(fullFrame);
    expect(projection.activeRepo).toBeNull();

    // Shared filled-state fixture (a backlog-added case): a full snapshot that really carries {id, name} — activeRepo must actually persist
    // 这份摘要。session_index_full_with_repo_name 与 Rust 侧
    // data_plane_v1_session_index_filled_variant_matches_fixture_and_drives_builders 共用
    // 同一张样张，字段改名会两端一起红，不会一端漏测。
    const withRepoFrame = parsed("session_index_full_with_repo_name");
    if (withRepoFrame.t !== "session.index" || !withRepoFrame.full) {
      throw new Error("expected session.index full frame");
    }
    projection.applySessionIndex(withRepoFrame);
    expect(projection.activeRepo).toEqual({
      id: "repo-a",
      name: "Acme Metrics",
    });

    // 每次全量快照原样替换（同 sessions 的"全量替换"语义）——省略 repo 键的旧桌面帧必须把
    // activeRepo 归零，不是保留上一次连接残留的项目摘要。这一态没有共享样张（省略 repo 键
    // 是纯粹的旧桌面兼容形状校验，不是数据字段填充态，与本任务的『填充态对拍』关注点不同——
    // 见 parseFrame.test.ts 里同款的手造边界语料），保留自造语料。
    const withoutRepoResult = parseFrame({
      t: "session.index",
      full: true,
      sessions: [],
    });
    if (
      !withoutRepoResult.ok ||
      withoutRepoResult.frame.t !== "session.index" ||
      !withoutRepoResult.frame.full
    ) {
      throw new Error("expected session.index full frame");
    }
    projection.applySessionIndex(withoutRepoResult.frame);
    expect(projection.activeRepo).toBeNull();
  });

  it("applySessionIndexCreated carries repo_name onto the new row (M2-4x)", () => {
    const projection = new MilestoneProjection();
    // Shared filled-state fixture (a backlog-added case): matches the Rust side's
    // data_plane_v1_session_index_filled_variant_matches_fixture_and_drives_builders 共用
    // session_index_created_with_repo_name。
    const createdFrame = parsed("session_index_created_with_repo_name");
    if (createdFrame.t !== "session.index")
      throw new Error("expected session.index");
    projection.applySessionIndex(createdFrame);
    expect(projection.sessions.get("sess-3")).toMatchObject({
      repo_name: "Acme Metrics",
    });
  });
});

describe("MilestoneProjection · business-level idempotency (self-authored — message_id/decision_id secondary dedup)", () => {
  it("applying the same msg.completed frame twice does not duplicate — Map stays single-entry, last write wins", () => {
    const projection = new MilestoneProjection();
    const frame = parsed("msg_completed");
    if (frame.t !== "msg.completed") throw new Error("expected msg.completed");
    projection.applyMsgCompleted(frame);
    projection.applyMsgCompleted(frame); // simulate a redelivery under a *different* client_msg_id
    expect(projection.messages.size).toBe(1);
    expect(projection.messages.get(101)).toEqual({
      messageId: 101,
      role: "assistant",
      blocks: frame.blocks,
      agent: "Claude",
      revision: 1,
    });
  });

  it("applying card.created twice for the same decision_id does not create two entries", () => {
    const projection = new MilestoneProjection();
    const frame = parsed("card_created");
    if (frame.t !== "card.created") throw new Error("expected card.created");
    projection.applyCardCreated(frame);
    projection.applyCardCreated(frame);
    expect(projection.decisionCards.size).toBe(1);
  });

  it("card.created with a non-string/missing decision_id is dropped, not crashed, and not recorded", () => {
    const projection = new MilestoneProjection();
    expect(() =>
      projection.applyCardCreated({
        t: "card.created",
        block: { status: "pending" },
      }),
    ).not.toThrow();
    expect(projection.decisionCards.size).toBe(0);
  });

  it("run.status re-applied with identical fields is idempotent (Map still one entry, same value)", () => {
    const projection = new MilestoneProjection();
    const frame = parsed("run_status_running");
    if (frame.t !== "run.status") throw new Error("expected run.status");
    projection.applyRunStatus(frame);
    projection.applyRunStatus(frame);
    expect(projection.runStatusBySession.size).toBe(1);
    expect(projection.runStatusBySession.get("sess-1")).toEqual({
      status: "running",
      runId: "run-7",
    });
  });
});

// ============================================================================
// applyMsgCompleted revision semantics — higher revision wins; a missing revision field
// 视为 1/旧行为不更坏。fixture 驱动一条（msg_completed_with_content_ref，真实的 content_ref 形状），
// 其余是本单自造语料——data-plane-v1.json 只有这一张带 content_ref 的正例，没有"同 message_id 两个
// 不同 revision 先后到达"这种多帧序列场景（fixture 每条 case 都是独立单帧，不表达时序）。
// ============================================================================
describe("MilestoneProjection · applyMsgCompleted revision 语义（msgfix1 T6·M0 §10.7）", () => {
  it("a message without content_ref defaults to revision 1 (old-desktop compat — never carried this field)", () => {
    const projection = new MilestoneProjection();
    projection.applyMsgCompleted({ t: "msg.completed", message_id: 1, role: "assistant", blocks: [{ type: "text", text: "hi" }] });
    expect(projection.messages.get(1)).toEqual({
      messageId: 1,
      role: "assistant",
      blocks: [{ type: "text", text: "hi" }],
      revision: 1,
    });
  });

  it("旧行为不更坏：连续两条都不带 content_ref 的 msg.completed——后到者无条件覆盖（同旧版行为逐字节一致，不因本次改动引入任何比较门槛）", () => {
    const projection = new MilestoneProjection();
    projection.applyMsgCompleted({ t: "msg.completed", message_id: 1, role: "assistant", blocks: [{ type: "text", text: "draft" }] });
    projection.applyMsgCompleted({ t: "msg.completed", message_id: 1, role: "assistant", blocks: [{ type: "text", text: "final" }] });
    expect(projection.messages.get(1)?.blocks).toEqual([{ type: "text", text: "final" }]);
    expect(projection.messages.get(1)?.revision).toBe(1);
  });

  it("a fixture msg.completed with content_ref (msg_completed_with_content_ref) records revision from content_ref.revision, not 1", () => {
    const projection = new MilestoneProjection();
    const frame = parsed("msg_completed_with_content_ref");
    if (frame.t !== "msg.completed") throw new Error("expected msg.completed");
    projection.applyMsgCompleted(frame);
    const entry = projection.messages.get(4821);
    expect(entry?.revision).toBe(1);
    expect(entry?.contentRef).toEqual(frame.content_ref);
    expect(entry?.fullBlocks).toBeUndefined();
  });

  it("revision 高者胜：a lower-revision msg.completed arriving after a higher one is ignored (never downgrades)", () => {
    const projection = new MilestoneProjection();
    const contentRefRev2 = { message_id: 1, revision: 2, content_sha256: "a".repeat(64), total_bytes: 10 };
    const contentRefRev1 = { message_id: 1, revision: 1, content_sha256: "b".repeat(64), total_bytes: 5 };
    projection.applyMsgCompleted({ t: "msg.completed", message_id: 1, role: "assistant", blocks: [{ type: "text", text: "v2" }], content_ref: contentRefRev2 });
    projection.applyMsgCompleted({ t: "msg.completed", message_id: 1, role: "assistant", blocks: [{ type: "text", text: "stale replay of v1" }], content_ref: contentRefRev1 });
    const entry = projection.messages.get(1);
    expect(entry?.revision).toBe(2);
    expect(entry?.blocks).toEqual([{ type: "text", text: "v2" }]);
  });

  it("a genuinely higher revision overwrites content and clears any previously-fetched fullBlocks (stale full text must not linger)", () => {
    const projection = new MilestoneProjection();
    const contentRefRev1 = { message_id: 1, revision: 1, content_sha256: "a".repeat(64), total_bytes: 10 };
    projection.applyMsgCompleted({ t: "msg.completed", message_id: 1, role: "assistant", blocks: [{ type: "text", text: "preview v1" }], content_ref: contentRefRev1 });
    const applied = projection.applyFullText(1, 1, [{ type: "text", text: "full v1" }]);
    expect(applied).toBe(true);
    expect(projection.messages.get(1)?.fullBlocks).toEqual([{ type: "text", text: "full v1" }]);

    const contentRefRev2 = { message_id: 1, revision: 2, content_sha256: "b".repeat(64), total_bytes: 20 };
    projection.applyMsgCompleted({ t: "msg.completed", message_id: 1, role: "assistant", blocks: [{ type: "text", text: "preview v2" }], content_ref: contentRefRev2 });
    const entry = projection.messages.get(1);
    expect(entry?.revision).toBe(2);
    expect(entry?.fullBlocks).toBeUndefined(); // v1 的全文对 v2 内容已经过期，必须清空。
  });

  it("re-applying the exact same revision keeps any already-fetched fullBlocks (same-version redelivery, not a new revision)", () => {
    const projection = new MilestoneProjection();
    const contentRef = { message_id: 1, revision: 1, content_sha256: "a".repeat(64), total_bytes: 10 };
    projection.applyMsgCompleted({ t: "msg.completed", message_id: 1, role: "assistant", blocks: [{ type: "text", text: "preview" }], content_ref: contentRef });
    projection.applyFullText(1, 1, [{ type: "text", text: "full" }]);
    // 同一 revision 的重复到达（relay at-least-once 重投的边缘情形）——不应清掉已拉到的全文。
    projection.applyMsgCompleted({ t: "msg.completed", message_id: 1, role: "assistant", blocks: [{ type: "text", text: "preview" }], content_ref: contentRef });
    expect(projection.messages.get(1)?.fullBlocks).toEqual([{ type: "text", text: "full" }]);
  });

  it("applyFullText is a no-op (returns false) when the message's revision has moved past the fetch's revision (stale fetch discarded, never shown)", () => {
    const projection = new MilestoneProjection();
    const contentRefRev2 = { message_id: 1, revision: 2, content_sha256: "a".repeat(64), total_bytes: 10 };
    projection.applyMsgCompleted({ t: "msg.completed", message_id: 1, role: "assistant", blocks: [{ type: "text", text: "v2" }], content_ref: contentRefRev2 });
    // 一次针对旧 revision=1 的 fetch 在这之后才完成（竞态：拉取在飞期间消息被重新编辑）。
    const applied = projection.applyFullText(1, 1, [{ type: "text", text: "stale full v1" }]);
    expect(applied).toBe(false);
    expect(projection.messages.get(1)?.fullBlocks).toBeUndefined();
  });

  it("applyFullText on an unknown message_id is a no-op (returns false, does not create a phantom entry)", () => {
    const projection = new MilestoneProjection();
    expect(projection.applyFullText(999, 1, [{ type: "text", text: "x" }])).toBe(false);
    expect(projection.messages.has(999)).toBe(false);
  });

  it("applyHistory threads content_ref onto newly-created entries (revision from content_ref.revision) but never touches an existing entry (history only fills gaps)", () => {
    const projection = new MilestoneProjection();
    const contentRef = { message_id: 5, revision: 3, content_sha256: "c".repeat(64), total_bytes: 500 };
    projection.applyHistory({
      t: "history",
      session: "s",
      before_message_id: null,
      messages: [{ message_id: 5, role: "assistant", blocks: [{ type: "text", text: "preview" }], content_ref: contentRef }],
      next_before: null,
    });
    expect(projection.messages.get(5)).toEqual({
      messageId: 5,
      role: "assistant",
      blocks: [{ type: "text", text: "preview" }],
      revision: 3,
      contentRef,
    });

    // 已经存在的 message_id——history 只补缺口，即便自己带了不同的 content_ref 也不覆盖已知内容。
    projection.applyHistory({
      t: "history",
      session: "s",
      before_message_id: null,
      messages: [{ message_id: 5, role: "assistant", blocks: [{ type: "text", text: "different" }], content_ref: { ...contentRef, revision: 99 } }],
      next_before: null,
    });
    expect(projection.messages.get(5)?.revision).toBe(3);
    expect(projection.messages.get(5)?.blocks).toEqual([{ type: "text", text: "preview" }]);
  });
});

// ============================================================================
// The top-level `revision` field takes priority over `content_ref.revision`, falling back to
// content_ref.revision，再无则钉死 1。以下 it.each 用例仍是自造语料（覆盖各字段组合的边界，
// fixture 样张不逐一穷举这些组合）——**但"桌面尚未真正发送这个顶层字段"这句已经过时**：
// msgfix2 U1 起桌面三个 msg.completed 产出点（live 首发/重连补发批/history 分页）恒带顶层
// revision，`data-plane-v1.json` 的 `msg_completed`/`msg_completed_with_content_ref`
// 两张样张都已经带 `revision` 字段，新增的 `activity_summary_first_send`/
// `activity_summary_revision_update` 两张也是（后者 revision:2，见下方
// `MilestoneProjection · fixture-driven` 补测，用真实样张而非自造语料验证顶层 revision 读取）。
// ============================================================================
describe("MilestoneProjection · applyMsgCompleted/applyHistory 顶层 revision 优先级（msgfix1 T7 C2）", () => {
  it("applyMsgCompleted：顶层 revision 在场时优先于 content_ref.revision", () => {
    const projection = new MilestoneProjection();
    const contentRef = { message_id: 1, revision: 1, content_sha256: "a".repeat(64), total_bytes: 10 };
    projection.applyMsgCompleted({
      t: "msg.completed", message_id: 1, role: "assistant",
      blocks: [{ type: "text", text: "v5" }], content_ref: contentRef, revision: 5,
    });
    expect(projection.messages.get(1)?.revision).toBe(5);
  });

  it("applyMsgCompleted：顶层 revision 在场、没有 content_ref 时同样生效（不必等降级为 preview 才有版本号）", () => {
    const projection = new MilestoneProjection();
    projection.applyMsgCompleted({
      t: "msg.completed", message_id: 1, role: "assistant",
      blocks: [{ type: "text", text: "v2" }], revision: 2,
    });
    expect(projection.messages.get(1)?.revision).toBe(2);
  });

  it("applyMsgCompleted：顶层 revision 缺失时回退 content_ref.revision（既有行为不变）", () => {
    const projection = new MilestoneProjection();
    const contentRef = { message_id: 1, revision: 4, content_sha256: "a".repeat(64), total_bytes: 10 };
    projection.applyMsgCompleted({
      t: "msg.completed", message_id: 1, role: "assistant",
      blocks: [{ type: "text", text: "v4" }], content_ref: contentRef,
    });
    expect(projection.messages.get(1)?.revision).toBe(4);
  });

  it("applyMsgCompleted：顶层 revision + content_ref 都缺失时钉死 1（既有行为不变）", () => {
    const projection = new MilestoneProjection();
    projection.applyMsgCompleted({ t: "msg.completed", message_id: 1, role: "assistant", blocks: [] });
    expect(projection.messages.get(1)?.revision).toBe(1);
  });

  it("applyMsgCompleted：顶层 revision 让『高者胜』在未降级消息的窄窗口也生效——旧到达的更新条目被拒", () => {
    const projection = new MilestoneProjection();
    projection.applyMsgCompleted({ t: "msg.completed", message_id: 1, role: "assistant", blocks: [{ type: "text", text: "v3" }], revision: 3 });
    projection.applyMsgCompleted({ t: "msg.completed", message_id: 1, role: "assistant", blocks: [{ type: "text", text: "stale v2" }], revision: 2 });
    const entry = projection.messages.get(1);
    expect(entry?.revision).toBe(3);
    expect(entry?.blocks).toEqual([{ type: "text", text: "v3" }]);
  });

  it("applyHistory：新条目的顶层 revision 优先于 content_ref.revision", () => {
    const projection = new MilestoneProjection();
    const contentRef = { message_id: 7, revision: 1, content_sha256: "b".repeat(64), total_bytes: 20 };
    projection.applyHistory({
      t: "history", session: "s", before_message_id: null,
      messages: [{ message_id: 7, role: "assistant", blocks: [{ type: "text", text: "hist" }], content_ref: contentRef, revision: 6 }],
      next_before: null,
    });
    expect(projection.messages.get(7)?.revision).toBe(6);
  });
});

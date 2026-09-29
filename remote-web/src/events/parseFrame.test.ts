// parseFrame.test.ts — TDD 覆盖 src/events/parseFrame.ts。
//
// ============================================================================
// 覆盖表（T6D1 worker 任务书 §2/§4 硬要求：data-plane-v1.json 30 张样张里哪些被本单消费、以
// 什么方式——不许静默跳层。九类帧口径 = 任务书 §0 原文「event/live/presence/input.ack/
// input.expired/session.index/msg.completed/control.snapshot/replay.head」，其中 session.index/
// msg.completed 是从通用 event 里单独点名的两类，事件其余里程碑（card.created/card.resolved/
// run.status/tool.completed）与「snapshot 应答」都仍在 fixture 自己的 coverage 数组里各占一行；
// 下表按 fixture 的 10 个 category 分组、逐条列消费方式，覆盖口径比任务书九类更细，两边不矛盾。
// ============================================================================
//
// | fixture category                  | 样张数 | 本文件消费方式                                      |
// |-----------------------------------|-------|------------------------------------------------------|
// | session.index                     | 7     | parseFrame() happy-path parsing, deep field comparison（including          |
// |                                    |       | backlog 补的 2 张填充态样张：repo/repo_name 均有值）   |
// | msg.completed                     | 1     | parseFrame() 正例解析，深比对 frame 全字段             |
// | card.created/resolved/run.status/ | 5     | parseFrame() 正例解析，深比对 frame 全字段             |
// | tool.completed（event 其余里程碑）  |       |                                                        |
// | live 四变体+截断变体                | 5     | parseFrame() 正例解析，深比对 frame 全字段（截断变体   |
// |                                    |       | 额外断言 text.length===2048，与 fixture desc 对齐）    |
// | control.snapshot 请求              | 4     | 1 正例 + 3 反例：fixture 配对只验 ok:true/false（fixture  |
// |                                    |       | 没声明 reason）；具体 reason/t 断言拆进独立"协议自造语料"  |
// |                                    |       | describe（审查返工·2026-08 校准）；正例另经              |
// |                                    |       | buildControlSnapshotRequest()→parseFrame() 往返         |
// | snapshot 应答                      | 3     | parseFrame() 正例解析，深比对 frame 全字段（三态：      |
// |                                    |       | 有 partial/无 partial/idle 三 null）                  |
// | presence                          | 1     | parseFrame() 正例解析                                 |
// | input.ack                         | 3     | 2 正例 + 1 反例（缺 command_id）：fixture 配对只验       |
// |                                    |       | ok:true/false；具体 reason 断言同样拆进"协议自造语料"    |
// |                                    |       | describe                                              |
// | input.expired                     | 1     | parseFrame() 正例解析                                 |
// | input.relay_queued                | 1     | parseFrame() 正例解析，深比对 frame 全字段（R6·返工：   |
// |                                    |       | 这张样张之前没有，正例断言曾是"协议自造语料"，现改接     |
// |                                    |       | fixture；4 条缺字段/类型错的反例仍是自造语料，见下方     |
// |                                    |       | describe）                                            |
// | replay.head                       | 1     | parseFrame() 正例解析                                 |
// | msg.fetch/msg.chunk/               | 5     | parseFrame() happy-path parsing, deep field comparison（including          |
// | msg.fetch.error（两 code 分支）/   |       | buildMsgFetchRequest()→parseFrame() 往返；重组状态机/   |
// | 带 content_ref 的 msg.completed    |       | revision 投影另见 msgFetch.test.ts/milestoneProjection. |
// |                                    |       | test.ts，本文件只验这层"帧能不能被正确解析"）            |
// | msgfix2 U1：activity_summary       | 2     | parseFrame() 正例解析，深比对 frame 全字段（由通用       |
// | 首发 + revision 改写重发两态       |       | it.each(validNames) 循环自动覆盖，未单出 it；不是新帧型— |
// |                                    |       | 仍是 msg.completed，blocks[0].type="activity_summary"） |
//
// 39 张样张全部被本文件消费（35 正例逐字段深比对 + 4 反例经 fixture 配对断言 ok:false）。**具体
// 拒绝原因（reason/t）不是 fixture 声明的**——data-plane-v1.json 的 case 形状只有
// `{name, desc, frame, valid, consumers}`，没有 `expect.reason` 字段；4 条 reason 断言拆进独立
// 的"协议自造语料"describe，标题显式标注，不再伪装成 fixture 对拍（审查返工·2026-08 校准）。
// 与 data-plane-v1.json 的 desktop/relay 侧既有消费方（.consumers 字段所列 Rust/JS 测试）并列，
// 不冲突——那些是「生产 builder 产得对」的证据，本文件是「C1 侧真路径消费方」的兑现（任务书
// §0 原文），验的是解析/拒绝这一层。
//
// input.ack 的「未知 outcome」兜底不在本文件——data-plane-v1.json 自己在 coverage 表里记档
// 了这张反样张没出（按字面造会与 relay 生产行为不符），归属 ackOutcome.test.ts 自造语料，
// 不是 fixture 消费方，那份文件顶部另有说明。
//
// ============================================================================
// 变异自证（≥3 条，方法论同 envelope.test.ts：开发期手动改 parseFrame.ts 源码观察对应测试
// 转红，验完已还原，不作为提交内容）——本文件报告里列出实际执行过的 3 条，见 worker 报告 ⑤。
// ============================================================================

import { describe, expect, it } from "vitest";
import { loadFixture } from "../test-support/fixtures.ts";
import {
  buildControlHistoryRequest,
  buildControlSnapshotRequest,
  buildMsgFetchRequest,
  parseFrame,
} from "./parseFrame.ts";

interface DataPlaneCase {
  name: string;
  desc: string;
  frame: unknown;
  valid: boolean;
  consumers: string[];
}

interface DataPlaneFixture {
  version: string;
  cases: DataPlaneCase[];
}

const fixture = loadFixture<DataPlaneFixture>("data-plane-v1.json");
const byName = new Map(fixture.cases.map((entry) => [entry.name, entry]));
const historyFixture = loadFixture<{ request: unknown; response: unknown }>(
  "history-v1.json",
);

function caseFrame(name: string): unknown {
  const entry = byName.get(name);
  if (!entry) throw new Error(`data-plane-v1.json missing case: ${name}`);
  return entry.frame;
}

describe("data-plane-v1.json is the shape this test file was written against", () => {
  it("has exactly 39 cases (coverage table above must be re-audited if this changes)", () => {
    expect(fixture.cases).toHaveLength(39);
  });

  it("has exactly 35 valid:true and 4 valid:false cases", () => {
    const validCount = fixture.cases.filter((entry) => entry.valid).length;
    const invalidCount = fixture.cases.filter((entry) => !entry.valid).length;
    expect(validCount).toBe(35);
    expect(invalidCount).toBe(4);
  });
});

// ---------------------------------------------------------------------------
// 正例：35 张——parseFrame() 必须 ok:true，且返回的 frame 与 fixture 的 frame 逐字段相等
// （不是"能解析就行"，是"解析出来的每个字段值都对得上"）。
// ---------------------------------------------------------------------------
describe("parseFrame() accepts every valid: true data-plane-v1 case and reproduces it field-for-field", () => {
  const validNames = fixture.cases
    .filter((entry) => entry.valid)
    .map((entry) => entry.name);

  it("the fixture really has 35 valid:true cases matching this list's length", () => {
    expect(validNames.length).toBe(35);
  });

  it.each(validNames)("%s", (name) => {
    const result = parseFrame(caseFrame(name));
    expect(result.ok).toBe(true);
    if (!result.ok) throw new Error("unreachable");
    expect(result.frame).toEqual(caseFrame(name));
  });

  it("live_text_delta_truncated: frame.text is exactly 2048 bytes (fixture desc's OUTPUT_TRUNCATE_BYTES claim)", () => {
    const result = parseFrame(caseFrame("live_text_delta_truncated"));
    expect(result.ok).toBe(true);
    if (!result.ok) throw new Error("unreachable");
    if (result.frame.t !== "text_delta") throw new Error("expected text_delta");
    expect(new TextEncoder().encode(result.frame.text).length).toBe(2048);
  });
});

// ---------------------------------------------------------------------------
// 反例：4 张——fixture 驱动的这一半**只断言 fixture 真正声明的东西**：`valid:false` ⟹
// `parseFrame().ok === false`。data-plane-v1.json 的 case 形状只有 `{name, desc, frame, valid,
// consumers}`——没有 `expect.reason`/`expect.t` 这类字段，fixture 本身不对"具体拒绝原因"下
// 任何断言。之前的版本在这里断言了精确的 `reason`/`t`，看起来像是在"跟 fixture 对拍"，实际上
// 那些期望值是本文件自己写的，不是 fixture 声明的——审查返工要求把这两件事显式拆开，不再让
// 自造期望值伪装成 fixture 驱动的断言。
// ---------------------------------------------------------------------------
describe("parseFrame() rejects every valid: false data-plane-v1 case (fixture-paired: only the ok:false verdict, not the specific reason)", () => {
  const invalidNames = fixture.cases
    .filter((entry) => !entry.valid)
    .map((entry) => entry.name);

  it("the fixture really has 4 valid:false cases matching this list's length", () => {
    expect(invalidNames.length).toBe(4);
  });

  it.each(invalidNames)("%s: parseFrame().ok === false", (name) => {
    const result = parseFrame(caseFrame(name));
    expect(result.ok).toBe(false);
  });
});

// ---------------------------------------------------------------------------
// 同样这 4 帧的具体拒绝原因（reason/t）——**协议自造语料，不是 fixture 对拍**：下面的
// `reason`/`t` 期望值是本单按 parseFrame.ts 自身的校验逻辑写的，data-plane-v1.json 并没有声明
// 过它们（该 fixture 只标了 `valid:true/false`）。之所以仍然值得测：这是"哪条具体规则拒绝了
// 这个具体样张"的会归档证据，用于在 parseFrame.ts 校验逻辑变化时能定位到底是哪条分支变了行为
// ——但读者必须清楚这属于本单的协议判断，不是 fixture 权威声明，标题已经明说。
// ---------------------------------------------------------------------------
describe("parseFrame() reject reasons for the 4 known-invalid data-plane-v1 shapes (protocol self-authored expectations, NOT fixture-declared)", () => {
  it("control_snapshot_request_unknown_t_rejected: unknown_t", () => {
    const result = parseFrame(
      caseFrame("control_snapshot_request_unknown_t_rejected"),
    );
    expect(result).toEqual({
      ok: false,
      reason: "unknown_t",
      t: "control.bogus",
    });
  });

  it("control_snapshot_request_missing_session_rejected: malformed_fields", () => {
    const result = parseFrame(
      caseFrame("control_snapshot_request_missing_session_rejected"),
    );
    expect(result).toEqual({
      ok: false,
      reason: "malformed_fields",
      t: "control.snapshot",
    });
  });

  it("control_snapshot_request_session_wrong_type_rejected: malformed_fields", () => {
    const result = parseFrame(
      caseFrame("control_snapshot_request_session_wrong_type_rejected"),
    );
    expect(result).toEqual({
      ok: false,
      reason: "malformed_fields",
      t: "control.snapshot",
    });
  });

  it("input_ack_missing_command_id_no_delete: malformed_fields (client-side judgment call: un-correlatable ack, not a re-assertion of the relay's own 'still broadcasts' behavior)", () => {
    const result = parseFrame(
      caseFrame("input_ack_missing_command_id_no_delete"),
    );
    expect(result).toEqual({
      ok: false,
      reason: "malformed_fields",
      t: "input.ack",
    });
  });
});

describe("parseFrame() input.ack optional reason contract (protocol self-authored, NOT a fixture case)", () => {
  it("omits reason when the raw value is a number or null", () => {
    for (const reason of [123, null]) {
      const result = parseFrame({
        t: "input.ack",
        command_id: "cmd-optional-reason",
        outcome: "failed",
        reason,
      });
      expect(result.ok).toBe(true);
      if (!result.ok) throw new Error("unreachable");
      expect("reason" in result.frame).toBe(false);
      expect(Object.keys(result.frame)).not.toContain("reason");
    }
  });
});

// ---------------------------------------------------------------------------
// msg.completed 的 agent 字段契约——自造语料，不是 fixture 样张：data-plane-v1.json 只有一张
// msg_completed 正例（agent: "Claude"），没有覆盖 agent 非法/省略两种边界。本单锁死
// parseMsgCompleted() 的三条分支：非 string 值整帧拒（含 null）、省略键时解析出的 frame 上
// 该键完全不存在（不是 undefined 值，是键真的没出现——in 判断 + Object.keys 双重锁定，防止
// "frame.agent === undefined" 这种同义反复式误判蒙混过关）。
// ---------------------------------------------------------------------------
describe("parseFrame() msg.completed agent field contract (protocol self-authored, NOT a fixture case)", () => {
  const baseRaw = {
    t: "msg.completed" as const,
    message_id: 101,
    role: "assistant",
    blocks: [{ type: "text", text: "hi" }],
  };

  it("agent: null is rejected as malformed_fields", () => {
    const result = parseFrame({ ...baseRaw, agent: null });
    expect(result).toEqual({
      ok: false,
      reason: "malformed_fields",
      t: "msg.completed",
    });
  });

  it("agent: 123 (non-string) is rejected as malformed_fields", () => {
    const result = parseFrame({ ...baseRaw, agent: 123 });
    expect(result).toEqual({
      ok: false,
      reason: "malformed_fields",
      t: "msg.completed",
    });
  });

  it("omitting the agent key parses ok and the returned frame has no agent key at all", () => {
    const result = parseFrame({ ...baseRaw });
    expect(result.ok).toBe(true);
    if (!result.ok) throw new Error("unreachable");
    expect("agent" in result.frame).toBe(false);
    expect(Object.keys(result.frame)).not.toContain("agent");
  });
});

// ---------------------------------------------------------------------------
// input.relay_queued——正例已改接 fixture（R6·返工·契约样张补位）：data-plane-v1.json 现在有
// `input_relay_queued` 一张样张（`t/command_id/expires_at`，与 relay `ff96acac` 实现一致），
// 走上面通用的"parseFrame() accepts every valid: true..."循环，不再在本 describe 里重复断言正例
// （之前这里自造了一条 `{command_id:"cmd-abc123", expires_at:1_800_000_000_123}` 正例，与
// fixture 补位后内容重复，已删除）。下面 4 条缺字段/字段类型不对的反例——data-plane-v1.json 只
// 覆盖了「合法形状」这一张正例样张，没有对应的反例样张（该 fixture 目前 4 张 valid:false 都属于
// 别的帧型），这 4 条仍是本单自己按 parseFrame.ts 校验逻辑写的协议自造语料，不是 fixture 对拍。
// ---------------------------------------------------------------------------
describe("parseFrame() input.relay_queued malformed-shape rejections (protocol self-authored negative corpus, NOT fixture-declared)", () => {
  it("rejects when command_id is missing: malformed_fields", () => {
    const result = parseFrame({ t: "input.relay_queued", expires_at: 1_000 });
    expect(result).toEqual({ ok: false, reason: "malformed_fields", t: "input.relay_queued" });
  });

  it("rejects when command_id is not a string: malformed_fields", () => {
    const result = parseFrame({ t: "input.relay_queued", command_id: 123, expires_at: 1_000 });
    expect(result).toEqual({ ok: false, reason: "malformed_fields", t: "input.relay_queued" });
  });

  it("rejects when expires_at is missing: malformed_fields", () => {
    const result = parseFrame({ t: "input.relay_queued", command_id: "cmd-abc123" });
    expect(result).toEqual({ ok: false, reason: "malformed_fields", t: "input.relay_queued" });
  });

  it("rejects when expires_at is not a finite number (string/NaN/Infinity): malformed_fields", () => {
    for (const badExpiresAt of ["1000", Number.NaN, Number.POSITIVE_INFINITY]) {
      const result = parseFrame({ t: "input.relay_queued", command_id: "cmd-abc123", expires_at: badExpiresAt });
      expect(result).toEqual({ ok: false, reason: "malformed_fields", t: "input.relay_queued" });
    }
  });
});

// ---------------------------------------------------------------------------
// control.snapshot 请求——round-trip：本模块也是"构造出站请求"的真路径消费方，不只是被动
// 解析。buildControlSnapshotRequest() 的输出必须自己也能被 parseFrame() 接受。
// ---------------------------------------------------------------------------
describe("buildControlSnapshotRequest() round-trips through parseFrame()", () => {
  it("matches the fixture's accepted request shape", () => {
    const built = buildControlSnapshotRequest("s-6");
    expect(built).toEqual(
      caseFrame("control_snapshot_request_accepted_todo_stub"),
    );
    const result = parseFrame(built);
    expect(result).toEqual({ ok: true, frame: built });
  });
});

// ---------------------------------------------------------------------------
// `buildMsgFetchRequest()` round-trip — follows the same convention as `buildControlSnapshotRequest`
// 的既有惯例，构造出站请求也过同一个 parseFrame() 自检。
// ---------------------------------------------------------------------------
describe("buildMsgFetchRequest() round-trips through parseFrame()", () => {
  it("matches the fixture's msg_fetch_request shape", () => {
    const built = buildMsgFetchRequest("sess-1", 4821, 1, 0);
    expect(built).toEqual(caseFrame("msg_fetch_request"));
    const result = parseFrame(built);
    expect(result).toEqual({ ok: true, frame: built });
  });
});

// ---------------------------------------------------------------------------
// Shape boundaries for content_ref/msg.chunk/msg.fetch.error — hand-authored cases, not
// fixture 样张：data-plane-v1.json 只给出"合法 stale_revision"/"合法 not_found"两张正例，没有覆盖
// current_ref 缺失该必带时的反例、其余 code 不该带却带了的反例、content_ref 形状残缺时的反例。
// 与上面 msg.completed agent 字段契约那组 describe 同一取向。
// ---------------------------------------------------------------------------
describe("parseFrame() msg.fetch.error/content_ref shape contract (protocol self-authored, NOT fixture cases)", () => {
  it("stale_revision without current_ref is rejected", () => {
    expect(parseFrame({ t: "msg.fetch.error", code: "stale_revision" })).toEqual({
      ok: false,
      reason: "malformed_fields",
      t: "msg.fetch.error",
    });
  });

  it("stale_revision with a malformed current_ref (missing field) is rejected", () => {
    expect(
      parseFrame({
        t: "msg.fetch.error",
        code: "stale_revision",
        current_ref: { message_id: 1, revision: 1, content_sha256: "a".repeat(64) },
      }),
    ).toEqual({ ok: false, reason: "malformed_fields", t: "msg.fetch.error" });
  });

  // When a non-stale_revision code unexpectedly carries current_ref — do not
  // 整帧拒，宽容忽略这个多余字段（M0 §10.5"仅 stale_revision 必带"这条纪律仍然成立，违反它的
  // 后果从"整帧作废"降级为"丢掉这个不该出现的字段"）。
  it("a non-stale_revision code carrying current_ref parses ok, silently drops the unexpected field (M0 §10.5: only stale_revision may carry it)", () => {
    expect(
      parseFrame({
        t: "msg.fetch.error",
        code: "not_found",
        current_ref: { message_id: 1, revision: 1, content_sha256: "a".repeat(64), total_bytes: 10 },
      }),
    ).toEqual({ ok: true, frame: { t: "msg.fetch.error", code: "not_found" } });
  });

  // A future protocol code outside this client's known six-value enum is no longer rejected outright — treat it as retryable
  // 错误渲染，保留 code 原文（`SessionStreamScreen.tsx` 的 retryable 判定 + `data-error-reason`
  // 属性消费这个原文 code）。
  it("an unknown code parses ok and the raw code text is preserved on the frame (forward-compat: not rejected)", () => {
    expect(parseFrame({ t: "msg.fetch.error", code: "quota_exceeded" })).toEqual({
      ok: true,
      frame: { t: "msg.fetch.error", code: "quota_exceeded" },
    });
  });

  it("an unknown code carrying current_ref parses ok, silently drops the unexpected field", () => {
    expect(
      parseFrame({
        t: "msg.fetch.error",
        code: "quota_exceeded",
        current_ref: { message_id: 1, revision: 1, content_sha256: "a".repeat(64), total_bytes: 10 },
      }),
    ).toEqual({ ok: true, frame: { t: "msg.fetch.error", code: "quota_exceeded" } });
  });

  it("code that is not a string is still rejected (unknown-code tolerance does not weaken basic type checking)", () => {
    expect(parseFrame({ t: "msg.fetch.error", code: 42 })).toEqual({
      ok: false,
      reason: "malformed_fields",
      t: "msg.fetch.error",
    });
  });

  it("msg.completed with a malformed content_ref (non-hex64 content_sha256) is rejected", () => {
    expect(
      parseFrame({
        t: "msg.completed",
        message_id: 1,
        role: "assistant",
        blocks: [],
        content_ref: { message_id: 1, revision: 1, content_sha256: "not-hex", total_bytes: 10 },
      }),
    ).toEqual({ ok: false, reason: "malformed_fields", t: "msg.completed" });
  });

  it("msg.completed omitting content_ref parses ok and the returned frame has no content_ref key at all (old-desktop compat: preview-less messages never carried this field either)", () => {
    const result = parseFrame({ t: "msg.completed", message_id: 1, role: "assistant", blocks: [] });
    expect(result.ok).toBe(true);
    if (!result.ok) throw new Error("unreachable");
    expect("content_ref" in result.frame).toBe(false);
  });

  it("history row with a malformed content_ref rejects the whole history frame", () => {
    const frame = {
      t: "history",
      session: "s",
      before_message_id: null,
      messages: [
        { message_id: 1, role: "user", blocks: [], content_ref: { message_id: 1, revision: 1, content_sha256: "bad", total_bytes: 10 } },
      ],
      next_before: null,
    };
    expect(parseFrame(frame)).toEqual({ ok: false, reason: "malformed_fields", t: "history" });
  });

  it("history row carrying a well-formed content_ref parses ok and round-trips it field-for-field", () => {
    const contentRef = { message_id: 1, revision: 2, content_sha256: "c".repeat(64), total_bytes: 12345 };
    const frame = {
      t: "history",
      session: "s",
      before_message_id: null,
      messages: [{ message_id: 1, role: "assistant", blocks: [{ type: "text", text: "preview" }], content_ref: contentRef }],
      next_before: null,
    };
    expect(parseFrame(frame)).toEqual({ ok: true, frame });
  });

  // Forward-compat guarantee: the desktop (or a future protocol version) may attach an unrecognized field on msg.completed /
  // history row that this client doesn't yet recognize (for example, a not-yet-shipped variant this batch stops short of, held back by a stop condition —
  // still pinned to a hypothetical top-level `revision`; this test deliberately uses a generic hypothetical unknown field name, regardless of whether that variant has actually
  // 已经发布，锁的是"未知字段不拒帧"这条通用行为）时，两口都必须正常解析、只是不把这个未知字段
  // 带进解析出的 frame（同 `agent`/`content_ref` 的既有 optional 惯例：只认识的字段才会出现在
  // 输出上）。
  it("msg.completed with an unknown extra field still parses ok, dropping the unrecognized field from the output frame", () => {
    const result = parseFrame({
      t: "msg.completed",
      message_id: 1,
      role: "assistant",
      blocks: [{ type: "text", text: "hi" }],
      future_unknown_field: "server-added-this-later",
    });
    expect(result.ok).toBe(true);
    if (!result.ok) throw new Error("unreachable");
    expect(result.frame).toEqual({
      t: "msg.completed",
      message_id: 1,
      role: "assistant",
      blocks: [{ type: "text", text: "hi" }],
    });
    expect("future_unknown_field" in result.frame).toBe(false);
  });

  it("history row with an unknown extra field still parses ok, dropping the unrecognized field from the output row", () => {
    const result = parseFrame({
      t: "history",
      session: "s",
      before_message_id: null,
      messages: [
        {
          message_id: 1,
          role: "user",
          blocks: [{ type: "text", text: "hi" }],
          future_unknown_field: "server-added-this-later",
        },
      ],
      next_before: null,
    });
    expect(result.ok).toBe(true);
    if (!result.ok) throw new Error("unreachable");
    if (result.frame.t !== "history") throw new Error("expected history");
    expect(result.frame.messages).toEqual([{ message_id: 1, role: "user", blocks: [{ type: "text", text: "hi" }] }]);
    expect("future_unknown_field" in result.frame.messages[0]).toBe(false);
  });
});

describe("history-v1.json request/response contract", () => {
  it("builds and parses the fixture request exactly", () => {
    const built = buildControlHistoryRequest("sess-history", 120);
    expect(built).toEqual(historyFixture.request);
    expect(parseFrame(historyFixture.request)).toEqual({
      ok: true,
      frame: historyFixture.request,
    });
  });

  it("parses the shared desktop response fixture field-for-field", () => {
    expect(parseFrame(historyFixture.response)).toEqual({
      ok: true,
      frame: historyFixture.response,
    });
  });

  it.each([
    { t: "control.history", session: "s", before_message_id: "1" },
    {
      t: "history",
      session: 1,
      before_message_id: null,
      messages: [],
      next_before: null,
    },
    {
      t: "history",
      session: "s",
      before_message_id: null,
      messages: {},
      next_before: null,
    },
    {
      t: "history",
      session: "s",
      before_message_id: null,
      messages: [{ message_id: Infinity, role: "user", blocks: [] }],
      next_before: null,
    },
    {
      t: "history",
      session: "s",
      before_message_id: null,
      messages: [{ message_id: 1, role: 2, blocks: [] }],
      next_before: null,
    },
    {
      t: "history",
      session: "s",
      before_message_id: null,
      messages: [{ message_id: 1, role: "user", blocks: {} }],
      next_before: null,
    },
    {
      t: "history",
      session: "s",
      before_message_id: null,
      messages: [],
      next_before: "older",
    },
  ])("rejects malformed history fields: %j", (frame) => {
    expect(parseFrame(frame)).toEqual({
      ok: false,
      reason: "malformed_fields",
      t: frame.t,
    });
  });
});

// ---------------------------------------------------------------------------
// 未知 t / 异形帧不崩溃——strict 但不 throw（brief §2 硬要求）。这几条不是 fixture 样张
// （data-plane-v1.json 没有覆盖"顶层不是对象"/"缺 t 字段"这两种最基础的异形输入），是本单
// 自造的边界语料，补 fixture 没测到的两个入口分支。
// ---------------------------------------------------------------------------
describe("parseFrame() never throws on malformed top-level input", () => {
  it.each([null, undefined, 42, "not an object", [], true])(
    "rejects non-object input: %j",
    (input) => {
      expect(() => parseFrame(input)).not.toThrow();
      expect(parseFrame(input)).toEqual({
        ok: false,
        reason: "not_an_object",
        t: null,
      });
    },
  );

  it("rejects an object missing t", () => {
    expect(parseFrame({ foo: "bar" })).toEqual({
      ok: false,
      reason: "missing_t",
      t: null,
    });
  });

  it("rejects an object whose t is not a string", () => {
    expect(parseFrame({ t: 42 })).toEqual({
      ok: false,
      reason: "missing_t",
      t: null,
    });
  });

  it("rejects a completely unknown t without throwing", () => {
    expect(parseFrame({ t: "something.nobody.invented" })).toEqual({
      ok: false,
      reason: "unknown_t",
      t: "something.nobody.invented",
    });
  });
});

describe("session.index full nullable preview/activity extension", () => {
  function fullFrameWithRow(rowPatch: Record<string, unknown>) {
    return {
      t: "session.index",
      full: true,
      sessions: [
        {
          id: "s-1",
          title: "Session",
          repo_id: "repo-a",
          archived: false,
          status: null,
          run_id: null,
          updated_at: 1000,
          ...rowPatch,
        },
      ],
    };
  }

  it.each([
    ["missing", {}],
    ["null", { last_msg_preview: null, last_activity_at: null }],
    ["values", { last_msg_preview: "latest message", last_activity_at: 2000 }],
  ])("accepts both new fields when %s", (_name, rowPatch) => {
    const frame = fullFrameWithRow(rowPatch);
    expect(parseFrame(frame)).toEqual({ ok: true, frame });
  });

  it.each([
    ["last_msg_preview", { last_msg_preview: 42 }],
    ["last_activity_at", { last_activity_at: "yesterday" }],
    [
      "last_activity_at non-finite",
      { last_activity_at: Number.POSITIVE_INFINITY },
    ],
  ])("rejects invalid %s", (_name, rowPatch) => {
    expect(parseFrame(fullFrameWithRow(rowPatch))).toEqual({
      ok: false,
      reason: "malformed_fields",
      t: "session.index",
    });
  });

  // M2-4x：row 上的 repo_name——同 last_msg_preview/last_activity_at 一样的 optional 惯例。
  it.each([
    ["missing", {}],
    ["null", { repo_name: null }],
    ["a real name", { repo_name: "Acme Corp" }],
  ])("accepts repo_name when %s", (_name, rowPatch) => {
    const frame = fullFrameWithRow(rowPatch);
    expect(parseFrame(frame)).toEqual({ ok: true, frame });
  });

  it("rejects non-string/non-null repo_name", () => {
    expect(parseFrame(fullFrameWithRow({ repo_name: 42 }))).toEqual({
      ok: false,
      reason: "malformed_fields",
      t: "session.index",
    });
  });
});

// ---------------------------------------------------------------------------
// M2-4x：全量快照顶层"当前被远程的项目"摘要（`repo`）——三态：键缺失（旧桌面兼容）、显式
// `null`（fail-closed，没有可判定的 active repo）、`{id, name}`（`name` 本身也可能是 null，见
// 桌面 `active_repo_summary_for_snapshot` 头注）。
// ---------------------------------------------------------------------------
describe("session.index full top-level repo summary extension", () => {
  function fullFrameWithRepo(repoPatch: object): Record<string, unknown> {
    return {
      t: "session.index",
      full: true,
      sessions: [],
      ...repoPatch,
    };
  }

  it("omitting the repo key parses ok and the returned frame has no repo key at all", () => {
    const frame = fullFrameWithRepo({});
    const result = parseFrame(frame);
    expect(result.ok).toBe(true);
    if (
      !result.ok ||
      result.frame.t !== "session.index" ||
      !result.frame.full
    ) {
      throw new Error("unreachable");
    }
    expect("repo" in result.frame).toBe(false);
  });

  it("repo: null round-trips as explicit null", () => {
    const frame = fullFrameWithRepo({ repo: null });
    expect(parseFrame(frame)).toEqual({ ok: true, frame });
  });

  it("repo: {id, name} round-trips", () => {
    const frame = fullFrameWithRepo({
      repo: { id: "repo-a", name: "Acme Corp" },
    });
    expect(parseFrame(frame)).toEqual({ ok: true, frame });
  });

  it("repo: {id, name: null} round-trips (active repo known, name unavailable)", () => {
    const frame = fullFrameWithRepo({ repo: { id: "repo-a", name: null } });
    expect(parseFrame(frame)).toEqual({ ok: true, frame });
  });

  it.each([
    ["not an object", { repo: "repo-a" }],
    ["missing id", { repo: { name: "Acme Corp" } }],
    ["non-string id", { repo: { id: 1, name: "Acme Corp" } }],
    ["non-string/non-null name", { repo: { id: "repo-a", name: 42 } }],
  ])("rejects malformed repo shape: %s", (_name, repoPatch) => {
    expect(parseFrame(fullFrameWithRepo(repoPatch))).toEqual({
      ok: false,
      reason: "malformed_fields",
      t: "session.index",
    });
  });
});

// ---------------------------------------------------------------------------
// M2-4x：created op 的 session.repo_name——同一套 optional 惯例（省略/null/字符串三态合法，
// 非字符串非 null 拒）。
// ---------------------------------------------------------------------------
describe("session.index created op repo_name extension", () => {
  function createdFrameWithSessionPatch(sessionPatch: Record<string, unknown>) {
    return {
      t: "session.index",
      op: "created",
      full: false,
      session: {
        id: "s-3",
        title: "New session",
        repo_id: "repo-a",
        namespace_id: "ns-1",
        archived: false,
        ...sessionPatch,
      },
    };
  }

  it.each([
    ["missing", {}],
    ["null", { repo_name: null }],
    ["a real name", { repo_name: "Acme Corp" }],
  ])("accepts repo_name when %s", (_name, sessionPatch) => {
    const frame = createdFrameWithSessionPatch(sessionPatch);
    expect(parseFrame(frame)).toEqual({ ok: true, frame });
  });

  it("rejects non-string/non-null repo_name", () => {
    expect(parseFrame(createdFrameWithSessionPatch({ repo_name: 42 }))).toEqual(
      {
        ok: false,
        reason: "malformed_fields",
        t: "session.index",
      },
    );
  });
});

// ---------------------------------------------------------------------------
// snapshot 应答形状不变量（M0 §3 v1.8.12）——data-plane-v1.json 只给了 3 张正例（三态都合法），
// 没有反例样张；这里补自造反例语料钉死"through_run_seq=0 被废除""三 null 不能拆开"两条硬约束
// （不是 fixture 消费，是本单按条文自证——上面覆盖表已注明这块归属）。
// ---------------------------------------------------------------------------
describe("parseFrame() rejects snapshot-response shape violations (M0 §3 v1.8.12 invariants, self-authored negative corpus)", () => {
  it("through_run_seq=0 is rejected (the abolished value)", () => {
    const result = parseFrame({
      t: "snapshot",
      session: "s-1",
      run_id: "run-1",
      through_run_seq: 0,
      partial_msg: null,
    });
    expect(result).toEqual({
      ok: false,
      reason: "malformed_fields",
      t: "snapshot",
    });
  });

  it("running with through_run_seq negative is rejected", () => {
    const result = parseFrame({
      t: "snapshot",
      session: "s-1",
      run_id: "run-1",
      through_run_seq: -1,
      partial_msg: null,
    });
    expect(result).toEqual({
      ok: false,
      reason: "malformed_fields",
      t: "snapshot",
    });
  });

  it("running with non-integer through_run_seq is rejected", () => {
    const result = parseFrame({
      t: "snapshot",
      session: "s-1",
      run_id: "run-1",
      through_run_seq: 1.5,
      partial_msg: null,
    });
    expect(result).toEqual({
      ok: false,
      reason: "malformed_fields",
      t: "snapshot",
    });
  });

  it("run_id null but through_run_seq non-null is rejected (idle must be all-three-null)", () => {
    const result = parseFrame({
      t: "snapshot",
      session: "s-1",
      run_id: null,
      through_run_seq: 3,
      partial_msg: null,
    });
    expect(result).toEqual({
      ok: false,
      reason: "malformed_fields",
      t: "snapshot",
    });
  });

  it("run_id null but partial_msg non-null is rejected (idle must be all-three-null)", () => {
    const result = parseFrame({
      t: "snapshot",
      session: "s-1",
      run_id: null,
      through_run_seq: null,
      partial_msg: { role: "assistant", blocks: [] },
    });
    expect(result).toEqual({
      ok: false,
      reason: "malformed_fields",
      t: "snapshot",
    });
  });

  it("run_id non-null but through_run_seq null is rejected", () => {
    const result = parseFrame({
      t: "snapshot",
      session: "s-1",
      run_id: "run-1",
      through_run_seq: null,
      partial_msg: null,
    });
    expect(result).toEqual({
      ok: false,
      reason: "malformed_fields",
      t: "snapshot",
    });
  });
});

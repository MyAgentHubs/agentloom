// decisionCardView.test.ts — TDD 覆盖 decisionCardView.ts::coerceDecisionCardBlock /
// groupDecisionCardsIntoTurns。`.test.ts` 落 vitest "logic" project（node 环境，不碰 DOM）——
// DOM 级验证（RunLeadTurn/DecisionCard 真的渲出来、chooser 真的 disabled）在
// `SessionStreamScreen.test.tsx` 里，这里只测这两个纯函数自己的输入输出契约。

import { describe, expect, it } from "vitest";
import { coerceDecisionCardBlock, groupDecisionCardsIntoTurns } from "./decisionCardView.ts";
import type { SessionStreamDecisionCard } from "./streamSource.ts";

function validRaw(overrides: Partial<Record<string, unknown>> = {}): Record<string, unknown> {
  return {
    type: "decision_card",
    decision_id: "dec-1",
    kind: "ask",
    question: "继续吗？",
    options: ["是", "否"],
    recommended: "是",
    rationale: "理由",
    payload: { foo: "bar" },
    source_run_id: "run-1",
    status: "pending",
    chosen_option: null,
    created_at: 1700000000000,
    ...overrides,
  };
}

describe("coerceDecisionCardBlock", () => {
  it("extracts a fully-formed block unchanged (field-for-field)", () => {
    const result = coerceDecisionCardBlock(validRaw());
    expect(result).toEqual({
      type: "decision_card",
      decision_id: "dec-1",
      kind: "ask",
      question: "继续吗？",
      options: ["是", "否"],
      recommended: "是",
      rationale: "理由",
      payload: { foo: "bar" },
      source_run_id: "run-1",
      status: "pending",
      chosen_option: null,
      created_at: 1700000000000,
    });
  });

  it.each([
    ["decision_id", { decision_id: 42 }],
    ["question", { question: null }],
    ["options (not an array)", { options: "是,否" }],
    ["options (array with a non-string element)", { options: ["是", 2] }],
    ["source_run_id", { source_run_id: undefined }],
  ])("returns null when %s is malformed (fail-closed, no crash)", (_label, overrides) => {
    expect(coerceDecisionCardBlock(validRaw(overrides))).toBeNull();
  });

  it("defaults status to 'pending' when missing or not a known value", () => {
    expect(coerceDecisionCardBlock(validRaw({ status: undefined }))?.status).toBe("pending");
    expect(coerceDecisionCardBlock(validRaw({ status: "some-unknown-future-status" }))?.status).toBe("pending");
  });

  it("only 'dispatch_confirm' maps kind to dispatch_confirm; anything else (including missing) defaults to 'ask'", () => {
    expect(coerceDecisionCardBlock(validRaw({ kind: "dispatch_confirm" }))?.kind).toBe("dispatch_confirm");
    expect(coerceDecisionCardBlock(validRaw({ kind: undefined }))?.kind).toBe("ask");
    expect(coerceDecisionCardBlock(validRaw({ kind: "something-else" }))?.kind).toBe("ask");
  });

  it("recommended/rationale/chosen_option default to null when not a string; payload defaults to null when nullish", () => {
    const result = coerceDecisionCardBlock(
      validRaw({ recommended: undefined, rationale: 5, payload: undefined, chosen_option: undefined }),
    );
    expect(result?.recommended).toBeNull();
    expect(result?.rationale).toBeNull();
    expect(result?.payload).toBeNull();
    expect(result?.chosen_option).toBeNull();
  });

  it("created_at defaults to 0 when not a number", () => {
    expect(coerceDecisionCardBlock(validRaw({ created_at: "not-a-number" }))?.created_at).toBe(0);
  });
});

describe("groupDecisionCardsIntoTurns", () => {
  function card(decisionId: string, block: Record<string, unknown> | undefined): SessionStreamDecisionCard {
    return { decisionId, block };
  }

  it("empty input yields empty output", () => {
    expect(groupDecisionCardsIntoTurns([])).toEqual([]);
  });

  it("groups cards sharing the same source_run_id into a single LeadTurnView", () => {
    const turns = groupDecisionCardsIntoTurns([
      card("dec-a", validRaw({ decision_id: "dec-a", source_run_id: "run-x" })),
      card("dec-b", validRaw({ decision_id: "dec-b", source_run_id: "run-x", question: "第二问" })),
    ]);
    expect(turns).toHaveLength(1);
    expect(turns[0].runId).toBe("run-x");
    expect(turns[0].decisionCards).toHaveLength(2);
    expect(turns[0].lead).toBeNull();
    expect(turns[0].members).toEqual([]);
    expect(turns[0].codingTask).toBeNull();
    expect(turns[0].verdict).toBeNull();
  });

  it("different source_run_id produce separate turns, in first-seen order", () => {
    const turns = groupDecisionCardsIntoTurns([
      card("dec-a", validRaw({ decision_id: "dec-a", source_run_id: "run-second-seen-later" })),
      card("dec-b", validRaw({ decision_id: "dec-b", source_run_id: "run-first-seen" })),
      card("dec-c", validRaw({ decision_id: "dec-c", source_run_id: "run-second-seen-later" })),
    ]);
    expect(turns.map((t) => t.runId)).toEqual(["run-second-seen-later", "run-first-seen"]);
    expect(turns[0].decisionCards).toHaveLength(2);
    expect(turns[1].decisionCards).toHaveLength(1);
  });

  it("cards with block===undefined (card.resolved with no matching card.created) are skipped entirely", () => {
    const turns = groupDecisionCardsIntoTurns([card("dec-orphan", undefined)]);
    expect(turns).toEqual([]);
  });

  it("cards that fail coercion (malformed block) are skipped, not thrown", () => {
    const turns = groupDecisionCardsIntoTurns([card("dec-bad", { type: "decision_card", question: "missing required fields" })]);
    expect(turns).toEqual([]);
  });
});

// ============================================================================
// T6f3 · localOverrides（本地本机在飞回答的展示态覆盖——submitting/failed，从不 chosen）
// ============================================================================

describe("groupDecisionCardsIntoTurns: localOverrides", () => {
  function card(decisionId: string, block: Record<string, unknown> | undefined): SessionStreamDecisionCard {
    return { decisionId, block };
  }

  it("server status 仍是 pending 时，localOverrides 里的 submitting 会覆盖显示状态，且连带覆盖 chosen_option 为本机选的选项", () => {
    const turns = groupDecisionCardsIntoTurns(
      [card("dec-1", validRaw({ status: "pending", options: ["是", "否"] }))],
      new Map([["dec-1", { status: "submitting", option: "否" }]]),
    );
    expect(turns[0].decisionCards[0]).toMatchObject({ status: "submitting", chosen_option: "否" });
  });

  it("server status 仍是 pending 时，localOverrides 里的 failed 会覆盖显示状态，chosen_option 同样覆盖为本机选的选项（不许退回 options[0]）", () => {
    const turns = groupDecisionCardsIntoTurns(
      [card("dec-1", validRaw({ status: "pending", options: ["是", "否"] }))],
      new Map([["dec-1", { status: "failed", option: "否" }]]),
    );
    expect(turns[0].decisionCards[0]).toMatchObject({ status: "failed", chosen_option: "否" });
  });

  it("server status 已经不是 pending（真正的 card.resolved 落地）——不本地臆断：覆盖值被忽略，服务器真相（含 chosen_option）原样展示", () => {
    const turns = groupDecisionCardsIntoTurns(
      [card("dec-1", validRaw({ status: "chosen", chosen_option: "是" }))],
      new Map([["dec-1", { status: "submitting", option: "否" }]]),
    );
    expect(turns[0].decisionCards[0]).toMatchObject({ status: "chosen", chosen_option: "是" });
  });

  it("localOverrides 里没有这个 decisionId 的条目——status/chosen_option 原样是服务器的 pending 值，不受影响", () => {
    const turns = groupDecisionCardsIntoTurns(
      [card("dec-1", validRaw({ status: "pending" }))],
      new Map([["dec-other", { status: "submitting", option: "是" }]]),
    );
    expect(turns[0].decisionCards[0]).toMatchObject({ status: "pending", chosen_option: null });
  });

  it("不传 localOverrides（undefined）——行为与旧签名完全一致，不报错", () => {
    const turns = groupDecisionCardsIntoTurns([card("dec-1", validRaw({ status: "pending" }))]);
    expect(turns[0].decisionCards[0]).toMatchObject({ status: "pending" });
  });
});

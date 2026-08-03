import { describe, it, expect } from "vitest";
import {
  shouldEnterCodingLoop,
  nextCodingAction,
  phaseAfterVerify,
  phaseAfterFinalize,
  isLandingBlockedError,
  type CodingState,
} from "../codingLoop";
import type { TeamRun } from "../../types/agent";

const mkRun = (over: Partial<TeamRun["members"][0]>): TeamRun => ({
  run_id: "r1",
  goal: null,
  lead: "Claude",
  members: [
    {
      participant_id: "p1",
      assignment_id: "m1",
      task_id: "t1",
      name: "Codex",
      status: "done",
      sub: "改 foo",
      steps_total: 3,
      steps_done: 3,
      cost_usd: 0,
      input_tokens: 0,
      output_tokens: 0,
      failed: false,
      blocks: [],
      // P1 折入：changed_files 是 ChangedFile[] 对象数组·不是 string[]（真实形状·防假绿）
      result: {
        status: "done",
        anchor: { base_sha: "base123", generated_from: "worktree" },
        changed_files: [{ path: "foo.rs", insertions: 1, deletions: 0 }],
        command_evidence: [],
      } as any,
      ...over,
    } as any,
    ...([] as any),
  ],
});

describe("shouldEnterCodingLoop", () => {
  it("单 worker + done + 有改动(含 path) + base_sha → 进闭环", () => {
    expect(shouldEnterCodingLoop(mkRun({}))).toBe(true);
  });
  it("无改动 → 不进（走原 summary）", () => {
    const r = mkRun({});
    (r.members[0].result as any).changed_files = [];
    expect(shouldEnterCodingLoop(r)).toBe(false);
  });
  it("status 非 done（needs_input/running）→ 不进（P2 折入·收紧触发）", () => {
    expect(shouldEnterCodingLoop(mkRun({ status: "needs_input" }))).toBe(false);
  });
  it("缺 anchor.base_sha → 不进（finalize 没 base 无法跑·P2 折入）", () => {
    const r = mkRun({});
    (r.members[0].result as any).anchor = { generated_from: "x" };
    expect(shouldEnterCodingLoop(r)).toBe(false);
  });
  it("worker 失败 → 不进", () => {
    expect(
      shouldEnterCodingLoop(mkRun({ failed: true, status: "failed" })),
    ).toBe(false);
  });
  it("多 worker → 不进（v1 只单 worker）", () => {
    const r = mkRun({});
    r.members.push({ ...r.members[0], assignment_id: "m2" } as any);
    expect(shouldEnterCodingLoop(r)).toBe(false);
  });
});

describe("phaseAfterVerify（verdict 分叉进纯函数·P3 折入·别压给 GUI）", () => {
  it("passed → merging", () => {
    expect(phaseAfterVerify("passed")).toBe("merging");
  });
  it("非 passed → verify_failed", () => {
    expect(phaseAfterVerify("non_zero_exit")).toBe("verify_failed");
    expect(phaseAfterVerify("dirty_after_test")).toBe("verify_failed");
  });
});

describe("phaseAfterFinalize（in-place 事实分叉）", () => {
  it("in-place → applied（finalize 后不进 verify/merge）", () => {
    expect(phaseAfterFinalize("", true)).toBe("applied");
    expect(phaseAfterFinalize("cargo test", true)).toBe("applied");
  });
  it("app 域受管 workspace → merging", () => {
    // 旧 fail-closed 根因（verifyCmd 空 → landing_blocked）已删除。
    expect(phaseAfterFinalize("", false)).toBe("merging");
    expect(phaseAfterFinalize("npm test", false)).toBe("merging");
  });
});

describe("isLandingBlockedError", () => {
  it.each([
    "landing.protectedPath",
    "landing.noEvidence",
    "landing.scopeExceeded",
    "landing.l1NotGreen",
  ])("识别 landing code：%s", (code) => {
    expect(isLandingBlockedError(`AL_ERR:${code}`)).toBe(true);
  });

  it("不把本族其余 code 归为 landing_blocked", () => {
    expect(isLandingBlockedError("AL_ERR:merge.stagingConflict")).toBe(false);
    expect(isLandingBlockedError("AL_ERR:finalize.noChanges")).toBe(false);
  });

  it("保留旧中文串与 fast-forward 兜底", () => {
    expect(
      isLandingBlockedError(
        "落地前检查未通过：改动超出 worker 声明 package.json",
      ),
    ).toBe(true);
    expect(isLandingBlockedError("fatal: Not possible to fast-forward")).toBe(
      true,
    );
  });
});

describe("nextCodingAction", () => {
  const base: CodingState = {
    runId: "r1",
    sessionId: "s1",
    assignmentId: "m1",
    baseSha: "base123",
    phase: "finalizing",
    artifactId: null,
    verifyCmd: "cargo test",
    isInPlace: false,
  };
  it("finalizing → finalize 命令", () => {
    expect(nextCodingAction(base).kind).toBe("finalize");
  });
  it("applied 是终态 → done（Local finalize 后直达 applied 即 done）", () => {
    expect(nextCodingAction({ ...base, phase: "applied" })).toEqual({
      kind: "done",
    });
  });
  it("ask_verify 用户确认 → verify 命令", () => {
    const a = nextCodingAction({
      ...base,
      phase: "verifying",
      artifactId: "art-1",
    });
    expect(a).toEqual({
      kind: "verify",
      artifactId: "art-1",
      cmd: "cargo test",
    });
  });
  it("ask_apply 用户选先放着 → 收尾 shelved（无后端动作）", () => {
    expect(nextCodingAction({ ...base, phase: "shelved" })).toEqual({
      kind: "done",
    });
  });
  it("merging → merge（合并进暂存仍自动）", () => {
    const a = nextCodingAction({
      ...base,
      phase: "merging",
      artifactId: "art-1",
    });
    expect(a).toEqual({ kind: "merge", artifactId: "art-1" });
  });
  it("applying → wait（b2b：关自动落地·merge 进 staging 后停等用户点改动条）", () => {
    const b = nextCodingAction({ ...base, phase: "applying" });
    expect(b).toEqual({ kind: "wait" });
  });
});

import { describe, it, expect, vi } from "vitest";
import { advanceCodingLoop } from "../codingLoopDriver";
import {
  phaseAfterVerify,
  selectCodingVerifier,
  type CodingState,
} from "../codingLoop";

const base: CodingState = {
  runId: "r1",
  sessionId: "s1",
  assignmentId: "m1",
  baseSha: "b",
  phase: "finalizing",
  artifactId: null,
  verifyCmd: "cargo test",
  isInPlace: false,
};

describe("advanceCodingLoop", () => {
  it("repo finalizing -> 调 finalize·得 artifactId·信任落地跳 verifying 直接 merging（T4 trust-land）", async () => {
    const inv = vi.fn().mockResolvedValue("art-1") as any;
    const next = await advanceCodingLoop(base, inv);
    expect(inv).toHaveBeenCalledWith(
      "finalize_member_artifact",
      expect.objectContaining({ baseSha: "b" }),
    );
    // 旧契约：有 verifier → "verifying"；新契约（T4）：repo 信任落地·跳 verifying → "merging"。
    expect(next).toMatchObject({ artifactId: "art-1", phase: "merging" });
  });
  it("Local finalizing -> finalize 即落地·直达 applied·landedHead=run_landing_info 的真 sha（T7）", async () => {
    // T7：finalize 返回 artifact_id（run-…）·不是 git sha；landedHead 须取 run_landing_info 的真 landed_head。
    const inv = vi.fn().mockImplementation((cmd: string) => {
      if (cmd === "finalize_member_artifact")
        return Promise.resolve("run-0001");
      if (cmd === "run_landing_info")
        return Promise.resolve({
          landedHead: "deadbeefcafef00d",
          preHead: "base-1",
          filesChanged: 1,
          insertions: 3,
          deletions: 0,
          files: [{ path: "README.md", insertions: 3, deletions: 0 }],
        });
      return Promise.resolve("");
    }) as any;
    const next = await advanceCodingLoop({ ...base, isInPlace: true }, inv);
    // in-place 只调 finalize + run_landing_info·不进 verify/merge/apply。
    expect(inv).toHaveBeenCalledWith(
      "finalize_member_artifact",
      expect.objectContaining({ baseSha: "b" }),
    );
    expect(inv).toHaveBeenCalledWith(
      "run_landing_info",
      expect.objectContaining({ sessionId: "s1", runId: "r1" }),
    );
    expect(inv).not.toHaveBeenCalledWith(
      "run_verifier_artifact",
      expect.anything(),
    );
    expect(inv).not.toHaveBeenCalledWith(
      "apply_run_to_current_branch",
      expect.anything(),
    );
    expect(next).toMatchObject({
      artifactId: "run-0001",
      landedHead: "deadbeefcafef00d", // 真 git sha·不是 run-…
      phase: "applied",
    });
  });
  it("Local finalize 后 run_landing_info 抛错·不降级为 error·仍 applied·landedHead 兜底 artifact_id（落地已成·只是 sha 解析失败）", async () => {
    // RISK 守护：finalize 已物理落地（Local in-place）·此后 run_landing_info 抛错（如 DB lock）
    // 绝不能让异常冒出去把 phase 降成 error/landing_blocked——否则 UI 显示 error·无「已落地」·无 undo。
    const inv = vi.fn().mockImplementation((cmd: string) => {
      if (cmd === "finalize_member_artifact")
        return Promise.resolve("run-0001");
      if (cmd === "run_landing_info")
        return Promise.reject(new Error("db locked"));
      return Promise.resolve("");
    }) as any;
    const next = await advanceCodingLoop({ ...base, isInPlace: true }, inv);
    expect(inv).toHaveBeenCalledWith(
      "run_landing_info",
      expect.objectContaining({ sessionId: "s1", runId: "r1" }),
    );
    // 落地已成·只是 cosmetic sha 解析失败 → 仍 applied·landedHead 退回 artifact_id。
    expect(next).toMatchObject({
      artifactId: "run-0001",
      landedHead: "run-0001",
      phase: "applied",
    });
  });
  it("verifying passed -> merging", async () => {
    const inv = vi
      .fn()
      .mockImplementation((cmd: string) =>
        cmd === "latest_verification_for_artifact_cmd"
          ? Promise.resolve({ verdict: "passed" })
          : Promise.resolve("ok"),
      ) as any;
    const next = await advanceCodingLoop(
      { ...base, phase: "verifying", artifactId: "art-1" },
      inv,
    );
    expect(next.phase).toBe("merging");
  });
  it("verifying 非 passed -> verify_failed", async () => {
    const inv = vi
      .fn()
      .mockImplementation((cmd: string) =>
        cmd === "latest_verification_for_artifact_cmd"
          ? Promise.resolve({ verdict: "non_zero_exit" })
          : Promise.resolve("v"),
      ) as any;
    const next = await advanceCodingLoop(
      { ...base, phase: "verifying", artifactId: "art-1" },
      inv,
    );
    expect(next.phase).toBe("verify_failed");
  });
  it("merging -> applying（merge 后）", async () => {
    const inv = vi.fn().mockResolvedValue("mc-1") as any;
    const next = await advanceCodingLoop(
      { ...base, phase: "merging", artifactId: "art-1" },
      inv,
    );
    expect(inv).toHaveBeenCalledWith("merge_artifact_to_staging", {
      artifactId: "art-1",
    });
    expect(next.phase).toBe("applying");
  });
  it("applying -> 停隔离区（b2b 关自动落地·driver 不再自动调 apply·phase 保持 applying）", async () => {
    const inv = vi.fn().mockResolvedValue("newhead") as any;
    const next = await advanceCodingLoop({ ...base, phase: "applying" }, inv);
    // merge 进 staging 后停在 applying 等用户点改动条·driver 单步不再自动落地。
    expect(inv).not.toHaveBeenCalledWith(
      "apply_run_to_current_branch",
      expect.anything(),
    );
    expect(next.phase).toBe("applying");
  });
});

describe("T-C3b b2a verify-first coding loop", () => {
  const b2aBase = (overrides: Partial<CodingState> = {}): CodingState => ({
    runId: "run-1",
    sessionId: "s1",
    assignmentId: "a1",
    taskId: "task-1",
    baseSha: "base",
    phase: "finalizing",
    artifactId: null,
    verifyCmd: "",
    isInPlace: false,
    ...overrides,
  });

  it("repo finalize 后有 verifier 信任落地·跳 verifying 直接 merging（T4 反转：旧为 verifying）", async () => {
    const inv = vi.fn(async (cmd: string) => {
      if (cmd === "finalize_member_artifact") return "art-1";
      throw new Error(cmd);
    }) as any;
    const next = await advanceCodingLoop(
      b2aBase({ verifyCmd: "npm test" }),
      inv,
    );
    // 旧契约：有 verifier → phase "verifying"；新契约（T4）：repo → "merging"（跳 verifying）。
    expect(next).toMatchObject({
      artifactId: "art-1",
      phase: "merging",
      verifyCmd: "npm test",
    });
  });

  it("repo finalize 后没有 verifier 不再 fail-closed·trust-land 进 merging（T4 反转：旧为 landing_blocked）", async () => {
    const inv = vi.fn(async (cmd: string) => {
      if (cmd === "finalize_member_artifact") return "art-1";
      throw new Error(cmd);
    }) as any;
    const next = await advanceCodingLoop(b2aBase(), inv);
    // 旧契约（根因）：verifyCmd 空 → "landing_blocked"（fail-closed）；新契约（T4）：repo → "merging"。
    expect(next).toMatchObject({
      artifactId: "art-1",
      phase: "merging",
    });
  });

  it("passed verification 进入 merging，非 passed 进入 verify_failed", () => {
    expect(phaseAfterVerify("passed")).toBe("merging");
    expect(phaseAfterVerify("failed")).toBe("verify_failed");
    expect(phaseAfterVerify("non_zero_exit")).toBe("verify_failed");
  });

  it("applying 停隔离区·driver 单步不自动落地（b2b 关自动落地·apply 留给用户点改动条触发）", async () => {
    const inv = vi.fn(async (cmd: string) => {
      throw new Error(cmd);
    }) as any;
    const next = await advanceCodingLoop(
      b2aBase({
        phase: "applying",
        artifactId: "art-1",
        verifyCmd: "npm test",
      }),
      inv,
    );
    // 不自动调 apply_run_to_current_branch（否则上面 inv 会 throw）·phase 停在 applying·无 landedHead。
    expect(inv).not.toHaveBeenCalled();
    expect(next.phase).toBe("applying");
    expect(next.landedHead).toBeUndefined();
  });

  it("selectCodingVerifier 优先当前 member.task_id 的 task verifier，再退当前 run verifier", () => {
    const rows: any[] = [
      {
        run_id: "run-1",
        task_id: "other-task",
        scope: "task",
        verifier: "npm run other",
      },
      {
        run_id: "run-1",
        task_id: "run-1-task",
        scope: "run",
        verifier: "npm run all",
      },
      {
        run_id: "run-1",
        task_id: "task-1",
        scope: "task",
        verifier: "npm test",
      },
    ];
    expect(selectCodingVerifier(rows, "task-1", "run-1")).toBe("npm test");
    expect(selectCodingVerifier(rows, "missing", "run-1")).toBe("npm run all");
  });

  it("selectCodingVerifier 不会随便拿其他 task verifier 兜底", () => {
    const rows: any[] = [
      {
        run_id: "run-1",
        task_id: "other-task",
        scope: "task",
        verifier: "npm run other",
      },
    ];
    expect(selectCodingVerifier(rows, "task-1", "run-1")).toBe("");
  });
});

import type { TeamRun, CodingPhase, AcceptanceCriterion } from "../types/agent";
import { parseBackendError } from "./backendMsg";

export type CodingState = {
  runId: string;
  sessionId: string;
  assignmentId: string;
  taskId?: string | null;
  baseSha: string;
  phase: CodingPhase;
  artifactId: string | null;
  verifyCmd: string;
  landedHead?: string | null;
  lastVerdict?: string | null; // P3 折入：verify 后驱动器写入·phaseAfterVerify 据此分叉
  // 后端权威事实：会话是否绑定真实项目并就地运行。
  // in-place finalize 后改动已在用户项目中，绝不再进 merge/apply 旧链。
  isInPlace: boolean;
};

export type CodingAction =
  | {
      kind: "finalize";
      runId: string;
      sessionId: string;
      assignmentId: string;
      baseSha: string;
    }
  | { kind: "verify"; artifactId: string; cmd: string }
  | { kind: "merge"; artifactId: string }
  | { kind: "apply"; runId: string; sessionId: string }
  | { kind: "wait" } // 阶段在等用户 askQ·无自动动作
  | { kind: "done" }; // 收尾（applied/shelved/error）

/** 单 worker + done + 真改了文件(含 path) + 有 base_sha → 走 coding 闭环；否则维持现有 summary。
 *  P2 折入（两路·收紧触发·防研究类 worker 写临时文件误触发）：要求 status==="done" + anchor.base_sha
 *  存在 + 每个 changed 有 path。仍是启发式·权威 task kind 留后续；本刀 fail-safe = 不满足就走原 summary。 */
export function shouldEnterCodingLoop(run: TeamRun): boolean {
  if (run.members.length !== 1) return false;
  const m = run.members[0];
  if (m.failed || m.status !== "done") return false;
  const r: any = m.result;
  if (!r?.anchor?.base_sha) return false;
  const changed = r.changed_files;
  return (
    Array.isArray(changed) &&
    changed.length > 0 &&
    changed.every((f: any) => typeof f?.path === "string")
  );
}

/** verdict 分叉（P3 折入·进纯函数可单测）。run_verifier 的 verdict 五态·只有 passed 进落地。 */
export function phaseAfterVerify(verdict: string): CodingPhase {
  return verdict === "passed" ? "merging" : "verify_failed";
}

export function selectCodingVerifier(
  criteria: AcceptanceCriterion[],
  taskId: string | null | undefined,
  runId: string,
): string {
  const clean = (v: string | null | undefined) => (v ?? "").trim();
  const taskKey = clean(taskId);
  if (taskKey) {
    const task = criteria.find(
      (c) =>
        c.run_id === runId &&
        c.scope === "task" &&
        c.task_id === taskKey &&
        clean(c.verifier),
    );
    if (task) return clean(task.verifier);
  }
  const run = criteria.find(
    (c) => c.run_id === runId && c.scope === "run" && clean(c.verifier),
  );
  return run ? clean(run.verifier) : "";
}

/** Finalize routing reflects where the landing has already been completed:
 *  - In-place finalize writes land directly; the backend has marked merged and recorded LandingCommit,
 *    so proceed to applied without verify/merge/apply.
 *  - App-managed workspaces trust the landing and proceed directly to merging, skipping verifying.
 *  verifyCmd only remains for call-signature compatibility and display; it does not select the landing branch.
 *  Real errors (protected paths / fast-forward conflicts; see isLandingBlockedError) still trigger landing_blocked in the driver's catch block. */
export function phaseAfterFinalize(
  verifyCmd: string,
  isInPlace: boolean,
): CodingPhase {
  void verifyCmd;
  if (isInPlace) return "applied";
  return "merging"; // 仅 app 域受管工作区保留旧落地链
}

export function isLandingBlockedError(e: unknown): boolean {
  const code = parseBackendError(e)?.code;
  if (
    code === "landing.protectedPath" ||
    code === "landing.noEvidence" ||
    code === "landing.scopeExceeded" ||
    code === "landing.l1NotGreen"
  ) {
    return true;
  }

  const msg = String(e);
  return (
    msg.includes("落地前检查未通过") ||
    msg.includes("L1 未绿") ||
    msg.includes("受保护路径") ||
    msg.includes("改动超出 worker 声明") ||
    msg.includes("fast-forward")
  );
}

/** 给定状态算下一步动作（确定性·App.tsx 驱动器据此 invoke）。 */
export function nextCodingAction(s: CodingState): CodingAction {
  switch (s.phase) {
    case "finalizing":
      return {
        kind: "finalize",
        runId: s.runId,
        sessionId: s.sessionId,
        assignmentId: s.assignmentId,
        baseSha: s.baseSha,
      };
    case "verifying":
      return { kind: "verify", artifactId: s.artifactId!, cmd: s.verifyCmd };
    case "merging":
      return { kind: "merge", artifactId: s.artifactId! };
    case "applying":
      return { kind: "wait" }; // b2b：关自动落地·merge 进 staging 后停等用户点改动条
    case "ask_apply":
    case "verify_failed":
    case "ask_verify":
      return { kind: "wait" }; // 旧持久态 / 失败重试态停等用户显式继续
    case "applied":
    case "shelved":
    case "landing_blocked":
    case "error":
      return { kind: "done" };
    default:
      return { kind: "wait" };
  }
}

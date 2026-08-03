import type { CodingState } from "./codingLoop";
import {
  nextCodingAction,
  phaseAfterFinalize,
  phaseAfterVerify,
} from "./codingLoop";

export type Invoker = <T>(
  cmd: string,
  args: Record<string, unknown>,
) => Promise<T>;

/** 驱动器单步推进：执行当前 phase 对应后端动作·返回推进后的新 state（ask_* 阶段返回 wait·不前进·等用户）。
 *  纯逻辑·invoke 注入（测试 mock·生产传 tauri invoke）。terminal/wait 由调用方据 phase 决定停。 */
export async function advanceCodingLoop(
  s: CodingState,
  invoke: Invoker,
): Promise<CodingState> {
  const act = nextCodingAction(s);
  switch (act.kind) {
    case "finalize": {
      const finalized = await invoke<string>("finalize_member_artifact", {
        runId: act.runId,
        sessionId: act.sessionId,
        memberAssignmentId: act.assignmentId,
        baseSha: act.baseSha,
      });
      const phase = phaseAfterFinalize(s.verifyCmd, s.isInPlace);
      // in-place finalize 即落地·phase 直达 applied·apply 段被跳过。
      // T7：finalize 返回的是 artifact_id（run-…）·不是 git sha；landedHead 须取后端 run_landing_info
      // 的真 landed_head。app 域受管 workspace 走 merging→applying，landedHead 由 apply 段填。
      if (s.isInPlace && phase === "applied") {
        // 落地已物理完成（in-place）。run_landing_info 仅解析真 git sha（cosmetic）：
        // 任何失败（reject 或 null）退回 artifact_id 作 landedHead·仍返回 applied——
        // 绝不能因 sha 解析失败把已落地的 commit 降成 error/landing_blocked（无 undo affordance）。
        let landedHead = finalized;
        try {
          const info = await invoke<{ landedHead?: string } | null>(
            "run_landing_info",
            { sessionId: act.sessionId, runId: act.runId },
          );
          if (info?.landedHead) landedHead = info.landedHead;
        } catch {
          /* 落地已成·保留 artifact_id 兜底·不降级 */
        }
        return { ...s, artifactId: finalized, landedHead, phase };
      }
      return { ...s, artifactId: finalized, phase };
    }
    case "verify": {
      await invoke<string>("run_verifier_artifact", {
        artifactId: act.artifactId,
        cmd: act.cmd,
      });
      const v = await invoke<{ verdict: string } | null>(
        "latest_verification_for_artifact_cmd",
        { artifactId: act.artifactId },
      );
      const verdict = v?.verdict ?? "non_zero_exit";
      return { ...s, lastVerdict: verdict, phase: phaseAfterVerify(verdict) };
    }
    case "merge": {
      await invoke<string>("merge_artifact_to_staging", {
        artifactId: act.artifactId,
      });
      return { ...s, phase: "applying" };
    }
    case "apply": {
      const landedHead = await invoke<string>("apply_run_to_current_branch", {
        sessionId: act.sessionId,
        runId: act.runId,
      });
      return { ...s, landedHead, phase: "applied" };
    }
    case "wait":
    case "done":
      return s; // ask_* 停等用户 / 终态
  }
}

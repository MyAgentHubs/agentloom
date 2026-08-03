import { describe, expect, it, vi } from "vitest";
import { taskRowView, codingPhaseView } from "./taskStatus";
import type { MemberUnit } from "../types/agent";
import * as memberFailure from "./memberFailure";

const mk = (o: Partial<MemberUnit>): MemberUnit => ({
  participant_id: "p",
  assignment_id: "a",
  task_id: "t",
  name: "Codex",
  status: "running",
  sub: "改组件",
  steps_total: 5,
  steps_done: 2,
  cost_usd: null,
  input_tokens: 0,
  output_tokens: 0,
  failed: false,
  blocks: [],
  ...o,
});

describe("taskRowView", () => {
  it("5 态映射·needs_input→等你确认", () => {
    expect(taskRowView(mk({ status: "running" })).label).toBe(
      "memberDrillIn.status.running",
    );
    expect(taskRowView(mk({ status: "needs_input" })).label).toBe(
      "memberDrillIn.status.needsInput",
    );
    expect(taskRowView(mk({ status: "done" })).label).toBe(
      "memberDrillIn.status.done",
    );
    expect(taskRowView(mk({ status: "failed" })).label).toBe(
      "memberDrillIn.status.failed",
    );
    expect(taskRowView(mk({ status: "stopped" })).label).toBe(
      "memberDrillIn.status.stopped",
    );
  });
  it("执行中：chip 只步进度·无 diff/验证", () => {
    const v = taskRowView(
      mk({ status: "running", steps_done: 2, steps_total: 5 }),
    );
    expect(v.chips).toContainEqual({
      key: "taskStatus.chip.steps",
      values: { done: 2, total: 5 },
    });
    expect(v.chips).toHaveLength(1);
  });
  it("终态 done/failed/stopped 有 result 时出 N files / 验证条数", () => {
    const result = {
      changed_files: [
        { path: "a.ts", insertions: 1, deletions: 0 },
        { path: "b.ts", insertions: 2, deletions: 1 },
      ],
      command_evidence: [
        {
          cmd: "npm test",
          exit_code: 0,
          status: "passed",
          source_provider: "x",
        },
      ],
      risks: [],
    } as any;
    for (const st of ["done", "failed", "stopped"] as const) {
      const v = taskRowView(mk({ status: st, result }));
      expect(v.chips).toContainEqual({
        key: "taskStatus.chip.files",
        values: { n: 2 },
      });
      expect(v.chips).toContainEqual({
        key: "taskStatus.chip.verify",
        values: { n: 1 },
      });
    }
  });
  it("sub 为空时以 key 返回准备中文案", () => {
    expect(taskRowView(mk({ sub: "  " })).name).toEqual({
      key: "liveStreamCard.preparing",
    });
  });
  it("进展取 m.blocks 最新 tool summary 原文", () => {
    const v = taskRowView(
      mk({
        status: "running",
        blocks: [
          {
            type: "tool",
            tool: "bash",
            summary: "Running tsc",
            card: "cmd",
            status: "done",
          } as any,
        ],
      }),
    );
    expect(v.progress).toBeNull();
    expect(v.rawProgress).toBe("Running tsc");
  });
  it("failed 无错误详情时进展显示 worker 未返回结果，不复述长子任务", () => {
    const v = taskRowView(
      mk({
        status: "failed",
        failed: true,
        sub: "用 GLM 模型动手补全本仓库的 Clash Verge 配置（B 完整方向）",
      }),
    );
    expect(v.progress).toBe("memberFailure.reason.noFinalText");
    expect(v.rawProgress).toBeNull();
  });
  it("spawn 无 detail 时进展回退到通用失败状态，不显示调用失败空话", () => {
    const reasonSpy = vi
      .spyOn(memberFailure, "memberFailureReason")
      .mockReturnValueOnce({ code: "spawn" });
    const v = taskRowView(mk({ status: "failed", failed: true }));
    expect(v.progress).toBe("memberDrillIn.status.failed");
    expect(v.progress).not.toBe("memberFailure.reason.spawn");
    reasonSpy.mockRestore();
  });
  it("failed 有 API limit 证据时进展显示可识别失败原因", () => {
    const v = taskRowView(
      mk({
        status: "failed",
        failed: true,
        blocks: [
          {
            type: "text",
            text: "错误：429 Too Many Requests: rate limit exceeded",
          },
        ],
      }),
    );
    expect(v.progress).toBe("memberFailure.reason.quota");
    expect(v.rawProgress).toBeNull();
  });
  it("codingPhaseView：ask_apply=旧持久态提示", () => {
    const v = codingPhaseView("ask_apply", "DeepSeekFlash");
    expect(v.label).toBe("memberDrillIn.status.needsInput");
    expect(v.dotClass).toBe("wait");
    expect(v.progress).toBe("taskStatus.phase.askApplyProgress");
    expect(v.rawProgress).toBeNull();
    expect(v.name).toBe("DeepSeekFlash");
  });
  it("codingPhaseView：applied=已完成/done·shelved=已搁置/cancel·error=失败/fail·verifying=进行中/run", () => {
    expect(codingPhaseView("applied", "X").label).toBe(
      "memberDrillIn.status.done",
    );
    expect(codingPhaseView("applied", "X").dotClass).toBe("done");
    expect(codingPhaseView("shelved", "X").dotClass).toBe("cancel");
    expect(codingPhaseView("error", "X").dotClass).toBe("fail");
    expect(codingPhaseView("verifying", "X").label).toBe(
      "memberDrillIn.status.running",
    );
  });
  it("codingPhaseView：landing_blocked=已阻止/fail", () => {
    const v = codingPhaseView("landing_blocked", "worker");
    expect(v.dotClass).toBe("fail");
    expect(v.label).toBe("codingTask.phase.landingBlocked");
    expect(v.progress).toBe("taskStatus.phase.landingBlockedProgress");
  });
});

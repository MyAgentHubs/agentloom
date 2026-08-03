import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { I18nProvider } from "../i18n";
import { DispatchCard } from "./DispatchCard";
import type { MemberUnit } from "../types/agent";
import * as memberFailure from "../lib/memberFailure";

const mk = (o: Partial<MemberUnit>): MemberUnit => ({
  participant_id: "p1",
  assignment_id: "a1",
  task_id: "t1",
  name: "DeepSeekFlash",
  status: "running",
  sub: "改 README",
  steps_total: 3,
  steps_done: 1,
  cost_usd: null,
  input_tokens: 0,
  output_tokens: 0,
  failed: false,
  blocks: [],
  ...o,
});

describe("DispatchCard", () => {
  it("渲 .workerrow：包含头像 + worker 名 + 进展 + toolcard__badge + 查看", () => {
    const { container } = render(
      <I18nProvider>
        <DispatchCard member={mk({})} />
      </I18nProvider>,
    );

    expect(container.querySelector(".workerrow")).not.toBeNull();
    const hasAvatar =
      container.querySelector(".agent-avatar") !== null ||
      container.querySelector(".av") !== null;
    expect(hasAvatar).toBe(true);
    expect(screen.getByText("DeepSeekFlash")).toBeInTheDocument();
    expect(container.querySelector(".wr-sub")).not.toBeNull();
    expect(container.querySelector(".toolcard__badge")).not.toBeNull();
    expect(screen.getByText("查看")).toBeInTheDocument();
  });

  // 布局泄漏防回归：.task-row 是历史遗留类，其 align-items:center 会泄漏进
  // flex-column 的 .workerrow 导致子行收缩居中、任务条视觉重叠（jsdom 验不了
  // 计算样式，这里只锁 className 不再含 task-row）。
  it("workerrow 根元素 className 不含 task-row", () => {
    const { container } = render(
      <I18nProvider>
        <DispatchCard member={mk({})} />
      </I18nProvider>,
    );
    const row = container.querySelector(".workerrow") as HTMLElement;
    expect(row).not.toBeNull();
    expect(row.className.split(/\s+/)).not.toContain("task-row");
  });

  it("点查看 → onOpenInspector(member.assignment_id) 被调", () => {
    const onOpenInspector = vi.fn();
    render(
      <I18nProvider>
        <DispatchCard member={mk({})} onOpenInspector={onOpenInspector} />
      </I18nProvider>,
    );

    fireEvent.click(screen.getByText("查看"));

    expect(onOpenInspector).toHaveBeenCalledWith("a1");
  });

  it("点整条 → onOpenInspector(member.assignment_id) 被调", () => {
    const onOpenInspector = vi.fn();
    const { container } = render(
      <I18nProvider>
        <DispatchCard member={mk({})} onOpenInspector={onOpenInspector} />
      </I18nProvider>,
    );
    const row = container.querySelector(".workerrow") as HTMLElement;
    fireEvent.click(row);
    expect(onOpenInspector).toHaveBeenCalledWith("a1");
  });

  it("running 状态 → .toolcard__badge--run 存在", () => {
    const { container } = render(
      <I18nProvider>
        <DispatchCard member={mk({ status: "running" })} />
      </I18nProvider>,
    );
    expect(container.querySelector(".toolcard__badge--run")).not.toBeNull();
  });

  it("done 状态 → .toolcard__badge--done 存在", () => {
    const { container } = render(
      <I18nProvider>
        <DispatchCard member={mk({ status: "done" })} />
      </I18nProvider>,
    );
    expect(container.querySelector(".toolcard__badge--done")).not.toBeNull();
  });

  it("failed 状态 → .toolcard__badge--fail 存在", () => {
    const { container } = render(
      <I18nProvider>
        <DispatchCard member={mk({ status: "failed", failed: true })} />
      </I18nProvider>,
    );
    expect(container.querySelector(".toolcard__badge--fail")).not.toBeNull();
  });

  it("stopped 状态 → 中性徽标（.toolcard__badge--intr）、非红 FAILED、文案「已中断」", () => {
    const { container } = render(
      <I18nProvider>
        <DispatchCard member={mk({ status: "stopped" })} />
      </I18nProvider>,
    );
    // 中性徽标，绝不再渲红色 FAILED（--fail）。
    expect(container.querySelector(".toolcard__badge--intr")).not.toBeNull();
    expect(container.querySelector(".toolcard__badge--fail")).toBeNull();
    expect(screen.getByText("已中断")).toBeInTheDocument();
    expect(screen.queryByText("FAILED")).toBeNull();
  });

  it("needs_input 状态 → .toolcard__badge--intr 存在（QUEUED 中性灰）", () => {
    const { container } = render(
      <I18nProvider>
        <DispatchCard member={mk({ status: "needs_input" })} />
      </I18nProvider>,
    );
    expect(container.querySelector(".toolcard__badge--intr")).not.toBeNull();
  });

  it("spawn 有 detail → 与摘要同格式并 humanize detail", () => {
    const { container } = render(
      <I18nProvider>
        <DispatchCard
          member={mk({
            status: "failed",
            failed: true,
            blocks: [
              {
                type: "tool",
                id: "tool-1",
                tool: "agent",
                summary: "",
                card: "command",
                status: "failed",
                exit_code: 1,
                output: "context_budget_exhausted: 拆小任务",
              },
            ],
          })}
        />
      </I18nProvider>,
    );
    const reasonEl = container.querySelector(".wr-fail-reason");
    expect(reasonEl).not.toBeNull();
    expect(reasonEl).toHaveTextContent(
      "worker 调用失败 — 上下文用满，已收工——发一条消息可继续: 拆小任务",
    );
  });

  it("spawn 无 detail → 不渲 .wr-fail-reason", () => {
    const reasonSpy = vi
      .spyOn(memberFailure, "memberFailureReason")
      .mockReturnValueOnce({ code: "spawn" });
    const { container } = render(
      <I18nProvider>
        <DispatchCard member={mk({ status: "failed", failed: true })} />
      </I18nProvider>,
    );
    expect(container.querySelector(".wr-fail-reason")).toBeNull();
    reasonSpy.mockRestore();
  });

  it("quota 等非 spawn code → 保持渲失败原因行", () => {
    const { container } = render(
      <I18nProvider>
        <DispatchCard
          member={mk({
            status: "failed",
            failed: true,
            blocks: [{ type: "text", text: "HTTP 429: quota exhausted" }],
          })}
        />
      </I18nProvider>,
    );
    const reasonEl = container.querySelector(".wr-fail-reason");
    expect(reasonEl).not.toBeNull();
    expect(reasonEl?.textContent).toContain("额度");
  });

  it("running 状态 → 不渲 .wr-fail-reason（只在 failed/stopped 时出现）", () => {
    const { container } = render(
      <I18nProvider>
        <DispatchCard member={mk({ status: "running" })} />
      </I18nProvider>,
    );
    expect(container.querySelector(".wr-fail-reason")).toBeNull();
  });

  // P1-2（opus 对抗审）：en locale 之前零覆盖——failure_kind="stalled" 在英文界面下也要
  // 渲出英文的诚实标签，不能只在中文 fixture 下测过一次就算数。
  it("en locale + failure_kind=stalled → 渲英文的 stalled 标签", () => {
    const { container } = render(
      <I18nProvider initialLocale="en">
        <DispatchCard
          member={mk({
            status: "failed",
            failed: true,
            result: {
              schema_version: 1,
              assignment_id: "a1",
              participant_id: "p1",
              status: "failed",
              failure_reason:
                "Worker stalled: it has a question pending or execution got blocked (exit status: 3). This is not an environment failure — see its last output.",
              failure_kind: "stalled",
              changed_files: [],
              anchor: { base_sha: "abc", generated_from: "test" },
              command_evidence: [],
              risk_inputs: {
                files_changed: 0,
                cmd_danger: "low",
                reversibility: "reversible",
              },
              result_source: "raw",
              exit_code: 3,
            },
          })}
        />
      </I18nProvider>,
    );
    const reasonEl = container.querySelector(".wr-fail-reason");
    expect(reasonEl).not.toBeNull();
    expect(reasonEl?.textContent).toContain("stalled");
    expect(reasonEl?.textContent).not.toContain("工人");
  });
});

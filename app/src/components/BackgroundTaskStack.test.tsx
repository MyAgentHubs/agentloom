import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { I18nProvider } from "../i18n";
import type { MemberUnit } from "../types/agent";
import { BackgroundTaskStack } from "./BackgroundTaskStack";

const member = {
  participant_id: "p1",
  assignment_id: "a1",
  task_id: "t1",
  name: "Codex",
  status: "running",
  sub: "",
  steps_total: 0,
  steps_done: 0,
  cost_usd: null,
  input_tokens: 0,
  output_tokens: 0,
  failed: false,
  blocks: [],
} as MemberUnit;

describe("BackgroundTaskStack i18n", () => {
  it("在组件端翻译状态与准备中 fallback key", () => {
    render(
      <I18nProvider initialLocale="en">
        <BackgroundTaskStack runId="r1" members={[member]} />
      </I18nProvider>,
    );

    expect(screen.getByText("Running")).toBeInTheDocument();
    expect(screen.getByText("Preparing…")).toBeInTheDocument();
  });

  it("完成且有成员时显示整轮撤销入口，并传回 team run_id", () => {
    const onUndoRun = vi.fn();

    render(
      <I18nProvider initialLocale="zh">
        <BackgroundTaskStack
          runId="team-run-1"
          members={[{ ...member, status: "done" }]}
          onUndoRun={onUndoRun}
        />
      </I18nProvider>,
    );

    fireEvent.click(screen.getByRole("button", { name: "撤销这一轮" }));
    expect(onUndoRun).toHaveBeenCalledTimes(1);
    expect(onUndoRun).toHaveBeenCalledWith("team-run-1");
  });

  it("run 未完成时不显示整轮撤销入口", () => {
    render(
      <I18nProvider initialLocale="zh">
        <BackgroundTaskStack
          runId="team-run-running"
          members={[member]}
          onUndoRun={vi.fn()}
        />
      </I18nProvider>,
    );

    expect(
      screen.queryByRole("button", { name: "撤销这一轮" }),
    ).not.toBeInTheDocument();
  });

  it("无成员时保持空渲染且不显示整轮撤销入口", () => {
    const { container } = render(
      <I18nProvider initialLocale="zh">
        <BackgroundTaskStack
          runId="team-run-empty"
          members={[]}
          onUndoRun={vi.fn()}
        />
      </I18nProvider>,
    );

    expect(container).toBeEmptyDOMElement();
    expect(
      screen.queryByRole("button", { name: "撤销这一轮" }),
    ).not.toBeInTheDocument();
  });
});

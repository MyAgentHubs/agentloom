import { render, screen, fireEvent } from "@testing-library/react";
import { describe, it, expect, vi, beforeEach } from "vitest";
import { I18nProvider } from "../i18n";
import { ContinuationBriefPanel } from "./ContinuationBriefPanel";

declare const process: { env: { VITEST_DEFER_INVOKE?: string } };

const { invokeMock } = vi.hoisted(() => ({ invokeMock: vi.fn() }));

// VITEST_DEFER_INVOKE=1 makes every invoke settle one macrotask later, which
// deterministically exposes assertions that read state landing from a *different*
// async source than the one they awaited. CI runners are ~12x slower than a dev
// machine and lose those races for real; this switch reproduces it on purpose.
function __deferInvoke<T>(p: T): T | Promise<Awaited<T>> {
  return process.env.VITEST_DEFER_INVOKE
    ? new Promise((r) => setTimeout(r, 0)).then(() => p as Promise<Awaited<T>>)
    : p;
}

vi.mock("@tauri-apps/api/core", () => ({
  invoke: (cmd: string, args?: unknown) => {
    if (cmd === "set_ui_locale") return __deferInvoke(Promise.resolve());
    return __deferInvoke(invokeMock(cmd, args));
  },
}));

const readyState = {
  status: "ready" as const,
  draft:
    "# Handoff Doc\n\n## Current State\n\nThe renderer shows markdown content.",
  suggestedTitle: "Continue markdown viewer",
  warnings: [] as string[],
};

function renderPanel(
  props: Partial<React.ComponentProps<typeof ContinuationBriefPanel>> = {},
) {
  return render(
    <I18nProvider initialLocale="zh">
      <ContinuationBriefPanel
        parentSessionId="parent-1"
        parentTitle="父会话"
        draftState={readyState}
        starting={false}
        onRetry={() => {}}
        onCancel={() => {}}
        onStart={() => {}}
        {...props}
      />
    </I18nProvider>,
  );
}

describe("ContinuationBriefPanel", () => {
  beforeEach(() => {
    invokeMock.mockReset();
  });

  it("is props-driven and renders a ready handoff without invoking generation", () => {
    renderPanel();

    expect(screen.getByText("Handoff Doc")).toBeInTheDocument();
    expect(
      screen.getByText("The renderer shows markdown content."),
    ).toBeInTheDocument();
    expect(invokeMock).not.toHaveBeenCalledWith(
      "generate_handoff_doc",
      expect.anything(),
    );
  });

  it("fills the suggested session name and switches to document editing", () => {
    renderPanel();

    expect(
      screen.getByDisplayValue("Continue markdown viewer"),
    ).toHaveAccessibleName(/建议会话名/);
    fireEvent.click(screen.getByRole("button", { name: "编辑" }));
    expect(screen.getByLabelText("handoff-doc-edit")).toHaveValue(
      readyState.draft,
    );
  });

  it("disables starting when the generated document is empty", () => {
    renderPanel({ draftState: { ...readyState, draft: "" } });

    expect(screen.getByRole("button", { name: "启动子会话" })).toBeDisabled();
  });

  it("starts a continuation session with document payload only", () => {
    const onStart = vi.fn();
    renderPanel({ onStart });

    fireEvent.change(screen.getByLabelText(/建议会话名/), {
      target: { value: "Child title" },
    });
    fireEvent.click(screen.getByRole("button", { name: "启动子会话" }));

    expect(onStart).toHaveBeenCalledWith({
      parentSessionId: "parent-1",
      handoffDoc: readyState.draft,
      suggestedTitle: "Child title",
    });
    expect(onStart.mock.calls[0][0]).not.toHaveProperty("leadAgentId");
    expect(onStart.mock.calls[0][0]).not.toHaveProperty("nextStep");
  });

  it("renders generation warnings", () => {
    renderPanel({
      draftState: {
        ...readyState,
        warnings: ["Missing memory projection", "Some files were skipped"],
      },
    });

    expect(screen.getByText("Missing memory projection")).toBeVisible();
    expect(screen.getByText("Some files were skipped")).toBeVisible();
  });

  it("shows categorized generation errors and delegates retry", () => {
    const onRetry = vi.fn();
    renderPanel({
      draftState: { status: "error", error: "Error: parse failed" },
      onRetry,
    });

    expect(screen.getByRole("alert")).toHaveTextContent(
      "解析类：Error: parse failed",
    );
    fireEvent.click(screen.getByRole("button", { name: "重试" }));
    expect(onRetry).toHaveBeenCalledTimes(1);
  });

  it("shows spinner and sub-hint in loading state", () => {
    renderPanel({ draftState: { status: "loading" } });

    expect(screen.getByRole("status")).toBeInTheDocument();
    expect(
      screen.getByText("读取会话历史并总结，可能需要几十秒"),
    ).toBeInTheDocument();
    expect(document.querySelector(".cc-spinner")).toBeInTheDocument();
    expect(screen.getByText("正在生成交接文档…")).toBeInTheDocument();
  });

  it("shows a ready draft immediately when reopened", () => {
    renderPanel({
      parentSessionId: "session-a",
      parentTitle: "已完成会话",
      draftState: readyState,
    });

    expect(screen.getByText("Handoff Doc")).toBeInTheDocument();
    expect(screen.getByDisplayValue("Continue markdown viewer")).toBeVisible();
  });
});

import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { I18nProvider } from "../i18n";
import type { DecisionCardBlock } from "../types/agent";
import { PendingDecisionBar } from "./PendingDecisionBar";

const block: DecisionCardBlock = {
  type: "decision_card",
  decision_id: "decision-1",
  kind: "dispatch_confirm",
  question: "确认推送 feature/pending-bar 分支到 origin?",
  options: ["确认：立即推送", "取消：暂不推送"],
  recommended: "确认：立即推送",
  rationale: null,
  payload: null,
  source_run_id: "run-1",
  status: "pending",
  chosen_option: null,
  created_at: 1,
};

function renderBar(onChoose?: (decisionId: string, option: string) => void) {
  return render(
    <I18nProvider initialLocale="zh">
      <PendingDecisionBar block={block} onChoose={onChoose} />
    </I18nProvider>,
  );
}

describe("PendingDecisionBar", () => {
  it("渲染状态提示、完整问题与全部选项 label", () => {
    renderBar(vi.fn());

    const status = screen.getByRole("status");
    expect(status).toHaveAttribute("aria-live", "polite");
    expect(status).toHaveTextContent("有一件事等你确认");
    expect(screen.getByTitle(block.question)).toHaveTextContent(block.question);
    expect(screen.getByRole("button", { name: /确认/ })).toHaveTextContent(
      "确认",
    );
    expect(screen.getByRole("button", { name: "取消" })).toBeInTheDocument();
    expect(status).not.toHaveTextContent("立即推送");
    expect(status).not.toHaveTextContent("暂不推送");
  });

  it("点击选项回传 decisionId 与原始 option，推荐项复用 rec 标记", () => {
    const onChoose = vi.fn();
    renderBar(onChoose);

    const recommended = screen.getByRole("button", { name: /确认.*推荐/ });
    expect(recommended).toHaveClass("rec");
    expect(recommended.querySelector(".rec-pill")).toHaveTextContent("推荐");

    fireEvent.click(recommended);
    expect(onChoose).toHaveBeenCalledWith("decision-1", "确认：立即推送");
  });

  it("缺少 onChoose 时禁用全部选项", () => {
    renderBar();

    for (const button of screen.getAllByRole("button")) {
      expect(button).toBeDisabled();
      expect(button).toHaveAttribute("type", "button");
    }
  });
});

import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { CodingDecisionCard } from "../CodingDecisionCard";
import type { CodingTaskBlock } from "../../types/agent";

const base: CodingTaskBlock = {
  type: "coding_task",
  run_id: "r1",
  assignment_id: "a1",
  worker_name: "DeepSeekFlash",
  phase: "ask_apply",
};

describe("CodingDecisionCard", () => {
  it("非决策 phase（finalizing）不渲任何卡", () => {
    const { container } = render(
      <CodingDecisionCard block={{ ...base, phase: "finalizing" }} />,
    );
    expect(container.querySelector(".coding-ask")).toBeNull();
  });
  it("ask_apply 是旧持久态：不再渲染落地确认卡", () => {
    const { container } = render(<CodingDecisionCard block={base} />);
    expect(container).toBeEmptyDOMElement();
  });
  it("ask_verify：开始验证 → onConfirmVerify(runId,cmd)", () => {
    const onConfirmVerify = vi.fn();
    render(
      <CodingDecisionCard
        block={{ ...base, phase: "ask_verify", verify_cmd: "npm test" }}
        onConfirmVerify={onConfirmVerify}
      />,
    );
    fireEvent.click(screen.getByRole("button", { name: "开始验证" }));
    expect(onConfirmVerify).toHaveBeenCalledWith("r1", "npm test");
  });
  it("verify_failed：改命令重验 → onRetryVerify(runId)", () => {
    const onRetryVerify = vi.fn();
    render(
      <CodingDecisionCard
        block={{ ...base, phase: "verify_failed", detail: "npm test" }}
        onRetryVerify={onRetryVerify}
      />,
    );
    fireEvent.click(screen.getByRole("button", { name: "改命令重验" }));
    expect(onRetryVerify).toHaveBeenCalledWith("r1");
  });
  it("verify_failed→ask_verify（重验·决策态直接切换）：input 取新 recommendedCmd·不复用旧卡空 state（与原三槽一致）", () => {
    const { rerender } = render(
      <CodingDecisionCard
        block={{ ...base, phase: "verify_failed", detail: "npm test" }}
      />,
    );
    rerender(
      <CodingDecisionCard
        block={{
          ...base,
          phase: "ask_verify",
          verify_cmd: "npm run test:unit",
        }}
      />,
    );
    expect((screen.getByRole("textbox") as HTMLInputElement).value).toBe(
      "npm run test:unit",
    );
  });
});

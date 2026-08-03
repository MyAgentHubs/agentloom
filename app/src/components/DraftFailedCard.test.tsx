import { describe, it, expect, vi } from "vitest";
import { render, screen, fireEvent } from "@testing-library/react";
import { DraftFailedCard } from "./DraftFailedCard";

describe("DraftFailedCard", () => {
  it("parseExhausted → 显格式相关文案 + 三选项", () => {
    const onRetry = vi.fn();
    const onManual = vi.fn();
    const onNormal = vi.fn();
    render(
      <DraftFailedCard
        failure={{ kind: "parseExhausted", attempts: 3, lastError: "bad json" }}
        onRetry={onRetry}
        onManual={onManual}
        onBackToNormal={onNormal}
      />,
    );
    expect(screen.getByText(/Lead 拟失败/)).toBeInTheDocument();
    expect(screen.getByText(/3 次/)).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: /重试拟/ }));
    fireEvent.click(screen.getByRole("button", { name: /手动填/ }));
    fireEvent.click(screen.getByRole("button", { name: /退回 Normal/ }));
    expect(onRetry).toHaveBeenCalled();
    expect(onManual).toHaveBeenCalled();
    expect(onNormal).toHaveBeenCalled();
  });

  it("invokeFailed → 显调用失败原因", () => {
    render(
      <DraftFailedCard
        failure={{ kind: "invokeFailed", reason: "spawn 失败" }}
        onRetry={vi.fn()}
        onManual={vi.fn()}
        onBackToNormal={vi.fn()}
      />,
    );
    expect(screen.getByText(/spawn 失败/)).toBeInTheDocument();
  });

  it("readonly continuation disables recovery actions", () => {
    const onRetry = vi.fn();
    const onManual = vi.fn();
    const onNormal = vi.fn();
    render(
      <DraftFailedCard
        failure={{ kind: "invokeFailed", reason: "spawn 失败" }}
        onRetry={onRetry}
        onManual={onManual}
        onBackToNormal={onNormal}
        disabled
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: /重试拟/ }));
    fireEvent.click(screen.getByRole("button", { name: /手动填/ }));
    fireEvent.click(screen.getByRole("button", { name: /退回 Normal/ }));
    expect(onRetry).not.toHaveBeenCalled();
    expect(onManual).not.toHaveBeenCalled();
    expect(onNormal).not.toHaveBeenCalled();
  });
});

import { describe, it, expect, vi } from "vitest";
import { render, screen, fireEvent } from "@testing-library/react";
import { CodingAskCard } from "../CodingAskCard";

describe("CodingAskCard ask_verify", () => {
  it("显示推荐验证命令·可编辑·确认回传命令", () => {
    const onConfirm = vi.fn();
    render(
      <CodingAskCard
        kind="verify"
        recommendedCmd="cargo test"
        onConfirmVerify={onConfirm}
        onShelve={() => {}}
      />,
    );
    const input = screen.getByDisplayValue("cargo test") as HTMLInputElement;
    fireEvent.change(input, { target: { value: "cargo test --lib" } });
    screen.getByRole("button", { name: /确认|开始验证/ }).click();
    expect(onConfirm).toHaveBeenCalledWith("cargo test --lib");
  });

  it("不再渲染 apply 自动落地勾选", () => {
    const { container } = render(
      <CodingAskCard
        kind="verify"
        recommendedCmd="npm test"
        onConfirmVerify={() => {}}
        onShelve={() => {}}
      />,
    );
    expect(container).not.toHaveTextContent("本会话之后自动落地");
    expect(container).not.toHaveTextContent("用到当前代码");
  });
});

describe("CodingAskCard verify_failed", () => {
  it("显示验证没通过标题和失败命令", () => {
    render(
      <CodingAskCard
        kind="verify_failed"
        recommendedCmd={null}
        detail="npx vitest run"
        onConfirmVerify={() => {}}
        onShelve={() => {}}
      />,
    );

    expect(screen.getByText("验证没通过")).toBeInTheDocument();
    expect(screen.getByText("npx vitest run")).toBeInTheDocument();
  });

  it("点击查看改动触发 onViewChanges", () => {
    const onViewChanges = vi.fn();
    render(
      <CodingAskCard
        kind="verify_failed"
        recommendedCmd={null}
        onConfirmVerify={() => {}}
        onShelve={() => {}}
        onViewChanges={onViewChanges}
      />,
    );

    screen.getByRole("button", { name: "查看改动" }).click();

    expect(onViewChanges).toHaveBeenCalled();
  });

  it("点击改命令重验触发 onRetryVerify", () => {
    const onRetryVerify = vi.fn();
    render(
      <CodingAskCard
        kind="verify_failed"
        recommendedCmd={null}
        onConfirmVerify={() => {}}
        onShelve={() => {}}
        onRetryVerify={onRetryVerify}
      />,
    );

    screen.getByRole("button", { name: "改命令重验" }).click();

    expect(onRetryVerify).toHaveBeenCalled();
  });
});

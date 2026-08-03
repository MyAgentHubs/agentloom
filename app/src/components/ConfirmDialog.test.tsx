import {
  render,
  screen,
  fireEvent,
  within,
  waitFor,
} from "@testing-library/react";
import { describe, it, expect, vi } from "vitest";
import { ConfirmDialog } from "./ConfirmDialog";

const defaultProps = {
  open: true,
  title: "删除会话",
  body: "此操作不可撤销。",
  confirmLabel: "删除",
  cancelLabel: "取消",
  onConfirm: () => {},
  onCancel: () => {},
};

describe("ConfirmDialog", () => {
  it("open=false 时不渲染", () => {
    const { container } = render(
      <ConfirmDialog {...defaultProps} open={false} />,
    );
    expect(container).toBeEmptyDOMElement();
  });

  it("open=true 渲染 title、body、两个按钮", () => {
    render(<ConfirmDialog {...defaultProps} />);
    const d = screen.getByRole("dialog", { name: "删除会话" });
    expect(
      within(d).getByRole("heading", { name: "删除会话" }),
    ).toBeInTheDocument();
    expect(within(d).getByText("此操作不可撤销。")).toBeInTheDocument();
    expect(within(d).getByRole("button", { name: "删除" })).toBeInTheDocument();
    expect(within(d).getByRole("button", { name: "取消" })).toBeInTheDocument();
  });

  it("点 confirm 按钮调 onConfirm", () => {
    const onConfirm = vi.fn();
    render(<ConfirmDialog {...defaultProps} onConfirm={onConfirm} />);
    const d = screen.getByRole("dialog", { name: "删除会话" });
    fireEvent.click(within(d).getByRole("button", { name: "删除" }));
    expect(onConfirm).toHaveBeenCalledTimes(1);
  });

  it("点 cancel 按钮调 onCancel", () => {
    const onCancel = vi.fn();
    render(<ConfirmDialog {...defaultProps} onCancel={onCancel} />);
    const d = screen.getByRole("dialog", { name: "删除会话" });
    fireEvent.click(within(d).getByRole("button", { name: "取消" }));
    expect(onCancel).toHaveBeenCalledTimes(1);
  });

  it("按 Esc 调 onCancel", () => {
    const onCancel = vi.fn();
    render(<ConfirmDialog {...defaultProps} onCancel={onCancel} />);
    fireEvent.keyDown(document, { key: "Escape" });
    expect(onCancel).toHaveBeenCalledTimes(1);
  });

  it("点 backdrop 调 onCancel；点 dialog 内部不调 onCancel", () => {
    const onCancel = vi.fn();
    const { container } = render(
      <ConfirmDialog {...defaultProps} onCancel={onCancel} />,
    );
    const backdrop = container.querySelector(".dialog__backdrop")!;
    const dialog = container.querySelector(".dialog")!;
    fireEvent.click(dialog);
    expect(onCancel).not.toHaveBeenCalled();
    fireEvent.click(backdrop);
    expect(onCancel).toHaveBeenCalledTimes(1);
  });

  it("默认焦点在取消按钮", async () => {
    render(<ConfirmDialog {...defaultProps} />);
    const d = screen.getByRole("dialog", { name: "删除会话" });
    const cancelBtn = within(d).getByRole("button", { name: "取消" });
    await waitFor(() => expect(cancelBtn).toHaveFocus());
  });

  it("tone=danger（默认）时 confirm 按钮带 dialog__btn--danger class", () => {
    render(<ConfirmDialog {...defaultProps} />);
    const d = screen.getByRole("dialog", { name: "删除会话" });
    const confirmBtn = within(d).getByRole("button", { name: "删除" });
    expect(confirmBtn).toHaveClass("dialog__btn--danger");
  });
});

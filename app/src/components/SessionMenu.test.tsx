import { render, fireEvent } from "@testing-library/react";
import { describe, it, expect, vi } from "vitest";
import { SessionMenu } from "./SessionMenu";

const baseProps = {
  pinned: false,
  unread: false,
  isArchived: false,
  running: false,
  onRename: vi.fn(),
  onTogglePin: vi.fn(),
  onToggleUnread: vi.fn(),
  onToggleArchive: vi.fn(),
  onDelete: vi.fn(),
  onClose: vi.fn(),
};

describe("SessionMenu", () => {
  it("渲染分组项 + 每项有 data-action", () => {
    const { container } = render(<SessionMenu {...baseProps} />);
    const menu = container.querySelector(".sess-menu")!;
    expect(menu).not.toBeNull();
    for (const action of [
      "pin",
      "unread",
      "rename",
      "handover",
      "archive",
      "delete",
    ]) {
      expect(menu.querySelector(`[data-action="${action}"]`)).not.toBeNull();
    }
    // 已移除的占位项不再渲染
    for (const action of [
      "open-external",
      "new-window",
      "copy-link",
      "export",
    ]) {
      expect(menu.querySelector(`[data-action="${action}"]`)).toBeNull();
    }
  });

  it("接续项可点击并关闭菜单", () => {
    const onHandover = vi.fn();
    const onClose = vi.fn();
    const { container } = render(
      <SessionMenu {...baseProps} onHandover={onHandover} onClose={onClose} />,
    );
    const handover = container.querySelector(
      '[data-action="handover"]',
    ) as HTMLButtonElement;
    expect(handover.disabled).toBe(false);
    fireEvent.click(handover);
    expect(onHandover).toHaveBeenCalledOnce();
    expect(onClose).toHaveBeenCalledOnce();
  });

  it("running/archived/already continued 时接续 disabled", () => {
    const { container, rerender } = render(
      <SessionMenu {...baseProps} onHandover={vi.fn()} running />,
    );
    expect(
      (container.querySelector('[data-action="handover"]') as HTMLButtonElement)
        .disabled,
    ).toBe(true);

    rerender(<SessionMenu {...baseProps} onHandover={vi.fn()} isArchived />);
    expect(
      (container.querySelector('[data-action="handover"]') as HTMLButtonElement)
        .disabled,
    ).toBe(true);

    rerender(
      <SessionMenu {...baseProps} onHandover={vi.fn()} alreadyContinued />,
    );
    expect(
      (container.querySelector('[data-action="handover"]') as HTMLButtonElement)
        .disabled,
    ).toBe(true);

    rerender(<SessionMenu {...baseProps} onHandover={vi.fn()} handoverBusy />);
    expect(
      (container.querySelector('[data-action="handover"]') as HTMLButtonElement)
        .disabled,
    ).toBe(true);
  });

  it("归档行不显「置顶」项（pin 只作用于活动列表）", () => {
    const { container } = render(<SessionMenu {...baseProps} isArchived />);
    expect(container.querySelector('[data-action="pin"]')).toBeNull();
    expect(container.querySelector('[data-action="unread"]')).not.toBeNull();
    expect(
      container.querySelector('[data-action="archive"]')!.textContent,
    ).toMatch(/恢复/);
  });

  it("toggle 文案随 pinned/unread 变（非归档）", () => {
    const { container, rerender } = render(<SessionMenu {...baseProps} />);
    expect(container.querySelector('[data-action="pin"]')!.textContent).toMatch(
      /置顶/,
    );
    expect(
      container.querySelector('[data-action="unread"]')!.textContent,
    ).toMatch(/标记未读/);
    expect(
      container.querySelector('[data-action="archive"]')!.textContent,
    ).toMatch(/归档/);
    rerender(<SessionMenu {...baseProps} pinned unread />);
    expect(container.querySelector('[data-action="pin"]')!.textContent).toMatch(
      /取消置顶/,
    );
    expect(
      container.querySelector('[data-action="unread"]')!.textContent,
    ).toMatch(/标记已读/);
  });

  it("continuation action labels say continuation session group while delete stays single-session", () => {
    const { container, rerender } = render(
      <SessionMenu {...baseProps} hasContinuationThread />,
    );
    expect(
      container.querySelector('[data-action="move-to-group"]')!.textContent,
    ).toContain("移动接续会话组");
    expect(
      container.querySelector('[data-action="archive"]')!.textContent,
    ).toContain("归档接续会话组");
    expect(
      container.querySelector('[data-action="delete"]')!.textContent,
    ).toContain("删除");
    expect(
      container.querySelector('[data-action="delete"]')!.textContent,
    ).not.toContain("接续会话组");

    rerender(<SessionMenu {...baseProps} hasContinuationThread isArchived />);
    expect(
      container.querySelector('[data-action="archive"]')!.textContent,
    ).toContain("恢复接续会话组");
  });

  it("running 时删除 disabled", () => {
    const { container } = render(<SessionMenu {...baseProps} running />);
    const del = container.querySelector(
      '[data-action="delete"]',
    ) as HTMLButtonElement;
    expect(del.disabled).toBe(true);
  });

  it("点置顶调 onTogglePin(next) + onClose", () => {
    const onTogglePin = vi.fn();
    const onClose = vi.fn();
    const { container } = render(
      <SessionMenu
        {...baseProps}
        onTogglePin={onTogglePin}
        onClose={onClose}
      />,
    );
    fireEvent.click(container.querySelector('[data-action="pin"]')!);
    expect(onTogglePin).toHaveBeenCalledWith(true);
    expect(onClose).toHaveBeenCalled();
  });

  it("点删除调 onDelete（不直接删·由父弹确认）", () => {
    const onDelete = vi.fn();
    const { container } = render(
      <SessionMenu {...baseProps} onDelete={onDelete} />,
    );
    fireEvent.click(container.querySelector('[data-action="delete"]')!);
    expect(onDelete).toHaveBeenCalled();
  });

  it("点移到分组 → 切 move 视图·列出组 + 当前组打勾", () => {
    const groups = [
      { id: "gA", repo_id: "r", name: "前端", position: 0, created_at: 0 },
    ];
    const { container } = render(
      <SessionMenu {...baseProps} groups={groups} currentGroupId="gA" />,
    );
    fireEvent.click(container.querySelector('[data-action="move-to-group"]')!);
    expect(container.textContent).toContain("前端");
    expect(
      container.querySelector('[data-group-id="gA"][data-checked="true"]'),
    ).not.toBeNull();
  });

  it("点某组调 onMoveSessionToGroup(groupId)", () => {
    const groups = [
      { id: "gA", repo_id: "r", name: "前端", position: 0, created_at: 0 },
    ];
    const onMove = vi.fn();
    const onClose = vi.fn();
    const { container } = render(
      <SessionMenu
        {...baseProps}
        groups={groups}
        currentGroupId={null}
        onMoveSessionToGroup={onMove}
        onClose={onClose}
      />,
    );
    fireEvent.click(container.querySelector('[data-action="move-to-group"]')!);
    fireEvent.click(container.querySelector('[data-group-id="gA"]')!);
    expect(onMove).toHaveBeenCalledWith("gA");
    expect(onClose).toHaveBeenCalled();
  });

  it("点未分组调 onMoveSessionToGroup(null)", () => {
    const groups = [
      { id: "gA", repo_id: "r", name: "前端", position: 0, created_at: 0 },
    ];
    const onMove = vi.fn();
    const onClose = vi.fn();
    const { container } = render(
      <SessionMenu
        {...baseProps}
        groups={groups}
        currentGroupId="gA"
        onMoveSessionToGroup={onMove}
        onClose={onClose}
      />,
    );
    fireEvent.click(container.querySelector('[data-action="move-to-group"]')!);
    fireEvent.click(container.querySelector('[data-action="move-ungrouped"]')!);
    expect(onMove).toHaveBeenCalledWith(null);
    expect(onClose).toHaveBeenCalled();
  });

  it("新建分组 → 输入 → Enter 调 onCreateGroupAndMove", () => {
    const groups: Array<{
      id: string;
      repo_id: string;
      name: string;
      position: number;
      created_at: number;
    }> = [];
    const onCreate = vi.fn();
    const { container } = render(
      <SessionMenu
        {...baseProps}
        groups={groups}
        currentGroupId={null}
        onCreateGroupAndMove={onCreate}
      />,
    );
    fireEvent.click(container.querySelector('[data-action="move-to-group"]')!);
    fireEvent.click(container.querySelector('[data-action="new-group-menu"]')!);
    const input = container.querySelector(
      'input[data-role="new-group-input"]',
    ) as HTMLInputElement;
    fireEvent.change(input, { target: { value: "调研" } });
    fireEvent.keyDown(input, { key: "Enter" });
    expect(onCreate).toHaveBeenCalledWith("调研");
  });

  it("archived 行无「移到分组」项", () => {
    const groups = [
      { id: "gA", repo_id: "r", name: "前端", position: 0, created_at: 0 },
    ];
    const { container } = render(
      <SessionMenu
        {...baseProps}
        groups={groups}
        currentGroupId={null}
        isArchived
      />,
    );
    expect(container.querySelector('[data-action="move-to-group"]')).toBeNull();
  });

  it("每次 mount 初始在 menu 视图（view 随 mount 重置）", () => {
    const groups = [
      { id: "gA", repo_id: "r", name: "前端", position: 0, created_at: 0 },
    ];
    const { container, unmount } = render(
      <SessionMenu {...baseProps} groups={groups} currentGroupId={null} />,
    );
    expect(
      container.querySelector('[data-action="move-to-group"]'),
    ).not.toBeNull();
    fireEvent.click(container.querySelector('[data-action="move-to-group"]')!);
    expect(container.querySelector('[data-action="move-to-group"]')).toBeNull();
    unmount();
    const { container: container2 } = render(
      <SessionMenu {...baseProps} groups={groups} currentGroupId={null} />,
    );
    expect(
      container2.querySelector('[data-action="move-to-group"]'),
    ).not.toBeNull();
  });

  it("接续菜单项对 Solo（Repo）会话可用——onHandover 有值、非 running/archived/continued 时不 disabled", () => {
    const onHandover = vi.fn();
    const { container } = render(
      <SessionMenu
        {...baseProps}
        onHandover={onHandover}
        running={false}
        isArchived={false}
        alreadyContinued={false}
        handoverBusy={false}
      />,
    );
    const handover = container.querySelector(
      '[data-action="handover"]',
    ) as HTMLButtonElement;
    expect(handover).not.toBeNull();
    expect(handover.disabled).toBe(false);
  });
});

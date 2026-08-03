import { render, fireEvent } from "@testing-library/react";
import { describe, it, expect, vi } from "vitest";
import { SessionRow } from "./SessionRow";
import { makeSession } from "../test/factories";

function setup(overrides = {}) {
  const onSelect = vi.fn();
  const onRename = vi.fn();
  const onRequestDelete = vi.fn();
  const onTogglePin = vi.fn();
  const onToggleUnread = vi.fn();
  const onToggleArchive = vi.fn();
  const props = {
    session: makeSession({ id: "s1", title: "修 typecheck 报错" }),
    active: false,
    running: false,
    isArchived: false,
    onSelect,
    onRename,
    onRequestDelete,
    onTogglePin,
    onToggleUnread,
    onToggleArchive,
    groups: undefined,
    onMoveSessionToGroup: undefined,
    onCreateGroup: undefined,
    ...overrides,
  };
  const utils = render(<SessionRow {...props} />);
  const row = utils.container.querySelector(
    '[data-session-id="s1"]',
  ) as HTMLElement;
  return {
    ...utils,
    row,
    onSelect,
    onRename,
    onRequestDelete,
    onToggleArchive,
  };
}

describe("SessionRow", () => {
  it("默认行无常显 × · hover 前无 ✎/⋯", () => {
    const { row } = setup();
    expect(row.querySelector(".sess__del")).toBeNull();
    expect(row.querySelector('[data-action="row-rename"]')).toBeNull();
    expect(row.querySelector('[data-action="row-more"]')).toBeNull();
  });

  it("hover 后浮出 ✎ + ⋯", () => {
    const { row } = setup();
    fireEvent.mouseEnter(row);
    expect(row.querySelector('[data-action="row-rename"]')).not.toBeNull();
    expect(row.querySelector('[data-action="row-more"]')).not.toBeNull();
  });

  it("显示 created_at 相对时间，hover 时让位给动作按钮", () => {
    const nowMs = new Date(2026, 6, 18, 12, 0, 0).getTime();
    const nowSpy = vi.spyOn(Date, "now").mockReturnValue(nowMs);
    const { row } = setup({
      session: makeSession({
        id: "s1",
        title: "同名会话",
        created_at: nowMs / 1000 - 5 * 60,
      }),
    });

    expect(row.querySelector(".sess__time")?.textContent).toBe("5 分钟");
    fireEvent.mouseEnter(row);
    expect(row.querySelector(".sess__time")).toBeNull();
    expect(row.querySelector('[data-action="row-more"]')).not.toBeNull();
    nowSpy.mockRestore();
  });

  it("点 ✎ 直接进 inline rename（不开菜单）", () => {
    const { row } = setup();
    fireEvent.mouseEnter(row);
    fireEvent.click(row.querySelector('[data-action="row-rename"]')!);
    expect(row.querySelector(".sess-menu")).toBeNull();
    expect(row.querySelector(".sess__rename input")).not.toBeNull();
  });

  it("点 ⋯ 开菜单", () => {
    const { row } = setup();
    fireEvent.mouseEnter(row);
    fireEvent.click(row.querySelector('[data-action="row-more"]')!);
    const menu = row.querySelector(".sess-menu") as HTMLElement;
    expect(menu).not.toBeNull();
    expect(menu.style.position).toBe("fixed");
  });

  it("右键开菜单", () => {
    const { row } = setup();
    fireEvent.contextMenu(row);
    expect(row.querySelector(".sess-menu")).not.toBeNull();
  });

  it("点行触发 onSelect；点行内按钮不触发 onSelect", () => {
    const { row, onSelect } = setup();
    fireEvent.click(row);
    expect(onSelect).toHaveBeenCalledWith("s1");
    onSelect.mockClear();
    fireEvent.mouseEnter(row);
    fireEvent.click(row.querySelector('[data-action="row-more"]')!);
    expect(onSelect).not.toHaveBeenCalled();
  });

  it("inline rename：Enter 调 onRename、Esc 取消", () => {
    const { row, onRename } = setup();
    fireEvent.mouseEnter(row);
    fireEvent.click(row.querySelector('[data-action="row-rename"]')!);
    const input = row.querySelector(".sess__rename input") as HTMLInputElement;
    fireEvent.change(input, { target: { value: "新标题" } });
    fireEvent.keyDown(input, { key: "Enter" });
    expect(onRename).toHaveBeenCalledWith("s1", "新标题");
    // Esc 路径
    const { row: row2, onRename: onRename2 } = setup();
    fireEvent.mouseEnter(row2);
    fireEvent.click(row2.querySelector('[data-action="row-rename"]')!);
    const input2 = row2.querySelector(
      ".sess__rename input",
    ) as HTMLInputElement;
    fireEvent.change(input2, { target: { value: "x" } });
    fireEvent.keyDown(input2, { key: "Escape" });
    expect(onRename2).not.toHaveBeenCalled();
    expect(row2.querySelector(".sess__rename input")).toBeNull();
  });

  it("进入重命名：input 聚焦 + 原名称全选", () => {
    const { row } = setup({
      session: makeSession({ id: "s1", title: "原名称" }),
    });
    fireEvent.mouseEnter(row);
    fireEvent.click(row.querySelector('[data-action="row-rename"]')!);
    const input = row.querySelector(".sess__rename input") as HTMLInputElement;
    expect(document.activeElement).toBe(input);
    expect(input.selectionStart).toBe(0);
    expect(input.selectionEnd).toBe("原名称".length);
  });

  it("IME 组词中 Enter 不提交（isComposing）", () => {
    const { row, onRename } = setup();
    fireEvent.mouseEnter(row);
    fireEvent.click(row.querySelector('[data-action="row-rename"]')!);
    const input = row.querySelector(".sess__rename input") as HTMLInputElement;
    fireEvent.change(input, { target: { value: "拼音中" } });
    // 模拟 IME 组词中的 Enter（isComposing=true）
    fireEvent.keyDown(input, { key: "Enter", isComposing: true });
    expect(onRename).not.toHaveBeenCalled();
    // 组词结束后的 Enter 才提交
    fireEvent.keyDown(input, { key: "Enter" });
    expect(onRename).toHaveBeenCalledWith("s1", "拼音中");
  });

  it("点 ⋯ 开菜单后 · 点页面别处（document mousedown）自动收起", () => {
    const { row } = setup();
    fireEvent.mouseEnter(row);
    fireEvent.click(row.querySelector('[data-action="row-more"]')!);
    expect(row.querySelector(".sess-menu")).not.toBeNull();
    // 点别处 = document 上的 mousedown（目标在 .dd 容器外）
    fireEvent.mouseDown(document.body);
    expect(row.querySelector(".sess-menu")).toBeNull();
  });

  it("菜单点删除 → 调 onRequestDelete(session)、不再渲染 .sess-pop", () => {
    const onRequestDelete = vi.fn();
    const { row } = setup({ onRequestDelete });
    fireEvent.contextMenu(row);
    fireEvent.click(row.querySelector('[data-action="delete"]')!);
    expect(onRequestDelete).toHaveBeenCalledWith(
      expect.objectContaining({ id: "s1" }),
    );
    expect(row.querySelector(".sess-pop")).toBeNull();
  });

  it("未读 → 标题加粗类 + 橙点；置顶 → pin 图标", () => {
    const { row } = setup({
      session: makeSession({
        id: "s1",
        title: "x",
        unread: true,
        pinned: true,
      }),
    });
    expect(row.querySelector(".sess__unread")).not.toBeNull();
    expect(row.querySelector(".sess__pin")).not.toBeNull();
    expect(row.querySelector(".sess__nm--unread")).not.toBeNull();
  });

  it("归档区行：菜单归档项文案为「恢复」、点调 onToggleArchive(id, false)", () => {
    const { row, onToggleArchive } = setup({
      isArchived: true,
      session: makeSession({ id: "s1", title: "x", archived: true }),
    });
    fireEvent.contextMenu(row);
    const arch = row.querySelector('[data-action="archive"]')!;
    expect(arch.textContent).toMatch(/恢复/);
    fireEvent.click(arch);
    expect(onToggleArchive).toHaveBeenCalledWith("s1", false);
  });

  it("归档区行：即便 pinned 也不显 pin 图标（pin 列保留·图标不渲染）", () => {
    const { row } = setup({
      isArchived: true,
      session: makeSession({
        id: "s1",
        title: "x",
        archived: true,
        pinned: true,
      }),
    });
    expect(row.querySelector(".sess__pin")).toBeNull();
  });

  it("左栏行状态点三态（dotStatus）：running 暖橙脉动 · attention 红 · done 绿 · 无状态回退 idle/run", () => {
    const { row: idleRow } = setup();
    const idleDot = idleRow.querySelector(".sess__dot")!;
    expect(idleDot.className).toContain("idle");
    expect(idleDot.className).not.toContain("run");
    expect(idleDot.className).not.toContain("attention");
    expect(idleDot.className).not.toContain("done");

    const { row: runningRow } = setup({ dotStatus: "running" });
    const runDot = runningRow.querySelector(".sess__dot")!;
    expect(runDot.className).toContain("run");
    expect(runDot.className).not.toContain("idle");

    const { row: attentionRow } = setup({ dotStatus: "attention" });
    expect(attentionRow.querySelector(".sess__dot")!.className).toContain(
      "attention",
    );

    const { row: doneRow } = setup({ dotStatus: "done" });
    expect(doneRow.querySelector(".sess__dot")!.className).toContain("done");

    // dotStatus 未传（undefined）时退化用既有 running 布尔二态（向后兼容·不破坏既有调用点）
    const { row: legacyRunningRow } = setup({ running: true });
    expect(legacyRunningRow.querySelector(".sess__dot")!.className).toContain(
      "run",
    );
  });

  it("渲染父子接续血缘标签", () => {
    const { row: parentRow } = setup({
      session: makeSession({
        id: "s1",
        title: "父会话",
        continued_to_session_id: "child-1",
      }),
    });
    expect(
      parentRow.querySelector('[data-testid="session-lineage-parent"]')
        ?.textContent,
    ).toContain("已交接到 →");

    const { row: childRow } = setup({
      session: makeSession({
        id: "s1",
        title: "子会话",
        parent_session_id: "parent-1",
      }),
      parentTitle: "父会话",
    });
    expect(childRow).toHaveClass("sess--child");
    expect(
      childRow.querySelector('[data-testid="session-lineage-child"]')
        ?.textContent,
    ).not.toContain("↳ 接续自 父会话");
    expect(
      childRow
        .querySelector('[data-testid="session-lineage-child"]')
        ?.getAttribute("title"),
    ).toContain("父会话");
  });
});

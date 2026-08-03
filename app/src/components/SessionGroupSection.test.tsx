import { render, fireEvent } from "@testing-library/react";
import { describe, it, expect, vi } from "vitest";
import { SessionGroupSection } from "./SessionGroupSection";
import type { GroupMeta, Session } from "../types/agent";
import { makeSession } from "../test/factories";

const group: GroupMeta = {
  id: "gA",
  repo_id: "local-default",
  name: "前端",
  position: 0,
  created_at: 0,
};

const sessions: Session[] = [
  makeSession({ id: "s1", title: "组内会话", group_id: "gA" }),
];

function renderSess(s: Session) {
  return (
    <div key={s.id} data-session-id={s.id}>
      {s.title}
    </div>
  );
}

describe("SessionGroupSection", () => {
  it("展开时渲染组内会话", () => {
    const { container } = render(
      <SessionGroupSection
        group={group}
        sessions={sessions}
        expanded={true}
        onToggle={() => {}}
        renderSess={renderSess}
      />,
    );
    expect(container.querySelector('[data-session-id="s1"]')).not.toBeNull();
  });

  it("折叠时 .sb-group.collapsed 存在", () => {
    const { container } = render(
      <SessionGroupSection
        group={group}
        sessions={sessions}
        expanded={false}
        onToggle={() => {}}
        renderSess={renderSess}
      />,
    );
    expect(container.querySelector(".sb-group.collapsed")).not.toBeNull();
  });

  it("点头部（.sb-group__name）切 onToggle", () => {
    const onToggle = vi.fn();
    const { container } = render(
      <SessionGroupSection
        group={group}
        sessions={sessions}
        expanded={true}
        onToggle={onToggle}
        renderSess={renderSess}
      />,
    );
    fireEvent.click(container.querySelector(".sb-group__name")!);
    expect(onToggle).toHaveBeenCalledTimes(1);
  });

  it("空组计数 0", () => {
    const { container } = render(
      <SessionGroupSection
        group={group}
        sessions={[]}
        expanded={true}
        onToggle={() => {}}
        renderSess={renderSess}
      />,
    );
    expect(container.querySelector(".sb-group__count")!.textContent).toBe("0");
  });

  it("hover 后出 ⋯ 按钮", () => {
    const { container } = render(
      <SessionGroupSection
        group={group}
        sessions={sessions}
        expanded={true}
        onToggle={() => {}}
        renderSess={renderSess}
      />,
    );
    expect(container.querySelector('[data-action="group-more"]')).toBeNull();
    fireEvent.mouseEnter(container.querySelector(".sb-group")!);
    expect(
      container.querySelector('[data-action="group-more"]'),
    ).not.toBeNull();
  });

  it("点⋯→ 重命名 → inline input → Enter 调 onRename", () => {
    const onRename = vi.fn();
    const { container } = render(
      <SessionGroupSection
        group={group}
        sessions={sessions}
        expanded={true}
        onToggle={() => {}}
        onRename={onRename}
        renderSess={renderSess}
      />,
    );
    fireEvent.mouseEnter(container.querySelector(".sb-group")!);
    fireEvent.click(container.querySelector('[data-action="group-more"]')!);
    fireEvent.click(container.querySelector('[data-action="group-rename"]')!);
    const input = container.querySelector(
      ".sb-group__rename-input",
    ) as HTMLInputElement;
    fireEvent.change(input, { target: { value: "新前端" } });
    fireEvent.keyDown(input, {
      key: "Enter",
      nativeEvent: { isComposing: false },
    });
    expect(onRename).toHaveBeenCalledWith("gA", "新前端");
  });

  it("点⋯→ 删除 → 调 onRequestDelete(group)", () => {
    const onRequestDelete = vi.fn();
    const { container } = render(
      <SessionGroupSection
        group={group}
        sessions={sessions}
        expanded={true}
        onToggle={() => {}}
        onRequestDelete={onRequestDelete}
        renderSess={renderSess}
      />,
    );
    fireEvent.mouseEnter(container.querySelector(".sb-group")!);
    fireEvent.click(container.querySelector('[data-action="group-more"]')!);
    fireEvent.click(container.querySelector('[data-action="group-delete"]')!);
    expect(onRequestDelete).toHaveBeenCalledWith(group);
  });
});

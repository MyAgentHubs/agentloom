import { describe, it, expect, vi } from "vitest";
import { render, screen, fireEvent } from "@testing-library/react";
import { SessionContextBar } from "./SessionContextBar";

describe("SessionContextBar · sf-ctx 会话状态条", () => {
  it("显会话标题（主）+ repo 次级 + idle（无 .st.run·无「干净」字样）", () => {
    const { container } = render(
      <SessionContextBar
        title="今天几号"
        repoLabel="ai-digest"
        status="idle"
      />,
    );
    expect(screen.getByText("今天几号")).not.toBeNull();
    expect(container.querySelector(".sf-ctx__title")).not.toBeNull();
    expect(container.querySelector(".sf-ctx__sub")).not.toBeNull();
    expect(container.querySelector(".sf-ctx .st.run")).toBeNull();
  });

  it("working 态 .st.run + workingLabel", () => {
    const { container } = render(
      <SessionContextBar
        title="t"
        status="working"
        workingLabel="working · 14s"
      />,
    );
    expect(container.querySelector(".sf-ctx .st.run")).not.toBeNull();
    expect(screen.getByText("working · 14s")).not.toBeNull();
  });

  it("点 ⋯ 触发 onMenu", () => {
    const onMenu = vi.fn();
    render(<SessionContextBar title="t" status="idle" onMenu={onMenu} />);
    fireEvent.click(screen.getByLabelText("会话菜单"));
    expect(onMenu).toHaveBeenCalledOnce();
  });
});

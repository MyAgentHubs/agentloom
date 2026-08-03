import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { AgentAvatar } from "./AgentAvatar";

describe("AgentAvatar", () => {
  it("claude → agent-avatar--claude + svg", () => {
    const { container } = render(<AgentAvatar kind="claude" />);
    const el = container.querySelector(".agent-avatar--claude");
    expect(el).toBeInTheDocument();
    expect(el?.querySelector("svg")).toBeInTheDocument();
  });

  it("user → agent-avatar--user + svg", () => {
    const { container } = render(<AgentAvatar kind="user" />);
    expect(
      container.querySelector(".agent-avatar--user svg"),
    ).toBeInTheDocument();
  });

  it("avatar_renders_new_kind_glm_kimi", () => {
    const { container } = render(
      <>
        <AgentAvatar kind="glm" />
        <AgentAvatar kind="kimi" />
        <AgentAvatar kind="borrow-glm" />
      </>,
    );

    const glm = container.querySelector(".agent-avatar--glm");
    const kimi = container.querySelector(".agent-avatar--kimi");
    expect(glm).toBeInTheDocument();
    expect(kimi).toBeInTheDocument();
    expect(container.querySelector(".agent-avatar--unknown")).toBeNull();
    expect(glm).toHaveAttribute("style", expect.stringContaining("background"));
    expect(kimi).toHaveAttribute(
      "style",
      expect.stringContaining("background"),
    );
  });

  it("zhipu / z.ai / bigmodel 都渲染成 glm 头像（非首字母）", () => {
    for (const kind of ["zhipu", "z.ai", "bigmodel"]) {
      const { container, unmount } = render(<AgentAvatar kind={kind} />);
      expect(container.querySelector(".agent-avatar--glm")).not.toBeNull();
      expect(container.querySelector(".agent-avatar--unknown")).toBeNull();
      unmount();
    }
  });

  it("unknown → 兜底首字母（无 svg、显首字母）", () => {
    const { container } = render(<AgentAvatar kind="mystery" />);
    expect(
      container.querySelector(".agent-avatar--unknown"),
    ).toBeInTheDocument();
    expect(screen.getByText("M")).toBeInTheDocument();
  });
});

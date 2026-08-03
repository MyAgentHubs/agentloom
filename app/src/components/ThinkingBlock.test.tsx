import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { ThinkingBlock } from "./ThinkingBlock";

describe("ThinkingBlock", () => {
  it("默认折叠：显 thinking 标签、不显内容", () => {
    render(<ThinkingBlock text="我的推理过程" />);
    expect(screen.getByText(/thinking/i)).toBeInTheDocument();
    expect(screen.queryByText("我的推理过程")).not.toBeInTheDocument();
  });

  it("点击展开后显内容", () => {
    render(<ThinkingBlock text="我的推理过程" />);
    fireEvent.click(screen.getByRole("button"));
    expect(screen.getByText("我的推理过程")).toBeInTheDocument();
  });
});

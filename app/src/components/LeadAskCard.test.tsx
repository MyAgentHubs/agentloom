import { render, screen, fireEvent } from "@testing-library/react";
import { describe, it, expect, vi } from "vitest";
import { LeadAskCard } from "./LeadAskCard";

describe("LeadAskCard", () => {
  it("渲问题 + 选项·点推荐项调 onChoose", () => {
    const onChoose = vi.fn();
    render(
      <LeadAskCard
        view={{
          kind: "ask",
          question: "派 worker 改 README，可以吗？",
          options: ["可以", "先放着"],
          recommended: "可以",
          rationale: "要改代码",
        }}
        onChoose={onChoose}
      />,
    );
    expect(
      screen.getByText("派 worker 改 README，可以吗？"),
    ).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "可以" }));
    expect(onChoose).toHaveBeenCalledWith("可以");
  });

  it("readonly continuation disables choices", () => {
    const onChoose = vi.fn();
    render(
      <LeadAskCard
        view={{
          kind: "ask",
          question: "继续派 worker？",
          options: ["可以", "先放着"],
          recommended: "可以",
          rationale: "",
        }}
        onChoose={onChoose}
        disabled
      />,
    );
    const choice = screen.getByRole("button", { name: "可以" });
    expect(choice).toBeDisabled();
    fireEvent.click(choice);
    expect(onChoose).not.toHaveBeenCalled();
  });
});

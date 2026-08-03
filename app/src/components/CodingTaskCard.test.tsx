import { describe, it, expect } from "vitest";
import { render, screen } from "@testing-library/react";
import { CodingTaskCard } from "./CodingTaskCard";

describe("CodingTaskCard", () => {
  it("基础渲染不崩", () => {
    render(
      <CodingTaskCard
        block={
          {
            type: "coding_task",
            run_id: "r1",
            assignment_id: "a1",
            worker_name: "w",
            phase: "finalizing",
          } as any
        }
        onOpenDetail={() => {}}
      />,
    );
    expect(screen.getByText("固化改动")).toBeInTheDocument();
  });

  it("带 lead_rationale → 渲折叠「为什么」小行", () => {
    render(
      <CodingTaskCard
        block={
          {
            type: "coding_task",
            run_id: "r1",
            assignment_id: "a1",
            worker_name: "w",
            phase: "finalizing",
            lead_rationale: "因为要改 README",
          } as any
        }
        onOpenDetail={() => {}}
      />,
    );
    expect(screen.getByText(/因为要改 README/)).toBeInTheDocument();
  });
});

import { describe, it, expect, vi } from "vitest";
import { render, screen } from "@testing-library/react";
import { CodingTaskCard } from "../CodingTaskCard";
import type { CodingTaskBlock } from "../../types/agent";

const blk: CodingTaskBlock = {
  type: "coding_task",
  run_id: "r1",
  assignment_id: "m1",
  worker_name: "Codex",
  phase: "verifying",
  step_done: 2,
  step_total: 3,
  artifact_id: "art-1",
  verify_cmd: "cargo test",
  detail: null,
};

describe("CodingTaskCard", () => {
  it("显示 worker 名 + 阶段人读文案 + 进度", () => {
    render(<CodingTaskCard block={blk} onOpenDetail={() => {}} />);
    expect(screen.getByText(/Codex/)).toBeInTheDocument();
    expect(screen.getByText(/验证中|跑验证|verifying/i)).toBeInTheDocument();
    expect(screen.getByText(/2\s*\/\s*3/)).toBeInTheDocument();
  });
  it("点详情触发 onOpenDetail(run_id, assignment_id)", () => {
    const fn = vi.fn();
    render(<CodingTaskCard block={blk} onOpenDetail={fn} />);
    screen.getByRole("button", { name: /详情/ }).click();
    expect(fn).toHaveBeenCalledWith("r1", "m1");
  });
});

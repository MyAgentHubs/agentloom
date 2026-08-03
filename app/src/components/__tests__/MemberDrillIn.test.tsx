import { describe, it, expect } from "vitest";
import { render, screen } from "@testing-library/react";
import { MemberDrillIn } from "../MemberDrillIn";

const member = {
  participant_id: "p1",
  assignment_id: "m1",
  task_id: "t1",
  name: "Codex",
  status: "done",
  sub: "改 foo",
  steps_total: 3,
  steps_done: 3,
  cost_usd: 0,
  input_tokens: 0,
  output_tokens: 0,
  failed: false,
  blocks: [],
  result: {
    status: "done",
    anchor: { base_sha: "b", generated_from: "worktree" },
    // P1 折入：真实形状 ChangedFile[] / CommandEvidence[]（不是 string[]·别假绿）
    changed_files: [
      { path: "src/foo.rs", insertions: 3, deletions: 1 },
      { path: "src/bar.rs", insertions: 2, deletions: 0 },
    ],
    command_evidence: [
      {
        cmd: "cargo test",
        exit_code: 0,
        status: "passed",
        source_provider: "codex",
      },
    ],
  } as any,
} as any;

describe("MemberDrillIn coding 详情", () => {
  it("有改动时展示改了哪几个文件 + 命令证据", () => {
    render(
      <MemberDrillIn
        members={[member]}
        selectedId="m1"
        onSelect={() => {}}
        onBack={() => {}}
        onStop={() => {}}
      />,
    );
    expect(screen.getByText(/src\/foo\.rs/)).toBeInTheDocument();
    expect(screen.getByText(/cargo test/)).toBeInTheDocument();
    expect(screen.getByText(/退出码|exit|0/)).toBeInTheDocument();
  });
});

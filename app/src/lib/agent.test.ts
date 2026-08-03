import { test, expect } from "vitest";
import type { MemberUnit, MemberResult } from "../types/agent";

test("MemberUnit 可带 result·老快照无 result 不破", () => {
  const result: MemberResult = {
    schema_version: 1,
    assignment_id: "a1",
    participant_id: "p1",
    status: "done",
    changed_files: [{ path: "src/a.rs", insertions: 3, deletions: 1 }],
    anchor: { base_sha: "abc", generated_from: "worktree_diff" },
    command_evidence: [
      { cmd: "cargo test", status: "ok", source_provider: "codex" },
    ],
    risk_inputs: { files_changed: 1, cmd_danger: "low", reversibility: "high" },
    result_source: "worker_tail",
  };
  const withResult = { result } as Partial<MemberUnit>;
  expect(withResult.result?.schema_version).toBe(1);
  expect(withResult.result?.changed_files[0].path).toBe("src/a.rs");

  const old = {} as Partial<MemberUnit>;
  expect(old.result).toBeUndefined();
});

test("软字段全 optional·最小 MemberResult 合法", () => {
  const minimal: MemberResult = {
    schema_version: 1,
    assignment_id: "a",
    participant_id: "p",
    status: "done",
    changed_files: [],
    anchor: { base_sha: "x", generated_from: "worktree_diff" },
    command_evidence: [],
    risk_inputs: { files_changed: 0, cmd_danger: "low", reversibility: "high" },
    result_source: "lead_extract",
  };
  expect(minimal.decisions).toBeUndefined();
  expect(minimal.risks).toBeUndefined();
});

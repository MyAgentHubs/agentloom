import { describe, it, expect } from "vitest";
import { parseAssignments, assignmentsToCriteria } from "./gate";

const SAMPLE = JSON.stringify([
  {
    subtask_id: "t1",
    subtask: "实现 mood-record 命令",
    assignee: { agent_id: "a-codex", provider: "openai", model: "gpt-5" },
    scope_files: ["src/commands/mood-record.ts"],
    acceptance: [
      { claim: "命令实现并导出 askMood", verifier: "npm test" },
      { claim: "单测覆盖核心分支", verifier: null },
    ],
  },
  {
    subtask_id: "t2",
    subtask: "写 fixture 文案",
    assignee: null,
    scope_files: ["fixtures/mood/*.json"],
    acceptance: [{ claim: "12 种语气齐全", verifier: null }],
  },
]);

describe("parseAssignments", () => {
  it("解析 snake_case JSON 字符串成结构化数组", () => {
    const a = parseAssignments(SAMPLE);
    expect(a).toHaveLength(2);
    expect(a[0].subtaskId).toBe("t1");
    expect(a[0].subtask).toBe("实现 mood-record 命令");
    expect(a[0].assignee?.provider).toBe("openai");
    expect(a[0].scopeFiles).toEqual(["src/commands/mood-record.ts"]);
    expect(a[0].acceptance[0].claim).toBe("命令实现并导出 askMood");
    expect(a[1].assignee).toBeNull(); // 未派到 agent
  });

  it("坏 JSON → 空数组（不抛·让 UI 走拟失败兜底外的防御）", () => {
    expect(parseAssignments("not json")).toEqual([]);
    expect(parseAssignments("")).toEqual([]);
  });
});

describe("assignmentsToCriteria", () => {
  it("拍平所有 subtask 的 acceptance 成带 id 的 criteria 列表", () => {
    const cs = assignmentsToCriteria(parseAssignments(SAMPLE));
    expect(cs).toHaveLength(3); // 2 + 1
    expect(cs[0].id).toBe("t1#0");
    expect(cs[0].claim).toBe("命令实现并导出 askMood");
    expect(cs[0].verifier).toBe("npm test");
    expect(cs[0].scope).toBe("task");
    expect(cs[0].taskId).toBe("t1");
    expect(cs[2].id).toBe("t2#0");
    expect(cs[1].verifier).toBeNull();
  });
});

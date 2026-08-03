import { describe, it, expect, vi } from "vitest";
import { render, screen, fireEvent } from "@testing-library/react";
import { AssignmentEditor } from "./AssignmentEditor";
import type { ParsedAssignment } from "../types/gate";
import type { AgentProfile } from "../types/agent";

const ASSIGNMENTS: ParsedAssignment[] = [
  {
    subtaskId: "t1",
    subtask: "实现 mood-record 命令",
    assignee: { agentId: "a1", provider: "openai", model: "gpt-5" },
    scopeFiles: ["src/commands/mood-record.ts"],
    acceptance: [],
  },
  {
    subtaskId: "t2",
    subtask: "写 fixture 文案",
    assignee: null,
    scopeFiles: ["fixtures/mood/*.json"],
    acceptance: [],
  },
];

const AGENTS = [
  {
    id: "a1",
    name: "codex",
    provider: "openai",
    primary_model: "gpt-5",
    enabled: true,
  },
  {
    id: "a2",
    name: "deepseek",
    provider: "deepseek",
    primary_model: "v3",
    enabled: true,
  },
] as unknown as AgentProfile[];

function setup() {
  const onAction = vi.fn();
  render(
    <AssignmentEditor
      assignments={ASSIGNMENTS}
      autoDispatch
      enabledAgents={AGENTS}
      onAction={onAction}
    />,
  );
  return { onAction };
}

describe("AssignmentEditor", () => {
  it("逐行渲子任务描述 + scope 文件 + chip", () => {
    setup();
    expect(screen.getByText("实现 mood-record 命令")).toBeInTheDocument();
    expect(screen.getByText("src/commands/mood-record.ts")).toBeInTheDocument();
    expect(screen.getByText("写 fixture 文案")).toBeInTheDocument();
  });

  it("未派到 agent 的行显「未派」", () => {
    setup();
    expect(screen.getByText(/未派/)).toBeInTheDocument();
  });

  it("点 chip → 改派下拉只列 enabled agent + 诚实文案(无「白名单」字样)", () => {
    setup();
    fireEvent.click(screen.getAllByRole("button", { name: /改派/ })[0]);
    expect(screen.getByText("codex")).toBeInTheDocument();
    expect(screen.getByText("deepseek")).toBeInTheDocument();
    expect(
      screen.getByText(/只列已启用且当前可用的 agent/),
    ).toBeInTheDocument();
    expect(screen.queryByText(/白名单/)).not.toBeInTheDocument();
  });

  it("点「+ 加一块活儿」→ onAction(addAssignment)（真控件·非置灰）", () => {
    const { onAction } = setup();
    fireEvent.click(screen.getByText(/加一块活儿/));
    expect(onAction).toHaveBeenCalledWith({ type: "addAssignment" });
  });

  it("下拉选一个 → onAction(reassign)", () => {
    const { onAction } = setup();
    fireEvent.click(screen.getAllByRole("button", { name: /改派/ })[0]);
    fireEvent.click(screen.getByText("deepseek"));
    expect(onAction).toHaveBeenCalledWith({
      type: "reassign",
      subtaskId: "t1",
      assignee: { agentId: "a2", provider: "deepseek", model: "v3" },
    });
  });

  it("点 ✕ → onAction(removeAssignment)", () => {
    const { onAction } = setup();
    fireEvent.click(screen.getAllByLabelText("移除该成员")[0]);
    expect(onAction).toHaveBeenCalledWith({
      type: "removeAssignment",
      subtaskId: "t1",
    });
  });

  it("自动派开关 → onAction(toggleAutoDispatch)", () => {
    const { onAction } = setup();
    fireEvent.click(screen.getByRole("switch", { name: /自动派/ }));
    expect(onAction).toHaveBeenCalledWith({ type: "toggleAutoDispatch" });
  });

  it("本地校验脚注 = 队长自己做", () => {
    setup();
    expect(screen.getByText(/本地校验.*Lead/)).toBeInTheDocument();
  });
});

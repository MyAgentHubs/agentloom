import { fireEvent, render } from "@testing-library/react";
import { describe, it, expect, vi } from "vitest";
import { TaskInspector } from "./TaskInspector";
import { I18nProvider } from "../i18n";
import type { MemberUnit } from "../types/agent";

function wrapper({ children }: { children: React.ReactNode }) {
  return <I18nProvider initialLocale="zh">{children}</I18nProvider>;
}

describe("TaskInspector", () => {
  it("done member: shows conclusion, artifacts, verification, owner, and raw trace is folded", () => {
    const member: MemberUnit = {
      participant_id: "p1",
      assignment_id: "a1",
      task_id: "t1",
      name: "Codex",
      status: "done",
      sub: "修复 GoalBar 绿态",
      steps_total: 3,
      steps_done: 3,
      cost_usd: 0.01,
      input_tokens: 100,
      output_tokens: 200,
      failed: false,
      blocks: [],
      result: {
        schema_version: 1,
        assignment_id: "a1",
        participant_id: "p1",
        status: "done",
        failure_reason: null,
        changed_files: [{ path: "01.md", insertions: 12, deletions: 0 }],
        anchor: { base_sha: "abc", generated_from: "worker" },
        command_evidence: [
          {
            cmd: "cat 01.md",
            exit_code: 0,
            status: "ok",
            source_provider: "claude",
          },
        ],
        risk_inputs: {
          files_changed: 1,
          cmd_danger: "low",
          reversibility: "high",
        },
        final_text_ref: "GoalBar 绿态已修复，allPass 逻辑正确。",
        result_source: "worker",
      },
    };

    const { getByText, getAllByText, container } = render(
      <TaskInspector member={member} onClose={() => {}} />,
      { wrapper },
    );

    expect(getAllByText(/已完成/)[0]).toBeTruthy();
    expect(getByText("Codex")).toBeTruthy();
    expect(container.textContent).toContain("1 个文件");
  });

  it("failed member: shows failure_reason", () => {
    const member: MemberUnit = {
      participant_id: "p2",
      assignment_id: "a2",
      task_id: "t2",
      name: "Claude",
      status: "failed",
      sub: "运行测试",
      steps_total: 2,
      steps_done: 1,
      cost_usd: null,
      input_tokens: 50,
      output_tokens: 80,
      failed: true,
      blocks: [],
      result: {
        schema_version: 1,
        assignment_id: "a2",
        participant_id: "p2",
        status: "failed",
        failure_reason: "测试没过",
        changed_files: [],
        anchor: { base_sha: "def", generated_from: "worker" },
        command_evidence: [],
        risk_inputs: {
          files_changed: 0,
          cmd_danger: "low",
          reversibility: "high",
        },
        final_text_ref: null,
        result_source: "worker",
        exit_code: 1,
        stderr_tail: "boom: connection refused",
      },
    };

    const { getAllByText, getByText, container } = render(
      <TaskInspector member={member} onClose={() => {}} />,
      { wrapper },
    );

    expect(getAllByText(/失败/)[0]).toBeTruthy();
    // 真断言：失败原因区必须渲出 failure_reason 原文 + 退出码，不能只停在通用「失败」标签。
    expect(getByText("测试没过")).toBeTruthy();
    expect(getByText(/退出码\s*1/)).toBeTruthy();
    // stderr_tail 默认折叠：<details> 存在、<pre> 内容存在但不强制展开可见。
    const details = container.querySelector(
      ".task-inspector__card--raw details",
    );
    expect(details).toBeTruthy();
    expect(details?.textContent).toContain("boom: connection refused");
  });

  it("inspector_detail_is_slimmed", () => {
    const member: MemberUnit = {
      participant_id: "p3",
      assignment_id: "a3",
      task_id: "t3",
      name: "Codex",
      status: "done",
      sub: "修复GoalBar绿态",
      steps_total: 3,
      steps_done: 3,
      cost_usd: 0.01,
      input_tokens: 100,
      output_tokens: 200,
      failed: false,
      blocks: [
        {
          type: "tool",
          id: "tool-1",
          tool: "bash",
          summary: "run tests",
          card: "command",
          status: "ok",
          exit_code: 0,
          output: "ok",
        },
      ],
      result: {
        schema_version: 1,
        assignment_id: "a3",
        participant_id: "p3",
        status: "done",
        failure_reason: null,
        changed_files: [{ path: "GoalBar.tsx", insertions: 18, deletions: 4 }],
        anchor: { base_sha: "abc", generated_from: "worker" },
        command_evidence: [
          {
            cmd: "npx vitest run",
            exit_code: 0,
            status: "ok",
            source_provider: "claude",
          },
        ],
        risk_inputs: {
          files_changed: 1,
          cmd_danger: "low",
          reversibility: "high",
        },
        final_text_ref: "GoalBar绿态已修复allPass逻辑正确",
        result_source: "worker",
      },
    };

    const { getByText, queryByText, container } = render(
      <TaskInspector member={member} onClose={() => {}} />,
      { wrapper },
    );

    expect(getByText("修复GoalBar绿态")).toBeTruthy();
    expect(getByText("执行者")).toBeTruthy();
    expect(queryByText("GoalBar绿态已修复allPass逻辑正确")).toBeNull();
    expect(queryByText("GoalBar.tsx")).toBeNull();
    expect(queryByText("npx vitest run")).toBeNull();
    const toolFold = container.querySelector("details.toolfold");
    expect(toolFold).toBeTruthy();
    expect(toolFold?.querySelector(".toolfold__label")?.textContent).toBe(
      "执行了 1 步",
    );
    expect(toolFold?.textContent).not.toMatch(
      /GoalBar绿态已修复allPass逻辑正确|GoalBar\.tsx|npx vitest run/,
    );
    expect(container.textContent).toContain("1 个文件");
    // changed_files 有 1 文件 path=GoalBar.tsx insertions=18 deletions=4
    expect(container.textContent).toMatch(/\+18/);
  });

  it("inspector_title_not_bold", () => {
    const member: MemberUnit = {
      participant_id: "p4",
      assignment_id: "a4",
      task_id: "t4",
      name: "Codex",
      status: "done",
      sub: "Test Task",
      steps_total: 1,
      steps_done: 1,
      cost_usd: null,
      input_tokens: 0,
      output_tokens: 0,
      failed: false,
      blocks: [],
      result: undefined,
    };

    const { getByText } = render(
      <TaskInspector member={member} onClose={() => {}} />,
      { wrapper },
    );

    const title = getByText("Test Task");
    expect(title).toBeTruthy();
    expect(title.tagName).not.toBe("H3");
  });

  it("member.sub 含 ** 加粗标记 → 走 markdown 渲染出 <strong>，裸 ** 不出现在文本中", () => {
    const member: MemberUnit = {
      participant_id: "p6",
      assignment_id: "a6",
      task_id: "t6",
      name: "Codex",
      status: "done",
      sub: "**加粗** 的任务简述",
      steps_total: 1,
      steps_done: 1,
      cost_usd: null,
      input_tokens: 0,
      output_tokens: 0,
      failed: false,
      blocks: [],
      result: undefined,
    };

    const { container } = render(
      <TaskInspector member={member} onClose={() => {}} />,
      { wrapper },
    );

    const title = container.querySelector(".task-inspector__title");
    expect(title).toBeTruthy();
    expect(title?.querySelector("strong")).not.toBeNull();
    expect(title?.textContent).not.toContain("**");
    expect(title?.textContent).toContain("加粗");
  });

  it("member.sub 为空字符串 → task-inspector__title 整块不渲染", () => {
    const member: MemberUnit = {
      participant_id: "p7",
      assignment_id: "a7",
      task_id: "t7",
      name: "Codex",
      status: "done",
      sub: "",
      steps_total: 1,
      steps_done: 1,
      cost_usd: null,
      input_tokens: 0,
      output_tokens: 0,
      failed: false,
      blocks: [],
      result: undefined,
    };

    const { container } = render(
      <TaskInspector member={member} onClose={() => {}} />,
      { wrapper },
    );

    expect(container.querySelector(".task-inspector__title")).toBeNull();
  });

  it("running_member_has_no_stop_button", () => {
    const member: MemberUnit = {
      participant_id: "p5",
      assignment_id: "a5",
      task_id: "t5",
      name: "Codex",
      status: "running",
      sub: "Running Task",
      steps_total: 1,
      steps_done: 0,
      cost_usd: null,
      input_tokens: 0,
      output_tokens: 0,
      failed: false,
      blocks: [],
      result: undefined,
    };

    const { container } = render(
      <TaskInspector member={member} onClose={() => {}} />,
      { wrapper },
    );

    expect(container.querySelector(".tc-stop")).toBeNull();
  });

  it("提供 onBackToList 时渲返回任务列表按钮并可点", () => {
    const onBack = vi.fn();
    const member: MemberUnit = {
      participant_id: "pb1",
      assignment_id: "ab1",
      task_id: "tb1",
      name: "Codex",
      status: "done",
      sub: "测试任务",
      steps_total: 1,
      steps_done: 1,
      cost_usd: null,
      input_tokens: 0,
      output_tokens: 0,
      failed: false,
      blocks: [],
      result: undefined,
    };

    const { getByText } = render(
      <TaskInspector
        member={member}
        onClose={() => {}}
        onBackToList={onBack}
      />,
      { wrapper },
    );

    fireEvent.click(getByText("返回任务列表"));

    expect(onBack).toHaveBeenCalled();
  });

  it("不传 onBackToList 时不渲返回任务列表按钮", () => {
    const member: MemberUnit = {
      participant_id: "pb2",
      assignment_id: "ab2",
      task_id: "tb2",
      name: "Codex",
      status: "done",
      sub: "测试任务",
      steps_total: 1,
      steps_done: 1,
      cost_usd: null,
      input_tokens: 0,
      output_tokens: 0,
      failed: false,
      blocks: [],
      result: undefined,
    };

    const { queryByText } = render(
      <TaskInspector member={member} onClose={() => {}} />,
      { wrapper },
    );

    expect(queryByText("返回任务列表")).toBeNull();
  });
});

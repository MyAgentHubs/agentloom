import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
// @ts-expect-error - Vitest runs in Node, but this frontend tsconfig has no Node type declarations.
import { readFileSync } from "fs";
import { BackgroundTaskStack } from "../BackgroundTaskStack";
import type { MemberUnit } from "../../types/agent";

const mk = (o: Partial<MemberUnit>): MemberUnit => ({
  participant_id: "p1",
  assignment_id: "a1",
  task_id: "t1",
  name: "DeepSeekFlash",
  status: "running",
  sub: "改 README",
  steps_total: 3,
  steps_done: 1,
  cost_usd: null,
  input_tokens: 0,
  output_tokens: 0,
  failed: false,
  blocks: [],
  ...o,
});

describe("BackgroundTaskStack", () => {
  it("渲后台任务条·KISS 一行（worker 名 + 子任务 + 状态徽标）·不渲 feed/lead 壳", () => {
    const { container } = render(
      <BackgroundTaskStack runId="r1" lead="Claude" members={[mk({})]} />,
    );
    expect(container.querySelector(".taskstack")).not.toBeNull();
    expect(screen.getByText("DeepSeekFlash")).toBeInTheDocument(); // worker 名（粗）
    expect(screen.getByText("改 README")).toBeInTheDocument(); // 子任务（灰·进展位）
    expect(container.querySelector(".task-badge.st-run")).not.toBeNull(); // 末尾 scoped 状态徽标
    expect(screen.queryByText("查看")).toBeNull();
    expect(container.querySelector(".livestream")).toBeNull();
    expect(container.querySelector(".say")).toBeNull();
    expect(container.querySelector(".team-run__lead")).toBeNull();
  });
  it("点任务行 → onOpenMember(runId, assignmentId)·行内无嵌套 <button>", () => {
    const onOpenMember = vi.fn();
    const { container } = render(
      <BackgroundTaskStack
        runId="r1"
        members={[mk({})]}
        onOpenMember={onOpenMember}
      />,
    );
    fireEvent.click(screen.getByText("改 README"));
    expect(onOpenMember).toHaveBeenCalledWith("r1", "a1");
    // task-row 自身是 role=button（div）·这里断言「行内无真 <button> 元素」（不放查看详情按钮）
    expect(container.querySelector("button")).toBeNull();
  });
  it("任务行用 scoped 状态徽标·不再渲独立查看文字", () => {
    const { container } = render(
      <BackgroundTaskStack runId="r1" members={[mk({ status: "running" })]} />,
    );
    expect(container.querySelector(".task-badge")).not.toBeNull();
    expect(screen.queryByText("查看")).toBeNull();
  });
  it("failed 任务条展示失败原因进展，并给整行失败态样式", () => {
    const { container } = render(
      <BackgroundTaskStack
        runId="r1"
        members={[mk({ status: "failed", failed: true })]}
      />,
    );

    expect(screen.getByText("worker 未返回结果")).toBeInTheDocument();
    expect(container.querySelector(".task-row.st-fail")).not.toBeNull();
  });
  it("failed task row CSS 有整行失败态，不只靠右侧 badge", () => {
    const css = readFileSync("src/styles/global.css", "utf-8");
    expect(css).toMatch(/\.task-row\.st-fail\s*\{[^}]*border-color:/);
    expect(css).toMatch(/\.task-row\.st-fail\s+\.tprog\s*\{[^}]*color:/);
  });
  it("空 members 返回 null", () => {
    const { container } = render(
      <BackgroundTaskStack runId="r1" members={[]} />,
    );
    expect(container.firstChild).toBeNull();
  });
});

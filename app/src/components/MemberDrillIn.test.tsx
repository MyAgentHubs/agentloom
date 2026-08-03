import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, test, vi } from "vitest";
import { MemberDrillIn } from "./MemberDrillIn";
import type { MemberUnit } from "../types/agent";

const mk = (o: Partial<MemberUnit>): MemberUnit => ({
  participant_id: "w",
  assignment_id: "a",
  task_id: "t",
  name: "worker-1",
  status: "running",
  sub: "实现 X",
  steps_total: 0,
  steps_done: 0,
  cost_usd: null,
  input_tokens: 0,
  output_tokens: 0,
  failed: false,
  blocks: [],
  ...o,
});

describe("MemberDrillIn", () => {
  test("带 taskPack 的队员渲染默认折叠的查看派单", () => {
    const member = mk({
      assignment_id: "a1",
      taskPack: "## 总目标\n看下 X\n## 你的子任务\n看下 X\n",
    });
    const onSelect = vi.fn();

    const { container } = render(
      <MemberDrillIn
        members={[member]}
        selectedId="a1"
        onSelect={onSelect}
        onBack={() => {}}
      />,
    );

    expect(screen.getByText("查看派单")).toBeInTheDocument();
    const brief = container.querySelector(".drillin__brief");
    expect(brief).not.toHaveAttribute("open");
    expect(brief).toHaveClass("drillin__fold");

    fireEvent.click(screen.getByText("查看派单"));
    expect(screen.getByText("总目标")).toBeInTheDocument();
  });

  test("顶部只显示轻量状态，不重复 worker 名和长派单 brief；Summary 是 brief 摘要", () => {
    const longBrief =
      "只读检查仓库 demo-configs 的 readme.md 内容并完整汇报其内容与结构；不要修改任何文件。读取路径 /Users/dev/Code/github.com/octocat/demo-configs/readme.md";
    const member = mk({
      assignment_id: "a1",
      name: "GLM 4.7",
      status: "failed",
      sub: longBrief,
      taskPack: `## 总目标\n${longBrief}\n\n## 你的子任务\n${longBrief}\n`,
    });
    const { container } = render(
      <MemberDrillIn
        members={[member]}
        selectedId="a1"
        onSelect={() => {}}
        onBack={() => {}}
      />,
    );

    const head = container.querySelector(".drillin__head");
    expect(head?.textContent).toContain("失败");
    expect(head?.textContent).not.toContain("GLM 4.7");
    expect(head?.textContent).not.toContain("只读检查仓库");
    expect(screen.getByText("概览")).toBeInTheDocument();
    const summary = container.querySelector(".drillin__summary");
    expect(summary?.textContent).toContain("只读检查仓库");
    const codes = Array.from(
      container.querySelectorAll(".drillin__summary code"),
    ).map((node) => node.textContent);
    expect(codes.some((code) => code?.startsWith("/Users/dev/Code"))).toBe(
      true,
    );
  });

  test("无 taskPack 的队员不渲染查看派单 brief", () => {
    render(
      <MemberDrillIn
        members={[mk({ assignment_id: "a1" })]}
        selectedId="a1"
        onSelect={() => {}}
        onBack={() => {}}
      />,
    );

    expect(screen.queryByText("查看派单")).toBeNull();
  });

  test("队员明细不渲 inline diff（diff 归右侧 Review）", () => {
    const member = mk({
      assignment_id: "a1",
      status: "done",
      result: {
        changed_files: [{ path: "src/x.ts", insertions: 3, deletions: 1 }],
        command_evidence: [],
      } as any,
    });
    const { container } = render(
      <MemberDrillIn
        members={[member]}
        selectedId="a1"
        onSelect={() => {}}
        onBack={() => {}}
      />,
    );
    expect(screen.queryByText("改动内容")).toBeNull();
    expect(container.querySelector(".review__diff")).toBeNull();
    expect(container.querySelector(".drillin__diff-wrap")).toBeNull();
  });

  test("有改动文件时提示终端直写可能未入账", () => {
    const member = mk({
      assignment_id: "a1",
      status: "done",
      result: {
        changed_files: [{ path: "src/x.ts", insertions: 3, deletions: 1 }],
        command_evidence: [],
      } as any,
    });

    render(
      <MemberDrillIn
        members={[member]}
        selectedId="a1"
        onSelect={() => {}}
        onBack={() => {}}
      />,
    );

    expect(screen.getByText(/终端直写.*可能未入账/)).toBeInTheDocument();
  });

  test("worker 原始过程收进默认折叠的 Raw Trace + 显 Summary 占位", () => {
    const member = mk({
      assignment_id: "a1",
      status: "done",
      blocks: [{ type: "text", text: "worker 的原始平铺输出" }],
      result: {
        changed_files: [{ path: "src/x.ts", insertions: 3, deletions: 1 }],
        command_evidence: [{ cmd: "npm test", exit_code: 0 }],
      } as any,
    });
    const { container } = render(
      <MemberDrillIn
        members={[member]}
        selectedId="a1"
        onSelect={() => {}}
        onBack={() => {}}
      />,
    );
    expect(screen.getByText("概览")).toBeInTheDocument();
    expect(screen.getByText("改动文件")).toBeInTheDocument();
    expect(screen.getByText("验证")).toBeInTheDocument();
    const raw = container.querySelector(".drillin__raw");
    expect(raw).not.toHaveAttribute("open");
    expect(raw).toHaveClass("drillin__fold");
    expect(screen.getByText("原始过程")).toBeInTheDocument();
    expect(container.querySelectorAll(".drillin__fold-sum")).toHaveLength(1);
    const rawBody = container.querySelector(".drillin__raw-body");
    expect(rawBody?.textContent).toContain("worker 的原始平铺输出");
  });

  // P1（member 失败原因透出）：失败态在头部状态行下补一行 failure_reason 原文。
  test("失败态渲一行 failure_reason（不影响 hasCodingDetails 判断）", () => {
    const member = mk({
      assignment_id: "a1",
      status: "failed",
      result: {
        failure_reason:
          "工人停摆：有问题在等回答，或执行被阻塞（exit status: 3）。这不是环境故障——看它最后的输出。",
        changed_files: [],
        command_evidence: [],
      } as any,
    });
    const { container } = render(
      <MemberDrillIn
        members={[member]}
        selectedId="a1"
        onSelect={() => {}}
        onBack={() => {}}
      />,
    );
    const failureRow = container.querySelector(".drillin__failure");
    expect(failureRow).not.toBeNull();
    expect(failureRow?.textContent).toContain("不是环境故障");
    expect(screen.getByText("失败原因")).toBeInTheDocument();
    // hasCodingDetails 语义不受影响：changed_files 空 → 不渲改动文件详情段。
    expect(screen.queryByText("改动文件")).toBeNull();
  });

  // 人话映射接线（MemberDrillIn 裸显点）：failure_reason 混合文本（诚实正文 + 尾部裸码）
  // 只 humanize 尾部裸码段，诚实正文原样保留。
  test("failure_reason 混合文本：尾部裸码变人话，诚实正文原样保留", () => {
    const member = mk({
      assignment_id: "a1",
      status: "failed",
      result: {
        failure_reason:
          "工人的上下文窗口装不下了（单轮 token 预算耗尽）；不是卡住，也没有问题在等回答。\ncontext_budget_exhausted: 拆小任务 / 换更大上下文的模型",
        changed_files: [],
        command_evidence: [],
      } as any,
    });
    const { container } = render(
      <MemberDrillIn
        members={[member]}
        selectedId="a1"
        onSelect={() => {}}
        onBack={() => {}}
      />,
    );
    const failureText = container.querySelector(".drillin__failure-text");
    expect(failureText?.textContent).toContain(
      "工人的上下文窗口装不下了（单轮 token 预算耗尽）",
    );
    expect(failureText?.textContent).toContain("上下文用满，已收工");
    expect(failureText?.textContent).not.toContain("context_budget_exhausted:");
    expect(failureText?.textContent).toContain("拆小任务 / 换更大上下文的模型");
  });

  test("failure_reason 整串以可识别裸码开头 → 转人话", () => {
    const member = mk({
      assignment_id: "a1",
      status: "failed",
      result: {
        failure_reason: "budget_exhausted_still_progressing: 发一条消息可继续",
        changed_files: [],
        command_evidence: [],
      } as any,
    });
    const { container } = render(
      <MemberDrillIn
        members={[member]}
        selectedId="a1"
        onSelect={() => {}}
        onBack={() => {}}
      />,
    );
    const failureText = container.querySelector(".drillin__failure-text");
    expect(failureText?.textContent).toContain(
      "本轮回合预算用完（任务还在推进）",
    );
    expect(failureText?.textContent).not.toContain(
      "budget_exhausted_still_progressing:",
    );
  });

  test("failure_reason 含未知裸码 → 原样透传可见（前向兼容·不静默吞）", () => {
    const member = mk({
      assignment_id: "a1",
      status: "failed",
      result: {
        failure_reason: "诚实正文在这里。\nsome_future_code: xxx",
        changed_files: [],
        command_evidence: [],
      } as any,
    });
    const { container } = render(
      <MemberDrillIn
        members={[member]}
        selectedId="a1"
        onSelect={() => {}}
        onBack={() => {}}
      />,
    );
    const failureText = container.querySelector(".drillin__failure-text");
    expect(failureText?.textContent).toBe(
      "诚实正文在这里。\nsome_future_code: xxx",
    );
  });

  test("非失败态不渲 .drillin__failure（即便 result 里带 failure_reason 残留）", () => {
    const member = mk({
      assignment_id: "a1",
      status: "done",
      result: {
        failure_reason: "上一轮的残留文本",
        changed_files: [],
        command_evidence: [],
      } as any,
    });
    const { container } = render(
      <MemberDrillIn
        members={[member]}
        selectedId="a1"
        onSelect={() => {}}
        onBack={() => {}}
      />,
    );
    expect(container.querySelector(".drillin__failure")).toBeNull();
  });

  test("brief 与 Raw Trace 同时存在时使用同一折叠样式", () => {
    const member = mk({
      assignment_id: "a1",
      taskPack: "## 总目标\n看下 X\n## 你的子任务\n看下 X\n",
      blocks: [{ type: "text", text: "worker trace" }],
    });
    const { container } = render(
      <MemberDrillIn
        members={[member]}
        selectedId="a1"
        onSelect={() => {}}
        onBack={() => {}}
      />,
    );

    expect(container.querySelector(".drillin__brief")).toHaveClass(
      "drillin__fold",
    );
    expect(container.querySelector(".drillin__raw")).toHaveClass(
      "drillin__fold",
    );
    expect(container.querySelectorAll(".drillin__fold-sum")).toHaveLength(2);
  });
});

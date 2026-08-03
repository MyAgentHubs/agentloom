import { render, screen, fireEvent } from "@testing-library/react";
import { describe, it, expect, vi } from "vitest";
import { RightPanel } from "./RightPanel";
import type { MemberUnit, ReviewResult } from "../types/agent";
import { I18nProvider } from "../i18n";

const withChanges: ReviewResult = {
  has_changes: true,
  stat: " a.txt | 1 +",
  patch: "diff --git a/a.txt b/a.txt\n@@ -0,0 +1 @@\n+hello\n",
  files_changed: 1,
  files: [{ path: "a.txt", undoable: false }],
  diff_available: true,
};

const base = {
  open: true,
  tab: null,
  review: null,
  onTab: () => {},
};

const member = (o: Partial<MemberUnit>): MemberUnit => ({
  participant_id: "w",
  assignment_id: "a",
  task_id: "t",
  name: "worker-1",
  status: "running",
  sub: "实现 X",
  steps_total: 8,
  steps_done: 3,
  cost_usd: null,
  input_tokens: 24000,
  output_tokens: 0,
  failed: false,
  blocks: [{ type: "text", text: "我在实现 mood-record" }],
  ...o,
});

describe("RightPanel v3", () => {
  it("收起时不渲染任何内容", () => {
    const { container } = render(<RightPanel {...base} open={false} />);
    expect(container).toBeEmptyDOMElement();
  });

  it("body-only：不再渲染 tab 条（tab 在顶栏右段）", () => {
    render(<RightPanel {...base} tab="review" />);
    expect(screen.queryByRole("tab")).not.toBeInTheDocument();
  });

  it("body-only：不再渲染面板内收起按钮", () => {
    render(<RightPanel {...base} tab="review" />);
    expect(screen.queryByLabelText("收起右面板")).not.toBeInTheDocument();
  });

  it("picker 默认态（tab=null）渲染 5 张卡（Files / Review / Side chat / Terminal / Browser）", () => {
    render(<RightPanel {...base} tab={null} />);
    expect(screen.getByLabelText("打开 Files")).toBeInTheDocument();
    expect(screen.getByLabelText("打开 Review")).toBeInTheDocument();
    expect(screen.getByLabelText("Side chat 即将支持")).toBeDisabled();
    expect(screen.getByLabelText("Terminal 即将支持")).toBeDisabled();
    expect(screen.getByLabelText("Browser 即将支持")).toBeDisabled();
  });

  it("previewPath 非空时 picker 显示预览项和 basename；为空时不显示", () => {
    const { rerender } = render(
      <RightPanel {...base} tab={null} previewPath="docs/guide/T4.md" />,
    );

    expect(screen.getByLabelText("打开 预览")).toHaveTextContent("预览 T4.md");

    rerender(<RightPanel {...base} tab={null} previewPath={null} />);
    expect(screen.queryByLabelText("打开 预览")).not.toBeInTheDocument();
  });

  it("点击 picker 的预览项切回 preview tab", () => {
    const onTab = vi.fn();
    render(
      <RightPanel
        {...base}
        tab={null}
        previewPath="docs/guide/T4.md"
        onTab={onTab}
      />,
    );

    fireEvent.click(screen.getByLabelText("打开 预览"));

    expect(onTab).toHaveBeenCalledWith("preview");
  });

  it("预览项的关闭按钮只上抛关闭动作", () => {
    const onTab = vi.fn();
    const onClosePreview = vi.fn();
    render(
      <RightPanel
        {...base}
        tab={null}
        previewPath="docs/guide/T4.md"
        onTab={onTab}
        onClosePreview={onClosePreview}
      />,
    );

    fireEvent.click(screen.getByLabelText("关闭预览"));

    expect(onClosePreview).toHaveBeenCalledOnce();
    expect(onTab).not.toHaveBeenCalled();
  });

  it("picker 不渲染 Agent 卡（v3 去 Agent）", () => {
    render(<RightPanel {...base} tab={null} />);
    expect(screen.queryByLabelText(/打开 Agent/i)).not.toBeInTheDocument();
  });

  it("点 picker 卡触发 onTab(卡 id)", () => {
    const onTab = vi.fn();
    render(<RightPanel {...base} tab={null} onTab={onTab} />);
    fireEvent.click(screen.getByLabelText("打开 Files"));
    expect(onTab).toHaveBeenCalledWith("files");
  });

  it("未支持的 picker 卡置灰且不会触发 onTab", () => {
    const onTab = vi.fn();
    render(<RightPanel {...base} tab={null} onTab={onTab} />);

    fireEvent.click(screen.getByLabelText("Terminal 即将支持"));

    expect(onTab).not.toHaveBeenCalled();
    expect(screen.getAllByText("即将")).toHaveLength(3);
  });

  it("picker 中英文文案之间保留空格", () => {
    render(<RightPanel {...base} tab={null} />);
    expect(screen.getByLabelText("打开 Files")).toHaveTextContent(
      "Files 浏览整个项目文件",
    );
  });

  it("Review tab 无改动时显示空态「尚无改动」（行为保全）", () => {
    render(<RightPanel {...base} tab="review" review={null} />);
    expect(screen.getByText("尚无改动")).toBeInTheDocument();
  });

  it("Files tab 接入项目文件面板，非会话视图显示空态", () => {
    render(<RightPanel {...base} tab="files" sessionId={null} />);
    expect(screen.getByText("进入会话后浏览项目文件")).toBeInTheDocument();
  });

  it("Review 空态：非 session 显「进入会话后审查」", () => {
    const { container } = render(
      <RightPanel
        open={true}
        tab="review"
        review={null}
        reviewContext="none"
        onTab={() => {}}
      />,
    );
    expect(container.textContent).toContain("进入会话后审查");
    expect(container.textContent).not.toContain("这个会话还没有");
  });

  it("Review tab 有改动时只提供 diff 查看，不提供留存/丢弃动作", () => {
    render(<RightPanel {...base} tab="review" review={withChanges} />);
    expect(
      screen.queryByRole("button", { name: "留存" }),
    ).not.toBeInTheDocument();
    expect(
      screen.queryByRole("button", { name: "全部丢弃" }),
    ).not.toBeInTheDocument();
    expect(screen.getByText("+hello")).toBeInTheDocument();
  });

  it("Review 有改动时显示未纳入本次 Review 的其余变更数", () => {
    render(
      <RightPanel
        {...base}
        tab="review"
        review={{ ...withChanges, other_dirty_count: 133 }}
      />,
    );

    expect(
      screen.getByText("工作目录另有 133 个未纳入本次 Review 的变更"),
    ).toHaveClass("rp-otherdirty");
  });

  it("其余变更提示的视觉样式只由 class 提供，不带 inline style", () => {
    render(
      <RightPanel
        {...base}
        tab="review"
        review={{ ...withChanges, other_dirty_count: 133 }}
      />,
    );

    const hint = screen.getByText(
      "工作目录另有 133 个未纳入本次 Review 的变更",
    );
    expect(hint).toHaveClass("rp-otherdirty");
    expect(hint).not.toHaveAttribute("style");
  });

  it("Review 空态同样显示未纳入本次 Review 的其余变更数", () => {
    render(
      <RightPanel
        {...base}
        tab="review"
        review={{
          has_changes: false,
          stat: "",
          patch: "",
          files_changed: 0,
          files: [],
          diff_available: true,
          other_dirty_count: 133,
        }}
      />,
    );

    expect(screen.getByText("尚无改动")).toBeInTheDocument();
    expect(
      screen.getByText("工作目录另有 133 个未纳入本次 Review 的变更"),
    ).toHaveClass("rp-otherdirty");
  });

  it("其余变更数为 0 或字段缺失时不显示提示", () => {
    const { rerender } = render(
      <RightPanel
        {...base}
        tab="review"
        review={{ ...withChanges, other_dirty_count: 0 }}
      />,
    );
    expect(
      screen.queryByText(/未纳入本次 Review 的变更/),
    ).not.toBeInTheDocument();

    rerender(<RightPanel {...base} tab="review" review={withChanges} />);
    expect(
      screen.queryByText(/未纳入本次 Review 的变更/),
    ).not.toBeInTheDocument();
  });

  it("没有选中会话时不显示其余变更提示", () => {
    render(
      <RightPanel
        {...base}
        tab="review"
        review={{ ...withChanges, other_dirty_count: 133 }}
        reviewContext="none"
      />,
    );

    expect(
      screen.queryByText(/未纳入本次 Review 的变更/),
    ).not.toBeInTheDocument();
  });

  it("review tab ✕ 关闭 → onTab(null)", () => {
    const onTab = vi.fn();
    render(
      <RightPanel
        {...base}
        open={true}
        tab="review"
        review={{
          has_changes: true,
          stat: "1 file",
          patch:
            "diff --git a/x.ts b/x.ts\n--- a/x.ts\n+++ b/x.ts\n@@ -1 +1 @@\n-a\n+b",
          files_changed: 1,
          files: [{ path: "x.ts", undoable: false }],
          diff_available: true,
        }}
        onTab={onTab}
      />,
    );
    fireEvent.click(screen.getByLabelText("关闭"));
    expect(onTab).toHaveBeenCalledWith(null);
  });

  it("Side chat / Terminal / Browser tab 显「即将」占位", () => {
    const { rerender } = render(<RightPanel {...base} tab="side" />);
    expect(screen.getByText(/即将/)).toBeInTheDocument();
    rerender(<RightPanel {...base} tab="terminal" />);
    expect(screen.getByText(/即将/)).toBeInTheDocument();
    rerender(<RightPanel {...base} tab="browser" />);
    expect(screen.getByText(/即将/)).toBeInTheDocument();
  });

  it("review 存在但 has_changes=false 时仍显空态（不渲染 ReviewPanel）", () => {
    render(
      <RightPanel
        {...base}
        tab="review"
        review={{
          has_changes: false,
          stat: "",
          patch: "",
          files_changed: 0,
          files: [],
          diff_available: true,
        }}
      />,
    );
    expect(screen.getByText("尚无改动")).toBeInTheDocument();
    expect(
      screen.queryByRole("button", { name: "留存" }),
    ).not.toBeInTheDocument();
  });

  it("非 git 项目显示可解释降级态", () => {
    render(
      <RightPanel
        {...base}
        tab="review"
        review={{
          has_changes: false,
          stat: "",
          patch: "",
          files_changed: 0,
          files: [],
          diff_available: false,
        }}
      />,
    );
    expect(screen.getByText("无法生成改动对比")).toBeInTheDocument();
    expect(
      screen.getByText("这个项目不是带 HEAD 的 Git 工作树"),
    ).toBeInTheDocument();
  });
});

describe("RightPanel drill-in", () => {
  const members = [
    member({ assignment_id: "a1", name: "worker-1" }),
    member({
      assignment_id: "a2",
      name: "worker-2",
      blocks: [{ type: "text", text: "我在写测试" }],
    }),
  ];

  it("drill 模式优先渲染选中队员细节、面包屑、切换器、token 行", () => {
    render(
      <RightPanel
        {...base}
        drill={{
          members,
          selectedId: "a1",
          onSelect: () => {},
          onBack: () => {},
        }}
      />,
    );
    expect(screen.queryByText("选一个工具开成 tab")).not.toBeInTheDocument();
    expect(screen.getByLabelText("回 Lead")).toBeInTheDocument();
    expect(screen.getByText("worker-1")).toBeInTheDocument();
    expect(screen.getByText("worker-2")).toBeInTheDocument();
    expect(screen.getByText("实现 X")).toBeInTheDocument();
    expect(document.querySelector(".drillin__head")).not.toHaveTextContent(
      "worker-1",
    );
    expect(screen.getByText(/24k tok|24000/)).toBeInTheDocument();
  });

  it("切到另一队员上抛 onSelect", () => {
    const onSelect = vi.fn();
    render(
      <RightPanel
        {...base}
        drill={{ members, selectedId: "a1", onSelect, onBack: () => {} }}
      />,
    );
    fireEvent.click(screen.getByText("worker-2"));
    expect(onSelect).toHaveBeenCalledWith("a2");
  });

  it("onBack 退出（恢复原 tab 由 App 负责·此处只验上抛）", () => {
    const onBack = vi.fn();
    render(
      <RightPanel
        {...base}
        drill={{ members, selectedId: "a1", onSelect: () => {}, onBack }}
      />,
    );
    fireEvent.click(screen.getByLabelText("回 Lead"));
    expect(onBack).toHaveBeenCalledOnce();
  });
});

describe("RightPanel inspector", () => {
  it("inspectorMember 非空 + open → 渲 TaskInspector（标题+kv）", () => {
    const member = {
      participant_id: "p1",
      assignment_id: "a1",
      task_id: "t1",
      name: "Codex",
      status: "done",
      sub: "修复测试",
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
        changed_files: [],
        anchor: { base_sha: "abc", generated_from: "worker" },
        command_evidence: [],
        risk_inputs: {
          files_changed: 0,
          cmd_danger: "low",
          reversibility: "high",
        },
        final_text_ref: "已生成 10 条并写入 01.md / 02.md",
        result_source: "worker",
      },
    } as any;
    render(
      <I18nProvider>
        <RightPanel
          open={true}
          tab={null}
          review={null}
          onTab={() => {}}
          inspectorMember={member}
          onCloseInspector={() => {}}
        />
      </I18nProvider>,
    );
    expect(screen.getByText("修复测试")).toBeInTheDocument();
  });
});

describe("RightPanel tasklist", () => {
  const workers: MemberUnit[] = [
    member({
      participant_id: "r1",
      assignment_id: "r1",
      task_id: "tr1",
      name: "running worker",
      status: "running",
    }),
    member({
      participant_id: "w1",
      assignment_id: "w1",
      task_id: "tw1",
      name: "waiting worker",
      status: "needs_input",
    }),
    member({
      participant_id: "d1",
      assignment_id: "d1",
      task_id: "td1",
      name: "done worker",
      status: "done",
    }),
  ];

  it("showTaskList=true renders .task-card x3, only running has .tc-stop, click triggers onSelectTask", () => {
    const onSelectTask = vi.fn();
    const { container } = render(
      <I18nProvider>
        <RightPanel
          {...base}
          showTaskList={true}
          taskListWorkers={workers}
          onSelectTask={onSelectTask}
          onStopTask={() => {}}
          inspectorMember={null}
        />
      </I18nProvider>,
    );
    const cards = container.querySelectorAll(".task-card");
    expect(cards).toHaveLength(3);
    expect(container.querySelectorAll(".tc-stop")).toHaveLength(1);

    fireEvent.click(cards[0]);
    expect(onSelectTask).toHaveBeenCalledWith("r1");
  });

  it("running 任务点停止时将 assignment_id 传给 onStopTask", () => {
    const onStopTask = vi.fn();
    const { container } = render(
      <I18nProvider>
        <RightPanel
          {...base}
          showTaskList={true}
          taskListWorkers={workers}
          onSelectTask={() => {}}
          onStopTask={onStopTask}
          inspectorMember={null}
        />
      </I18nProvider>,
    );

    fireEvent.click(container.querySelector(".tc-stop")!);
    expect(onStopTask).toHaveBeenCalledWith("r1");
  });

  it("inspectorMember present + showTaskList=true => inspector wins, .tasklist absent", () => {
    const inspectorMember = member({
      participant_id: "x1",
      assignment_id: "x1",
      task_id: "tx1",
      name: "inspected worker",
      status: "done",
    });
    const { container } = render(
      <I18nProvider>
        <RightPanel
          {...base}
          showTaskList={true}
          taskListWorkers={workers}
          onSelectTask={() => {}}
          onStopTask={() => {}}
          inspectorMember={inspectorMember}
          onCloseInspector={() => {}}
        />
      </I18nProvider>,
    );
    expect(container.querySelector(".task-inspector")).toBeTruthy();
    expect(container.querySelector(".tasklist")).toBeFalsy();
  });
});

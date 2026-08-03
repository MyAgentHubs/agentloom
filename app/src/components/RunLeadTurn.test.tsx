import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { RunLeadTurn } from "./RunLeadTurn";
import type { LeadTurnView } from "../lib/leadTurns";
import type {
  CodingTaskBlock,
  LeadSummaryBlock,
  MemberUnit,
} from "../types/agent";

const member = (overrides: Partial<MemberUnit> = {}): MemberUnit => ({
  participant_id: "p1",
  assignment_id: "a1",
  task_id: "t1",
  name: "DeepSeekFlash",
  status: "running",
  sub: "检查类型",
  steps_total: 2,
  steps_done: 1,
  cost_usd: null,
  input_tokens: 0,
  output_tokens: 0,
  failed: false,
  blocks: [],
  ...overrides,
});

const codingTask = (
  overrides: Partial<CodingTaskBlock> = {},
): CodingTaskBlock => ({
  type: "coding_task",
  run_id: "r1",
  assignment_id: "c1",
  worker_name: "Codex",
  phase: "verifying",
  step_done: 2,
  step_total: 3,
  artifact_id: "art-1",
  verify_cmd: "npm test",
  detail: null,
  ...overrides,
});

const verdict = (
  overrides: Partial<LeadSummaryBlock> = {},
): LeadSummaryBlock => ({
  type: "lead_summary",
  run_id: "r1",
  summary_source: "lead_synthesis",
  status: { kind: "all_succeeded", succeeded_count: 2, total: 2 },
  sections: [
    {
      heading: "",
      body_richtext: "结论：任务完成。",
      findings: [],
      attribution: ["a1"],
      trace_ref: { run_id: "r1", assignment_ids: ["a1"] },
    },
  ],
  findings: [],
  artifact_refs: [],
  ...overrides,
});

type DecisionCardBlock = LeadTurnView["decisionCards"][number];

const dc = (
  id: string,
  overrides: Partial<DecisionCardBlock> = {},
): DecisionCardBlock => ({
  type: "decision_card",
  decision_id: id,
  kind: "ask",
  question: "Q?",
  options: ["A", "B"],
  recommended: "A",
  rationale: null,
  payload: null,
  source_run_id: "r1",
  status: "pending",
  chosen_option: null,
  created_at: 1,
  ...overrides,
});

const turn = (overrides: Partial<LeadTurnView> = {}): LeadTurnView => ({
  kind: "run",
  runId: "r1",
  lead: "Claude",
  codingTask: null,
  decisionCards: [],
  members: [],
  verdict: null,
  phase: "live",
  outcome: "running",
  showProcessFold: false,
  ...overrides,
});

describe("RunLeadTurn", () => {
  it("执行中只渲任务条，不渲 proc-fold，且只有一个真实队长 author", () => {
    const { container } = render(
      <RunLeadTurn
        turn={turn({
          members: [member({ name: "Kimi" })],
          codingTask: codingTask(),
          lead: "DeepSeekFlash",
        })}
      />,
    );

    expect(container.firstElementChild).toHaveClass("turn");
    expect(container.querySelectorAll(".turn__author")).toHaveLength(1);
    expect(container.querySelector(".turn__author")).toHaveTextContent(
      "DeepSeekFlash",
    );
    expect(screen.getByText("· 队长")).toBeInTheDocument();
    expect(container.querySelector(".taskstack")).not.toBeNull();
    expect(container.querySelectorAll(".task-row")).toHaveLength(1);
    expect(screen.getByText("Codex")).toBeInTheDocument();
    expect(screen.queryByText("Kimi")).not.toBeInTheDocument();
    expect(container.querySelector(".proc-fold")).toBeNull();
  });

  it("已落地态（applied）展示「已落地」状态·不再渲撤销按钮（撤销改为对话式）", () => {
    render(
      <RunLeadTurn
        turn={turn({
          codingTask: codingTask({
            phase: "applied",
            assignment_id: "c1",
            detail: "已落地到当前分支 · deadbeef",
          }),
        })}
      />,
    );
    expect(screen.getByText("已落地到当前分支 · deadbeef")).toBeInTheDocument();
    expect(
      screen.queryByRole("button", { name: "撤销本次落地" }),
    ).not.toBeInTheDocument();
  });

  it("turn.decisionCards 非空 → 渲出对应数量 .decision-card", () => {
    const { container } = render(
      <RunLeadTurn
        turn={turn({
          decisionCards: [dc("d1"), dc("d2")],
        })}
      />,
    );

    expect(container.querySelectorAll(".decision-card")).toHaveLength(2);
  });

  it("完成态先渲 verdict，再渲 proc-fold，展开后行内显示任务条", () => {
    const { container } = render(
      <RunLeadTurn
        turn={turn({
          members: [
            member({ assignment_id: "a1", name: "DeepSeekFlash" }),
            member({ assignment_id: "a2", name: "Kimi", sub: "检查样式" }),
          ],
          verdict: verdict(),
          phase: "terminal",
          outcome: "succeeded",
          showProcessFold: true,
        })}
      />,
    );

    const summary = container.querySelector(".lead-summary");
    const fold = container.querySelector(".proc-fold");
    expect(summary).not.toBeNull();
    expect(fold).not.toBeNull();
    expect(
      summary!.compareDocumentPosition(fold!) &
        Node.DOCUMENT_POSITION_FOLLOWING,
    ).toBeTruthy();
    expect(screen.getByText("结论：任务完成。")).toBeInTheDocument();
    expect(screen.getByText("过程：2 个任务")).toBeInTheDocument();

    fireEvent.click(container.querySelector(".pf-tog")!);

    expect(container.querySelector(".proc-fold.open")).not.toBeNull();
    expect(
      container.querySelector(".proc-fold.open .taskstack"),
    ).not.toBeNull();
    expect(screen.getByText("DeepSeekFlash")).toBeInTheDocument();
    expect(screen.getByText("Kimi")).toBeInTheDocument();
  });

  it("查看过程把当前 run id 上抛给右侧过程面板", () => {
    const onViewProcess = vi.fn();
    render(
      <RunLeadTurn
        turn={turn({
          members: [member({ status: "done" })],
          verdict: verdict(),
          phase: "terminal",
          outcome: "succeeded",
          showProcessFold: true,
        })}
        onViewProcess={onViewProcess}
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: "查看过程" }));
    expect(onViewProcess).toHaveBeenCalledWith("r1");
  });

  it("partial verdict 仍渲完成态骨架与 proc-fold", () => {
    const { container } = render(
      <RunLeadTurn
        turn={turn({
          members: [member()],
          verdict: verdict({
            status: { kind: "partial", succeeded_count: 1, total: 2 },
          }),
          phase: "terminal",
          outcome: "partial",
          showProcessFold: true,
        })}
      />,
    );

    expect(screen.getByText("部分完成 · 1/2")).toBeInTheDocument();
    expect(container.querySelector(".proc-fold")).not.toBeNull();
  });

  it.each([
    ["failed", { kind: "failed" as const, succeeded_count: 0, total: 1 }],
    ["partial", { kind: "partial" as const, succeeded_count: 1, total: 2 }],
  ])(
    "stopped member + %s verdict 渲染中性停止提示，不显示失败补救按钮",
    (_label, status) => {
      render(
        <RunLeadTurn
          turn={turn({
            members: [member({ status: "stopped" })],
            verdict: verdict({
              status,
              sections: [
                {
                  heading: "失败分析",
                  body_richtext: "这里是原失败正文。",
                  findings: [],
                  attribution: ["a1"],
                  trace_ref: { run_id: "r1", assignment_ids: ["a1"] },
                },
              ],
              findings: [
                { status: "miss", text: "typecheck 红", assignment_id: "a1" },
              ],
            }),
            phase: "terminal",
            outcome: status.kind,
          })}
        />,
      );

      expect(screen.getByText("已停止")).toBeInTheDocument();
      expect(screen.getByText(/已停下这个 worker/)).toBeInTheDocument();
      expect(screen.queryByText("失败分析")).not.toBeInTheDocument();
      expect(screen.queryByText("typecheck 红")).not.toBeInTheDocument();
      expect(screen.queryByText("没做到")).not.toBeInTheDocument();
      expect(screen.queryByText(/下一步怎么办/)).not.toBeInTheDocument();
      expect(screen.queryByText(/我接手/)).not.toBeInTheDocument();
      expect(screen.queryByText(/从头干净重派/)).not.toBeInTheDocument();
    },
  );

  it("failed member + failed verdict 显示失败原因和静态建议，不再显示补救按钮", () => {
    render(
      <RunLeadTurn
        turn={turn({
          members: [member({ status: "failed", failed: true })],
          verdict: verdict({
            status: { kind: "failed", succeeded_count: 0, total: 1 },
            findings: [
              { status: "miss", text: "typecheck 红", assignment_id: "a1" },
            ],
          }),
          phase: "terminal",
          outcome: "failed",
        })}
      />,
    );

    expect(screen.getByText("未完成 · 0/1")).toBeInTheDocument();
    expect(
      screen.getByText("worker 调用失败：typecheck 红"),
    ).toBeInTheDocument();
    expect(screen.queryByText("没做到")).not.toBeInTheDocument();
    expect(
      screen.getByText(/建议：换一个可用 worker 重派/),
    ).toBeInTheDocument();
    expect(screen.queryByText(/下一步怎么办/)).not.toBeInTheDocument();
    expect(screen.queryByText(/我接手/)).not.toBeInTheDocument();
    expect(screen.queryByText(/从头干净重派/)).not.toBeInTheDocument();
  });

  it("stopped + failed 混合终态仍显示失败建议，不用中性停止提示遮盖失败", () => {
    render(
      <RunLeadTurn
        turn={turn({
          members: [
            member({ assignment_id: "a1", status: "stopped" }),
            member({ assignment_id: "a2", status: "failed", failed: true }),
          ],
          verdict: verdict({
            status: { kind: "partial", succeeded_count: 0, total: 2 },
            findings: [
              { status: "miss", text: "用户停止", assignment_id: "a1" },
              { status: "miss", text: "typecheck 红", assignment_id: "a2" },
            ],
          }),
          phase: "terminal",
          outcome: "partial",
        })}
      />,
    );

    expect(screen.queryByText("已停止")).not.toBeInTheDocument();
    expect(screen.getByText("部分完成 · 0/2")).toBeInTheDocument();
    expect(screen.getByText("没做到")).toBeInTheDocument();
    expect(screen.getByText("typecheck 红")).toBeInTheDocument();
    expect(
      screen.getByText(/建议：换一个可用 worker 重派/),
    ).toBeInTheDocument();
    expect(screen.queryByText(/下一步怎么办/)).not.toBeInTheDocument();
  });

  it("terminal turn（有 verdict + showProcessFold）的决策卡在 turn body·不落进 .pf-list 折叠区", () => {
    const { container } = render(
      <RunLeadTurn
        turn={turn({
          verdict: verdict(),
          phase: "terminal",
          outcome: "succeeded",
          showProcessFold: true,
          decisionCards: [dc("d1")],
        })}
      />,
    );

    expect(container.querySelector(".decision-card")).not.toBeNull();
    expect(container.querySelector(".pf-list .decision-card")).toBeNull();
  });

  it("lead 名字缺失时兜底显示「队长」（走 i18n，非硬编码）", () => {
    const { container } = render(
      <RunLeadTurn
        turn={turn({
          members: [member({ name: "Kimi" })],
          codingTask: codingTask(),
          lead: "",
        })}
      />,
    );

    expect(container.querySelector(".turn__author")).toHaveTextContent("队长");
    // 名字兜底不应当作真实队员名匹配到 Kimi
    expect(screen.queryByText("Kimi")).not.toBeInTheDocument();
  });
});

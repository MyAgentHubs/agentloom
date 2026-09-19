import { fireEvent, render, screen } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { Block, MemberUnit } from "../types/agent";

vi.mock("./CodeBlock", () => ({
  CodeBlock: ({ code, lang }: { code: string; lang?: string }) => (
    <div data-lang={lang} data-testid="codeblock">
      {code}
    </div>
  ),
}));

vi.mock("./MermaidBlock", () => ({
  MermaidBlock: ({ code, complete }: { code: string; complete: boolean }) => (
    <div data-testid="mermaidblock" data-complete={String(complete)}>
      {code}
    </div>
  ),
}));

vi.mock("@tauri-apps/plugin-opener", () => ({
  openUrl: vi.fn().mockResolvedValue(undefined),
}));

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(),
  isTauri: vi.fn().mockReturnValue(false),
}));

import { invoke } from "@tauri-apps/api/core";
import { openUrl } from "@tauri-apps/plugin-opener";
import { I18nProvider } from "../i18n";
import { clearAttachmentCache } from "../lib/attachmentCache";
import { draftFromResult } from "../lib/gateReducer";
import type { ProposeResult } from "../types/gate";
import { MessageContent } from "./MessageContent";

beforeEach(() => {
  vi.mocked(invoke).mockReset();
  vi.mocked(openUrl).mockClear();
  clearAttachmentCache();
});

const R: ProposeResult = {
  runId: "r1",
  contractId: "r1-gc",
  goal: "做登录",
  tier: "tier2",
  riskLevel: "med",
  subtaskCount: 1,
  unassignedCount: 0,
  status: "draft",
  assignmentsJson: JSON.stringify([
    {
      subtask_id: "t1",
      subtask: "登录",
      assignee: null,
      scope_files: [],
      acceptance: [{ claim: "能登录", verifier: null }],
    },
  ]),
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
  input_tokens: 0,
  output_tokens: 0,
  failed: false,
  blocks: [],
  ...o,
});

// ─────────────────────────────────────────────────────────────────────────
// V3b（2026-08-26·刀②「桌面 chat verbose 分级」渲染接线）：MessageContent
// verbosity 折算 + ActivityFold + artifacts 段 + suppressArtifacts + scope_change
// 顺修断线。设计稿 desktop-verbose-design §2B / §2F 5
// ─────────────────────────────────────────────────────────────────────────

function toolBlock(
  overrides: Partial<Extract<Block, { type: "tool" }>> = {},
): Extract<Block, { type: "tool" }> {
  return {
    type: "tool",
    id: "t",
    tool: "Bash",
    summary: "run",
    card: "compact",
    status: "ok",
    exit_code: 0,
    output: null,
    ...overrides,
  };
}

function approvalBlock(
  status: Extract<Block, { type: "approval" }>["status"],
): Extract<Block, { type: "approval" }> {
  return {
    type: "approval",
    approval_id: `approval-${status}`,
    run_id: "run-1",
    tool: "Bash",
    command: "npm test",
    summary: "运行测试",
    cwd: "/repo",
    status,
  };
}

// 「5 工具 1 失败」场景：两组连续成功工具（各 2 个）夹一个失败工具，首尾各一段正文。
function fiveToolsOneFailedBlocks(): Block[] {
  return [
    { type: "text", text: "开始处理" },
    toolBlock({ id: "t1", tool: "Read", summary: "读文件 A" }),
    toolBlock({ id: "t2", tool: "Edit", summary: "改文件 B" }),
    toolBlock({
      id: "t3",
      tool: "Bash",
      summary: "跑测试",
      status: "failed",
      exit_code: 1,
      output: "boom",
    }),
    toolBlock({ id: "t4", tool: "Grep", summary: "搜索 X" }),
    toolBlock({ id: "t5", tool: "Write", summary: "写文件 C" }),
    { type: "text", text: "处理完成" },
  ];
}

describe("MessageContent verbosity 折算（V3b·三档 ×「5 工具 1 失败」）", () => {
  it("full 档：现状零变化——toolgroup 折叠 + 独立失败卡，零 activity-fold", () => {
    const { container } = render(
      <MessageContent
        blocks={fiveToolsOneFailedBlocks()}
        verbosity="full"
        onOpenPreview={() => {}}
        onOpenLightbox={() => {}}
      />,
    );

    expect(container.querySelectorAll(".activity-fold")).toHaveLength(0);
    expect(container.querySelectorAll(".toolfold")).toHaveLength(2);
    expect(screen.getByText("跑测试")).toBeInTheDocument();
    expect(screen.getByText("开始处理")).toBeInTheDocument();
  });

  it("summary 档：过程块折算成一枚 chip（含 5、1 计数），点开出现工具卡且失败行可见", () => {
    render(
      <MessageContent
        blocks={fiveToolsOneFailedBlocks()}
        verbosity="summary"
        onOpenPreview={() => {}}
        onOpenLightbox={() => {}}
      />,
    );

    const chip = screen.getByRole("button", { expanded: false });
    expect(chip.textContent).toContain("5");
    expect(chip.textContent).toContain("1");
    expect(screen.queryByText("跑测试")).toBeNull();

    fireEvent.click(chip);

    expect(chip).toHaveAttribute("aria-expanded", "true");
    expect(screen.getByText("跑测试")).toBeInTheDocument();
    expect(screen.getByText("失败")).toBeInTheDocument();
  });

  // P2-1（codex 整盘审）：英文 locale 下 n=1 的计数文案曾恒为复数「1 tools」——
  // ActivityFold 现按 n 选单/复数键（activity.fold.toolCount / toolsCount），
  // 这里用英文 locale + 单个成功工具（n=1，无失败/思考）锁死单数文案。
  it("英文 locale 下单个工具（n=1）计数文案是单数「1 tool」而非「1 tools」", () => {
    render(
      <I18nProvider initialLocale="en">
        <MessageContent
          blocks={[toolBlock({ id: "t1", tool: "Bash", summary: "run" })]}
          verbosity="summary"
          onOpenPreview={() => {}}
          onOpenLightbox={() => {}}
        />
      </I18nProvider>,
    );

    const chip = screen.getByRole("button", { expanded: false });
    expect(chip.textContent).toContain("1 tool");
    expect(chip.textContent).not.toContain("1 tools");
  });

  it("minimal 档：完成后不渲染过程块，只留含失败的一枚 chip", () => {
    const { container } = render(
      <MessageContent
        blocks={fiveToolsOneFailedBlocks()}
        verbosity="minimal"
        onOpenPreview={() => {}}
        onOpenLightbox={() => {}}
      />,
    );

    const folds = container.querySelectorAll(".activity-fold");
    expect(folds).toHaveLength(1);
    expect(folds[0].textContent).toContain("失败");
    expect(container.querySelectorAll(".toolfold")).toHaveLength(0);
    expect(screen.getByText("开始处理")).toBeInTheDocument();
    expect(screen.getByText("处理完成")).toBeInTheDocument();
  });

  it("verbosity 未传时默认 full（诊断消费方零改）", () => {
    const { container } = render(
      <MessageContent
        blocks={fiveToolsOneFailedBlocks()}
        onOpenPreview={() => {}}
        onOpenLightbox={() => {}}
      />,
    );

    expect(container.querySelectorAll(".activity-fold")).toHaveLength(0);
    expect(container.querySelectorAll(".toolfold")).toHaveLength(2);
  });
});

describe("MessageContent verbosity · 图片产物段（V3b §2A/§2B）", () => {
  beforeEach(() => {
    vi.mocked(invoke).mockResolvedValue({
      kind: "image",
      imageBase64: "aW1n",
      mediaType: "image/png",
    });
  });

  it("summary/minimal 档：过程段被折算/丢弃时，图片产物仍作为独立段渲染", async () => {
    render(
      <MessageContent
        blocks={[
          toolBlock({
            id: "t1",
            tool: "Bash",
            summary: "生成图",
            output: "saved to /abs/pic.png",
          }),
        ]}
        verbosity="minimal"
        sessionId="s1"
        onOpenPreview={() => {}}
        onOpenLightbox={() => {}}
      />,
    );

    expect(await screen.findByAltText("pic.png")).toBeInTheDocument();
  });

  it("展开 chip 后图片不重不丢：展开前后该图片在 DOM 里都只出现一次", async () => {
    render(
      <MessageContent
        blocks={[
          toolBlock({
            id: "t1",
            tool: "Bash",
            summary: "生成图",
            output: "saved to /abs/pic.png",
          }),
          toolBlock({
            id: "t2",
            tool: "Bash",
            summary: "跑测试",
            status: "failed",
            output: "x",
          }),
        ]}
        verbosity="summary"
        sessionId="s1"
        onOpenPreview={() => {}}
        onOpenLightbox={() => {}}
      />,
    );

    await screen.findByAltText("pic.png");
    expect(screen.getAllByAltText("pic.png")).toHaveLength(1);

    fireEvent.click(screen.getByRole("button", { expanded: false }));

    expect(screen.getAllByAltText("pic.png")).toHaveLength(1);
  });

  it("tool → text → tool 同图只挂一段：图片只渲染一次", async () => {
    render(
      <MessageContent
        blocks={[
          toolBlock({
            id: "t1",
            tool: "Bash",
            summary: "生成图",
            output: "saved to /abs/pic.png",
          }),
          { type: "text", text: "中间正文" },
          toolBlock({
            id: "t2",
            tool: "Bash",
            summary: "再次引用",
            output: "see /abs/pic.png again",
          }),
        ]}
        verbosity="summary"
        sessionId="s1"
        onOpenPreview={() => {}}
        onOpenLightbox={() => {}}
      />,
    );

    await screen.findByAltText("pic.png");
    expect(screen.getAllByAltText("pic.png")).toHaveLength(1);
  });

  it("suppressArtifacts 为真时 pass 段不渲染图片 chip", () => {
    render(
      <MessageContent
        blocks={[
          toolBlock({
            id: "t1",
            tool: "Bash",
            summary: "生成图",
            output: "saved to /abs/pic.png",
          }),
        ]}
        verbosity="full"
        suppressArtifacts
        sessionId="s1"
        onOpenPreview={() => {}}
        onOpenLightbox={() => {}}
      />,
    );

    expect(screen.queryByAltText("pic.png")).toBeNull();
  });

  // P2-2（codex 整盘审）：既有两条图片断言都用 ok 工具，只命中 toolgroup 分支
  // （streamItems.ts FOLD_THRESHOLD=1——哪怕只有一个 ok 工具也会成组）；failed/
  // running 状态的工具永远不进组，走的是 renderPassEntry 里 `block.type==="tool"`
  // 的单卡分支（pass-entry）。这里单独补一条失败工具的覆盖，同时覆盖
  // ActivityFold 展开态（recursion 带 suppressArtifacts）与直接 suppressArtifacts
  // 两种路径，证明单卡分支也走了同一条图片去重/抑制守卫，不是只有 toolgroup 分支有。
  it("单个 failed 工具带图片路径：展开 fold 后该图仍只出现一次（pass-entry 单卡分支）", async () => {
    render(
      <MessageContent
        blocks={[
          toolBlock({
            id: "t1",
            tool: "Bash",
            summary: "跑测试",
            status: "failed",
            exit_code: 1,
            output: "saved to /abs/fail-pic.png",
          }),
        ]}
        verbosity="summary"
        sessionId="s1"
        onOpenPreview={() => {}}
        onOpenLightbox={() => {}}
      />,
    );

    await screen.findByAltText("fail-pic.png");
    expect(screen.getAllByAltText("fail-pic.png")).toHaveLength(1);

    fireEvent.click(screen.getByRole("button", { expanded: false }));

    // 展开后递归渲染带 suppressArtifacts——failed 工具走的是单卡分支而非
    // toolgroup 分支，图片同样不该重渲。
    expect(screen.getAllByAltText("fail-pic.png")).toHaveLength(1);
  });

  it("suppressArtifacts 下 pass-entry 单卡分支（失败工具）不渲染图片", () => {
    render(
      <MessageContent
        blocks={[
          toolBlock({
            id: "t1",
            tool: "Bash",
            summary: "跑测试",
            status: "failed",
            exit_code: 1,
            output: "saved to /abs/fail-pic-2.png",
          }),
        ]}
        verbosity="full"
        suppressArtifacts
        sessionId="s1"
        onOpenPreview={() => {}}
        onOpenLightbox={() => {}}
      />,
    );

    expect(screen.queryByAltText("fail-pic-2.png")).toBeNull();
  });
});

describe("MessageContent verbosity · ActivityFold 开合与 rerender（V3b §2B）", () => {
  it("两个 activity_fold 独立开合", () => {
    render(
      <MessageContent
        blocks={[
          toolBlock({ id: "t1", tool: "Bash", summary: "第一组" }),
          { type: "text", text: "分隔正文" },
          toolBlock({ id: "t2", tool: "Bash", summary: "第二组" }),
        ]}
        verbosity="summary"
        onOpenPreview={() => {}}
        onOpenLightbox={() => {}}
      />,
    );

    const chips = screen.getAllByRole("button");
    expect(chips).toHaveLength(2);
    fireEvent.click(chips[0]);
    expect(chips[0]).toHaveAttribute("aria-expanded", "true");
    expect(chips[1]).toHaveAttribute("aria-expanded", "false");
  });

  it("切档（rerender 换 verbosity）后已展开的 fold 复位为收起", () => {
    const blocks: Block[] = [
      toolBlock({
        id: "t1",
        tool: "Bash",
        summary: "跑测试",
        status: "failed",
        output: "boom",
      }),
    ];
    const { rerender } = render(
      <MessageContent
        blocks={blocks}
        verbosity="summary"
        onOpenPreview={() => {}}
        onOpenLightbox={() => {}}
      />,
    );

    fireEvent.click(screen.getByRole("button"));
    expect(screen.getByRole("button")).toHaveAttribute("aria-expanded", "true");

    rerender(
      <MessageContent
        blocks={blocks}
        verbosity="minimal"
        onOpenPreview={() => {}}
        onOpenLightbox={() => {}}
      />,
    );

    expect(screen.getByRole("button")).toHaveAttribute(
      "aria-expanded",
      "false",
    );
  });

  it("interrupted 工具在 minimal 档不静默：chip 出现", () => {
    render(
      <MessageContent
        blocks={[
          toolBlock({
            id: "t1",
            tool: "Bash",
            summary: "被打断",
            status: "interrupted",
          }),
        ]}
        verbosity="minimal"
        onOpenPreview={() => {}}
        onOpenLightbox={() => {}}
      />,
    );

    const chip = screen.getByRole("button");
    expect(chip.textContent).toContain("中断");
  });

  it.each([
    { locale: "zh", text: "1 被拒" },
    { locale: "en", text: "1 rejected" },
  ] as const)(
    "rejectedCount 在 $locale locale 渲染为警告色 chip 文案",
    ({ locale, text }) => {
      render(
        <I18nProvider initialLocale={locale}>
          <MessageContent
            blocks={[approvalBlock("rejected")]}
            verbosity="summary"
          />
        </I18nProvider>,
      );

      const chip = screen.getByRole("button", { expanded: false });
      const warningCount = chip.querySelector(".activity-fold__count--warn");
      expect(warningCount).toHaveTextContent(text);
    },
  );

  it("同一 approval 从 pending → approved → tool 时 summary chip 不空白且工具只计一次", () => {
    const pending = approvalBlock("pending");
    const { rerender } = render(
      <MessageContent blocks={[pending]} verbosity="summary" streaming />,
    );
    expect(screen.getByText("等待决定")).toBeInTheDocument();

    const approved = { ...pending, status: "approved" as const };
    rerender(
      <MessageContent blocks={[approved]} verbosity="summary" streaming />,
    );
    const approvedChip = screen.getByRole("button", { expanded: false });
    expect(approvedChip).toHaveTextContent("已放行");

    rerender(
      <MessageContent
        blocks={[approved, toolBlock({ id: "approved-tool", status: "ok" })]}
        verbosity="summary"
        streaming={false}
      />,
    );
    const completedChip = screen.getByRole("button", { expanded: false });
    expect(completedChip).toHaveTextContent("1 工具");
    expect(completedChip).not.toHaveTextContent("2 工具");
  });

  it("minimal 直播瞬态的 approved chip 有文案，停流后按规则折掉", () => {
    const approved = approvalBlock("approved");
    const { rerender } = render(
      <MessageContent blocks={[approved]} verbosity="minimal" streaming />,
    );
    expect(screen.getByRole("button", { expanded: false })).toHaveTextContent(
      "已放行",
    );

    rerender(
      <MessageContent
        blocks={[approved]}
        verbosity="minimal"
        streaming={false}
      />,
    );
    expect(screen.queryByRole("button")).not.toBeInTheDocument();
  });

  it("rerender 计数：running → failed → done 三次 rerender，chip 文案随之变化", () => {
    const { rerender } = render(
      <MessageContent
        blocks={[
          toolBlock({
            id: "t1",
            tool: "Bash",
            summary: "构建中",
            status: "running",
          }),
        ]}
        verbosity="summary"
        streaming
        onOpenPreview={() => {}}
        onOpenLightbox={() => {}}
      />,
    );
    expect(screen.getByRole("button").textContent).toContain("正在运行");

    rerender(
      <MessageContent
        blocks={[
          toolBlock({
            id: "t1",
            tool: "Bash",
            summary: "构建中",
            status: "failed",
            output: "boom",
          }),
        ]}
        verbosity="summary"
        streaming={false}
        onOpenPreview={() => {}}
        onOpenLightbox={() => {}}
      />,
    );
    expect(screen.getByRole("button").textContent).toContain("失败");

    rerender(
      <MessageContent
        blocks={[
          toolBlock({
            id: "t1",
            tool: "Bash",
            summary: "构建中",
            status: "ok",
          }),
        ]}
        verbosity="summary"
        streaming={false}
        onOpenPreview={() => {}}
        onOpenLightbox={() => {}}
      />,
    );
    const finalText = screen.getByRole("button").textContent ?? "";
    expect(finalText).toContain("工具");
    expect(finalText).not.toContain("失败");
  });
});

describe("MessageContent verbosity · minimal 档 L0 参数化渲染（经 MessageContent，V3b §2F 5）", () => {
  it("approval 块在 minimal 档原样出现", () => {
    render(
      <MessageContent
        verbosity="minimal"
        sessionId="s1"
        blocks={[
          {
            type: "approval",
            approval_id: "ap1",
            run_id: "r1",
            tool: "Bash",
            command: "rm -rf tmp",
            summary: "清理临时文件",
            cwd: "/repo",
            status: "pending",
          },
        ]}
      />,
    );

    expect(screen.getByText("清理临时文件")).toBeInTheDocument();
  });

  it("scope_change 块在 minimal 档原样出现，点主按钮真触发 onContinueScope（顺修既有断线）", () => {
    const onContinueScope = vi.fn();
    render(
      <MessageContent
        verbosity="minimal"
        onContinueScope={onContinueScope}
        blocks={[
          {
            type: "scope_change",
            changes: [
              {
                proposal_id: "p1",
                kind: "scope",
                detail_text: "新范围详情",
                detail_summary: null,
              },
            ],
          },
        ]}
      />,
    );

    fireEvent.click(screen.getByText("采纳并继续"));

    expect(onContinueScope).toHaveBeenCalledTimes(1);
    expect(onContinueScope).toHaveBeenCalledWith(expect.any(String));
  });

  it("gate_card + gateView=draft 在 minimal 档原样出现", () => {
    render(
      <MessageContent
        verbosity="minimal"
        blocks={[{ type: "gate_card", session_id: "s1" }]}
        gateView={{ kind: "draft", draft: draftFromResult(R) }}
        leadName="Claude"
        enabledAgents={[]}
        onGateAction={() => {}}
        onGateFreeze={() => {}}
        onGateRedraft={() => {}}
      />,
    );

    expect(screen.getByText("草案")).toBeInTheDocument();
    expect(screen.getByText("能登录")).toBeInTheDocument();
  });

  it("draft_failed + 有效 gateView=failed 在 minimal 档原样出现", () => {
    render(
      <MessageContent
        verbosity="minimal"
        blocks={[{ type: "draft_failed", session_id: "s1" }]}
        gateView={{
          kind: "failed",
          failure: { kind: "invokeFailed", reason: "网络错误" },
          runId: "r1",
          contractId: "c1",
        }}
        onGateRetry={() => {}}
        onGateManual={() => {}}
        onGateBackToNormal={() => {}}
      />,
    );

    expect(screen.getByText(/网络错误/)).toBeInTheDocument();
  });

  it("run_terminal 非空 在 minimal 档原样出现", () => {
    render(
      <MessageContent
        verbosity="minimal"
        blocks={[
          {
            type: "run_terminal",
            run_id: "r1",
            status: "error",
            message: "网络超时",
          },
        ]}
      />,
    );

    expect(screen.getByText("网络超时")).toBeInTheDocument();
  });

  it("context_compacted 在 minimal 档原样出现", () => {
    render(
      <MessageContent
        verbosity="minimal"
        blocks={[{ type: "context_compacted" }]}
      />,
    );

    expect(screen.getByText("会话上下文已自动压实")).toBeInTheDocument();
  });

  it("context_truncated 在 minimal 档原样出现", () => {
    render(
      <MessageContent
        verbosity="minimal"
        blocks={[{ type: "context_truncated" }]}
      />,
    );

    expect(
      screen.getByText("上下文超出模型窗口，已截断部分早期内容"),
    ).toBeInTheDocument();
  });

  it("dispatch_card 在 minimal 档原样出现", () => {
    const m = member({
      participant_id: "p1",
      assignment_id: "a1",
      task_id: "t1",
      name: "DeepSeekFlash",
      status: "running",
      sub: "改 README",
      steps_total: 3,
      steps_done: 1,
    });
    const { container } = render(
      <MessageContent
        verbosity="minimal"
        blocks={[{ type: "dispatch_card", run_id: "w", member: m }]}
      />,
    );

    expect(container.querySelector(".workerrow")).not.toBeNull();
  });

  it("run_card 在 minimal 档原样出现", () => {
    const { container } = render(
      <MessageContent
        verbosity="minimal"
        blocks={[
          {
            type: "run_card",
            run_id: "r1",
            commit_sha: "abc123",
            files_changed: 2,
            insertions: 5,
            deletions: 1,
            interrupted: false,
          },
        ]}
        onViewRun={() => {}}
      />,
    );

    expect(container.querySelector(".run-card")).not.toBeNull();
  });

  it("text 块在 minimal 档原样出现（不被过程折算吞掉）", () => {
    render(
      <MessageContent
        verbosity="minimal"
        blocks={[{ type: "text", text: "普通正文" }]}
      />,
    );

    expect(screen.getByText("普通正文")).toBeInTheDocument();
  });

  it("image 块在 minimal 档原样出现", async () => {
    vi.mocked(invoke).mockResolvedValueOnce({
      kind: "image",
      imageBase64: "aW1n",
      mediaType: "image/png",
    });
    render(
      <MessageContent
        verbosity="minimal"
        sessionId="s1"
        blocks={[
          {
            type: "image",
            attachment_id: "/abs/x.png",
            media_type: "image/png",
          },
        ]}
      />,
    );

    expect(await screen.findByRole("img")).toBeInTheDocument();
  });
});

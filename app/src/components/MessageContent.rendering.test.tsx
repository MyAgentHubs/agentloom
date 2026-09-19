import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, test, vi } from "vitest";
// @ts-expect-error - Vitest runs in Node, but this frontend tsconfig has no Node type declarations.
import { readFileSync } from "fs";
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
}));

import { invoke } from "@tauri-apps/api/core";
import { openUrl } from "@tauri-apps/plugin-opener";
import { I18nProvider } from "../i18n";
import { clearAttachmentCache } from "../lib/attachmentCache";
import { draftFromResult } from "../lib/gateReducer";
import type { ProposeResult } from "../types/gate";
import { MessageContent } from "./MessageContent";

const text = (value: string): Block[] => [{ type: "text", text: value }];

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

describe("MessageContent", () => {
  it("gate_card 块 + gateView=draft → 渲 GateCard 草案", () => {
    render(
      <MessageContent
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

  it("streaming=true → 仍走 markdown 渲染，避免处理中闪成原始 Markdown", () => {
    const { container } = render(
      <MessageContent blocks={text("**bold**")} streaming />,
    );

    expect(container.querySelector("pre.turn__streaming")).toBeNull();
    expect(container.querySelector("strong")).not.toBeNull();
  });

  it("streaming=false → markdown 渲染（粗体/列表/inline code）", () => {
    const { container } = render(
      <MessageContent blocks={text("**bold** and `code`\n\n- item")} />,
    );

    expect(container.querySelector("strong")).not.toBeNull();
    expect(container.querySelector("code.inline")).not.toBeNull();
    expect(container.querySelector("li")).not.toBeNull();
  });

  it("可预览内联路径可点击，普通内联代码保持非按钮", () => {
    const onOpenPreview = vi.fn();
    const onOpenLightbox = vi.fn();
    render(
      <MessageContent
        blocks={text("`README.md` and `array.map`")}
        onOpenPreview={onOpenPreview}
        onOpenLightbox={onOpenLightbox}
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: "README.md" }));

    expect(onOpenPreview).toHaveBeenCalledWith("README.md");
    expect(onOpenLightbox).not.toHaveBeenCalled();
    expect(screen.getByText("array.map")).not.toHaveAttribute("role", "button");
  });

  it("html 内联路径点击后调用后端外部打开且不打开预览", async () => {
    vi.mocked(invoke).mockResolvedValueOnce(undefined);
    const onOpenPreview = vi.fn();
    render(
      <MessageContent
        blocks={text("`artifacts/report.HTML`")}
        sessionId="session-html"
        onOpenPreview={onOpenPreview}
      />,
    );

    const button = await screen.findByRole("button", {
      name: "在浏览器打开 report.HTML",
    });
    expect(button).toHaveAttribute("title", "在浏览器打开 report.HTML");
    fireEvent.click(button);

    await waitFor(() =>
      expect(invoke).toHaveBeenCalledWith("open_attachment_external", {
        sessionId: "session-html",
        path: "artifacts/report.HTML",
      }),
    );
    expect(onOpenPreview).not.toHaveBeenCalled();
  });

  it("非 html 内联路径仍打开预览", () => {
    const onOpenPreview = vi.fn();
    render(
      <MessageContent
        blocks={text("`README.md`")}
        sessionId="session-markdown"
        onOpenPreview={onOpenPreview}
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: "README.md" }));

    expect(onOpenPreview).toHaveBeenCalledWith("README.md");
    expect(invoke).not.toHaveBeenCalled();
  });

  it("html 外部打开失败时显示后端错误且不抛未处理异常", async () => {
    vi.mocked(invoke).mockRejectedValueOnce(
      'AL_ERR:file.openExternalFailed:{"detail":"boom"}',
    );
    const onOpenPreview = vi.fn();
    render(
      <MessageContent
        blocks={text("`broken.htm`")}
        sessionId="session-error"
        onOpenPreview={onOpenPreview}
      />,
    );

    fireEvent.click(
      await screen.findByRole("button", {
        name: "在浏览器打开 broken.htm",
      }),
    );

    expect(
      await screen.findByRole("status", {
        name: "无法在系统浏览器打开文件：boom",
      }),
    ).toBeInTheDocument();
    expect(onOpenPreview).not.toHaveBeenCalled();
  });

  it("markdown link 点击后用系统浏览器打开，不导航当前 webview", () => {
    render(
      <MessageContent blocks={text("[docs](https://example.com/docs)")} />,
    );

    const link = screen.getByRole("link", { name: "docs" });
    const event = new MouseEvent("click", { bubbles: true, cancelable: true });
    const prevented = !link.dispatchEvent(event);

    expect(prevented).toBe(true);
    expect(openUrl).toHaveBeenCalledWith("https://example.com/docs");
  });

  it("markdown 相对链接不交给系统浏览器", () => {
    vi.mocked(openUrl).mockClear();
    render(<MessageContent blocks={text("[中文](README.zh.md)")} />);

    fireEvent.click(screen.getByRole("link", { name: "中文" }));

    expect(openUrl).not.toHaveBeenCalled();
  });

  it("fenced 代码块 → CodeBlock（带 lang）", () => {
    render(<MessageContent blocks={text("```ts\nconst x=1\n```")} />);

    const codeBlock = screen.getByTestId("codeblock");
    expect(codeBlock).toHaveAttribute("data-lang", "ts");
    expect(codeBlock).toHaveTextContent("const x=1");
  });

  it("mermaid fenced 代码块 → MermaidBlock（默认 complete）", () => {
    render(<MessageContent blocks={text("```mermaid\ngraph TD;A-->B\n```")} />);

    const mermaidBlock = screen.getByTestId("mermaidblock");
    expect(mermaidBlock).toHaveTextContent("graph TD;A-->B");
    expect(mermaidBlock).toHaveAttribute("data-complete", "true");
    expect(screen.queryByTestId("codeblock")).not.toBeInTheDocument();
  });

  it("streaming mermaid fenced 代码块 → complete=false", () => {
    render(
      <MessageContent
        blocks={text("```mermaid\ngraph TD;A-->B\n```")}
        streaming
      />,
    );

    expect(screen.getByTestId("mermaidblock")).toHaveAttribute(
      "data-complete",
      "false",
    );
  });

  it("GFM 表格 → .mm-table-wrap 包裹", () => {
    const md = "| a | b |\n|---|---|\n| 1 | 2 |";
    const { container } = render(<MessageContent blocks={text(md)} />);

    expect(container.querySelector(".mm-table-wrap table")).not.toBeNull();
  });

  it("带 GFM 右对齐语法的表格仍强制左对齐", () => {
    const md = "| item | count |\n|---|---:|\n| apples | 12 |";
    const { container } = render(<MessageContent blocks={text(md)} />);

    const cells = container.querySelectorAll("td");
    expect(cells[1]).toHaveStyle({ textAlign: "left" });
  });

  it("表格迭代：常态 100% 自适应换行，横滚只作兜底", () => {
    const css = readFileSync("src/styles/global.css", "utf-8");

    expect(css).toMatch(/\.mm-table-wrap\s*\{[^}]*overflow-x:\s*auto/);
    expect(css).toMatch(/\.mm-table-wrap table\s*\{[^}]*width:\s*100%/);
    expect(css).toMatch(/\.mm-table-wrap th\s*\{[^}]*text-align:\s*left/);
    expect(css).toMatch(/\.mm-table-wrap td\s*\{[^}]*text-align:\s*left/);
    expect(css).toMatch(
      /\.mm-table-wrap td\s*\{[^}]*overflow-wrap:\s*anywhere/,
    );
    expect(css).not.toMatch(
      /\.mm-table-wrap (?:th|td)\s*\{[^}]*white-space:\s*nowrap/,
    );
    expect(css).not.toMatch(
      /\.mm-table-wrap (?:th|td)\s*\{[^}]*min-width:\s*140px/,
    );
  });

  it("raw HTML 走 skipHtml，不渲染 HTML 节点", () => {
    const { container } = render(
      <MessageContent blocks={text("<span data-x='bad'>bad</span> ok")} />,
    );

    expect(container.querySelector("span[data-x='bad']")).toBeNull();
    expect(container).toHaveTextContent("bad ok");
  });

  it("按块顺序渲染 text / tool / thinking 混排", () => {
    render(
      <MessageContent
        blocks={[
          { type: "text", text: "开始" },
          {
            type: "tool",
            id: "t1",
            tool: "Bash",
            summary: "npm test",
            card: "command",
            status: "running",
            exit_code: null,
            output: null,
          },
          { type: "thinking", text: "继续分析" },
          { type: "text", text: "结束" },
        ]}
      />,
    );

    expect(screen.getByText("开始")).toBeInTheDocument();
    expect(screen.getByText("npm test")).toBeInTheDocument();
    expect(screen.getByText(/thinking/i)).toBeInTheDocument();
    expect(screen.getByText("结束")).toBeInTheDocument();
  });

  describe("prose 排版契约（2026-05-31 · 拉回设计系统）", () => {
    const css = readFileSync("src/styles/global.css", "utf-8");

    it("正文收窄到 p,li = --ink-2（容器基色不变）", () => {
      // 分组 selector 断最后一个 .turn__text li {（首 selector 后是逗号、断不到）
      expect(css).toMatch(/\.turn__text li\s*\{[^}]*color:\s*var\(--ink-2\)/);
    });

    it("容器基色仍 --ink、未被改成 ink-2（锚行首避免误匹配 .turn--user .turn__text）", () => {
      expect(css).toMatch(
        /(^|\n)\.turn__text\s*\{[^}]*color:\s*var\(--ink\)\s*;/,
      );
      const container = css.match(/(^|\n)\.turn__text\s*\{([^}]*)\}/);
      expect(container?.[2]).not.toContain("--ink-2");
    });

    it("消息正文共享同一列宽上限，长 token 不顶穿视口", () => {
      const container = css.match(/(^|\n)\.turn__text\s*\{([^}]*)\}/);
      expect(container?.[2]).toContain("max-width: 100%");
      expect(container?.[2]).toContain("overflow-wrap: anywhere");
      expect(container?.[2]).toContain("word-break: break-word");
    });

    it("无语言代码块外层 pre 横向溢出受控，且不改变 .mm-code 横滚契约", () => {
      const { container } = render(
        <MessageContent blocks={text("```\na-very-long-code-line\n```")} />,
      );

      expect(
        container.querySelector(".turn__text > pre > code.inline"),
      ).not.toBeNull();
      expect(css).toMatch(/\.turn__text > pre\s*\{[^}]*max-width:\s*100%/);
      expect(css).toMatch(/\.turn__text > pre\s*\{[^}]*overflow-x:\s*auto/);
      expect(css).toMatch(/\.mm-code\s*\{[^}]*overflow:\s*hidden/);
      expect(css).toMatch(/\.mm-code pre\s*\{[^}]*overflow-x:\s*auto/);
    });

    it("用户气泡可撑到与 LLM 回复同列宽，不再被 75% 封顶", () => {
      const bubble = css.match(/\.turn--user \.turn__text\s*\{([^}]*)\}/);
      expect(bubble?.[1]).toContain("width: fit-content");
      expect(bubble?.[1]).toContain("max-width: 100%");
      expect(bubble?.[1]).not.toContain("75%");
    });

    it("加粗 = --ink + 600（断分组最后 selector .turn__text b {）", () => {
      expect(css).toMatch(/\.turn__text b\s*\{[^}]*font-weight:\s*600/);
      expect(css).toMatch(/\.turn__text b\s*\{[^}]*color:\s*var\(--ink\)/);
    });

    it("引用内段落回到弱一级 color: inherit（不被 p,li 提亮）", () => {
      expect(css).toMatch(
        /\.turn__text blockquote li\s*\{[^}]*color:\s*inherit/,
      );
    });

    it("标题受控字阶：h1 17px + 全 h1–h6 600", () => {
      expect(css).toMatch(/\.turn__text h1\s*\{[^}]*font-size:\s*17px/);
      expect(css).toMatch(/\.turn__text h6\s*\{[^}]*font-weight:\s*600/);
    });

    it("hr = 暖 1px --line 线", () => {
      expect(css).toMatch(
        /\.turn__text hr\s*\{[^}]*border-top:\s*1px solid var\(--line\)/,
      );
    });

    it("流式裸文本同色 --ink-2", () => {
      expect(css).toMatch(/\.turn__streaming\s*\{[^}]*color:\s*var\(--ink-2\)/);
    });

    it("DOM：markdown 产出 strong / h1 / hr / blockquote>p", () => {
      const md = "# 标题\n\n**粗** 正文\n\n> 引用\n\n---\n";
      const { container } = render(<MessageContent blocks={text(md)} />);
      expect(container.querySelector("h1")).not.toBeNull();
      expect(container.querySelector("strong")).not.toBeNull();
      expect(container.querySelector("hr")).not.toBeNull();
      expect(container.querySelector("blockquote p")).not.toBeNull();
    });
  });
});

describe("MessageContent 巨型文本块折叠（T7）", () => {
  // 阈值 100_000 字符；用带 markdown 语法的重复串既超阈值又能验证「不整体走 markdown」。
  const hugeMarkdownish = "# 标题 **粗体** ".repeat(9000); // 108000 字符
  const previewHead = hugeMarkdownish.slice(0, 4000);

  it("超阈值块默认折叠：只渲染前 4000 字符预览，全文不进 DOM，且不走 MarkdownBody（无 strong/h1）", () => {
    const { container } = render(
      <MessageContent blocks={text(hugeMarkdownish)} />,
    );

    expect(container.textContent).toContain(previewHead);
    expect(container.textContent).not.toContain(hugeMarkdownish);
    expect(container.querySelector("strong")).toBeNull();
    expect(container.querySelector("h1")).toBeNull();
    expect(screen.getByRole("button", { name: /108000/ })).toBeInTheDocument();
  });

  it("点展开 → 全文以 pre-wrap 纯文本出现（仍不走 markdown）；再点收起回预览", () => {
    const { container } = render(
      <MessageContent blocks={text(hugeMarkdownish)} />,
    );

    fireEvent.click(screen.getByRole("button", { name: /108000/ }));

    expect(container.textContent).toContain(hugeMarkdownish);
    expect(container.querySelector("strong")).toBeNull();
    expect(container.querySelector("h1")).toBeNull();
    const body = container.querySelector(".huge-text__body");
    expect(body).not.toBeNull();

    fireEvent.click(screen.getByRole("button", { name: "收起" }));

    expect(container.textContent).toContain(previewHead);
    expect(container.textContent).not.toContain(hugeMarkdownish);
  });

  it("streaming 中的超阈值文本块同样走折叠预览，不整体走 markdown", () => {
    const { container } = render(
      <MessageContent blocks={text(hugeMarkdownish)} streaming />,
    );

    expect(container.textContent).toContain(previewHead);
    expect(container.textContent).not.toContain(hugeMarkdownish);
    expect(container.querySelector("strong")).toBeNull();
  });

  it("阈值内文本块行为不变：仍走 MarkdownBody 正常渲染", () => {
    const { container } = render(
      <MessageContent blocks={text("**bold** and `code`\n\n- item")} />,
    );

    expect(container.querySelector("strong")).not.toBeNull();
    expect(container.querySelector("code.inline")).not.toBeNull();
    expect(container.querySelector("li")).not.toBeNull();
    expect(container.querySelector(".huge-text")).toBeNull();
  });

  it("恰好 50000 字符仍走 MarkdownBody（阈值取 > 不取 >=，D3 P2①）", () => {
    const exact = "a".repeat(50_000);
    const { container } = render(<MessageContent blocks={text(exact)} />);
    expect(container.querySelector(".huge-text")).toBeNull();
  });

  it("50001 字符即折叠（超出阈值 1 个字符也要折）", () => {
    const overByOne = "a".repeat(50_001);
    const { container } = render(<MessageContent blocks={text(overByOne)} />);
    expect(container.querySelector(".huge-text")).not.toBeNull();
  });

  it("预览末字符恰为高位代理时整体去掉，避免劈半渲出替换字符（D3 P2②）", () => {
    const prefix = "a".repeat(3999);
    const emoji = "😀"; // 😀：高位代理 \uD83D + 低位代理 \uDE00
    const filler = "b".repeat(60_000);
    const huge = prefix + emoji + filler; // slice(0, 4000) 恰好切在代理对中间
    const { container } = render(<MessageContent blocks={text(huge)} />);
    const body = container.querySelector(".huge-text__body");
    expect(body?.textContent?.length).toBe(3999);
    expect(body?.textContent).not.toContain("�");
    expect(body?.textContent).not.toMatch(/[\uD800-\uDBFF]$/);
  });
});

describe("MessageContent · 未知块类型守卫（msgfix2 F2 S1，兑现 M0 §10.11「不识别的块类型不崩溃」）", () => {
  it("未知块类型（如后端新发的 activity_summary_v99）不抛异常，降级渲染一行提示，且不影响同消息内其它块正常渲染", () => {
    const blocks: Block[] = [
      { type: "text", text: "before" },
      { type: "activity_summary_v99", foo: "bar" } as unknown as Block,
      { type: "text", text: "after" },
    ];

    expect(() => render(<MessageContent blocks={blocks} />)).not.toThrow();

    expect(screen.getByText("before")).toBeInTheDocument();
    expect(screen.getByText("after")).toBeInTheDocument();
    expect(screen.getByText("[未知内容块]")).toBeInTheDocument();
  });
});

const teamRunBlocks: Block[] = [
  {
    type: "team_run",
    run_id: "r1",
    goal: null,
    lead: "Claude",
    members: [
      member({ assignment_id: "a1", name: "worker-1" }),
      member({
        assignment_id: "a2",
        name: "worker-2",
        status: "done",
        steps_done: 4,
        steps_total: 4,
        result: {
          changed_files: [{ path: "src/x.ts", insertions: 3, deletions: 1 }],
        } as any,
      }),
    ],
  },
];

describe("MessageContent team_run", () => {
  it("team_run 渲后台任务条（taskstack）·无 lead 壳/无 livestream（块B）", () => {
    const { container } = render(<MessageContent blocks={teamRunBlocks} />);
    expect(container.querySelector(".taskstack")).not.toBeNull();
    expect(container.querySelector(".team-run__lead")).toBeNull();
    expect(container.querySelector(".livestream")).toBeNull();
    expect(screen.getByText("worker-1")).toBeInTheDocument();
  });

  it("执行中队员渲任务行（st-run 颜色态 + 队员名）·非 LiveStreamCard（块B）", () => {
    const { container } = render(<MessageContent blocks={teamRunBlocks} />);
    expect(container.querySelector(".taskstack")).not.toBeNull();
    // 状态由 bar 颜色 class 声明（不再用「进行中」状态徽标文字）
    expect(container.querySelector(".task-row.st-run")).not.toBeNull();
    expect(screen.getByText("worker-1")).toBeInTheDocument();
    expect(container.querySelector(".livestream")).toBeNull();
  });

  it("终态队员主区不渲 diff（diff 去右侧 Review）·任务条保留（块B）", () => {
    const { container } = render(<MessageContent blocks={teamRunBlocks} />);
    expect(screen.queryByText("src/x.ts")).not.toBeInTheDocument();
    expect(container.querySelector(".taskstack")).not.toBeNull();
    expect(screen.getByText("worker-1")).toBeInTheDocument();
  });

  it("终态队员主区不再渲 member.blocks 叙述墙", () => {
    const blocks: Block[] = [
      {
        type: "team_run",
        run_id: "r1",
        goal: null,
        lead: "Claude",
        members: [
          member({
            assignment_id: "a2",
            name: "worker-2",
            status: "done",
            steps_done: 4,
            steps_total: 4,
            blocks: [{ type: "text", text: "队员的大段叙述墙文本" }],
          }),
        ],
      },
    ];
    render(<MessageContent blocks={blocks} />);
    expect(screen.queryByText("队员的大段叙述墙文本")).not.toBeInTheDocument();
  });

  it("点执行中队员卡上抛 onOpenMember(runId, assignmentId)", () => {
    const onOpenMember = vi.fn();
    render(
      <MessageContent blocks={teamRunBlocks} onOpenMember={onOpenMember} />,
    );
    fireEvent.click(screen.getByText("worker-1").closest('[role="button"]')!);
    expect(onOpenMember).toHaveBeenCalledWith("r1", "a1");
  });
});

describe("MessageContent coding_task", () => {
  it("渲染右侧状态 task-badge 且不再显示查看文字", () => {
    const blocks: Block[] = [
      {
        type: "coding_task",
        run_id: "r1",
        assignment_id: "a1",
        worker_name: "worker-1",
        phase: "applied",
      },
    ];

    const { container } = render(<MessageContent blocks={blocks} />);

    const badge = container.querySelector(".task-badge.st-done");
    expect(badge).not.toBeNull();
    expect(badge).toHaveTextContent("已完成");
    expect(screen.queryByText("查看")).not.toBeInTheDocument();
  });
});

describe("MessageContent 工具步骤折叠（F2）", () => {
  const okTool = (summary: string): Block => ({
    type: "tool",
    id: summary,
    tool: "bash",
    summary,
    card: "command",
    status: "ok",
    exit_code: 0,
    output: null,
  });

  const failedTool = (summary: string): Block => ({
    type: "tool",
    id: summary,
    tool: "bash",
    summary,
    card: "command",
    status: "failed",
    exit_code: 1,
    output: null,
  });

  test("连续成功工具卡渲成一条「执行了 N 步」折叠条", () => {
    render(
      <MessageContent
        blocks={[okTool("ls"), okTool("cat a"), okTool("cd b")]}
      />,
    );
    expect(screen.getByText("执行了 3 步")).toBeInTheDocument();
  });

  test("单条成功工具卡也折成一组", () => {
    render(<MessageContent blocks={[okTool("ls")]} />);
    expect(screen.getByText("执行了 1 步")).toBeInTheDocument();
  });

  test("失败卡打断连续段、自己单独渲染、不被折进组", () => {
    render(
      <MessageContent
        blocks={[
          okTool("ls"),
          okTool("cat a"),
          okTool("cd b"),
          failedTool("rm x"),
        ]}
      />,
    );
    expect(screen.getByText("执行了 3 步")).toBeInTheDocument();
    // 失败卡逃逸出组，单独渲染（compact 卡仍可见 summary 文本）
    expect(screen.getByText("rm x")).toBeInTheDocument();
  });

  test("所有 toolgroup 默认收起，尾随失败卡仍单独渲染", () => {
    const { container } = render(
      <MessageContent
        blocks={[
          okTool("ls"),
          { type: "text", text: "继续" },
          okTool("cat a"),
          failedTool("rm x"),
        ]}
      />,
    );
    const folds = container.querySelectorAll("details.toolfold");

    expect(folds).toHaveLength(2);
    expect(folds[0].hasAttribute("open")).toBe(false);
    expect(folds[1].hasAttribute("open")).toBe(false);
    expect(screen.getByText("rm x")).toBeInTheDocument();
  });

  test("新组出现时仍全部默认收起", () => {
    const { container, rerender } = render(
      <MessageContent blocks={[okTool("ls")]} />,
    );
    expect(
      container.querySelector("details.toolfold")?.hasAttribute("open"),
    ).toBe(false);

    rerender(
      <MessageContent
        blocks={[okTool("ls"), { type: "text", text: "继续" }, okTool("cat a")]}
      />,
    );
    const folds = container.querySelectorAll("details.toolfold");
    expect(folds).toHaveLength(2);
    expect(folds[0].hasAttribute("open")).toBe(false);
    expect(folds[1].hasAttribute("open")).toBe(false);
  });
});

describe("MessageContent lead_summary", () => {
  it("passes the session id into lead summary rendering", async () => {
    vi.mocked(invoke).mockResolvedValueOnce({
      kind: "image",
      imageBase64: "cmVsYXRpdmU=",
      mediaType: "image/png",
    });

    render(
      <MessageContent
        blocks={[
          {
            type: "lead_summary",
            run_id: "r1",
            summary_source: "lead_synthesis",
            status: {
              kind: "all_succeeded",
              succeeded_count: 1,
              total: 1,
            },
            sections: [
              {
                heading: "",
                body_richtext: "![chart](assets/x.png)",
                findings: [],
                attribution: ["a1"],
                trace_ref: { run_id: "r1", assignment_ids: ["a1"] },
              },
            ],
            findings: [],
            artifact_refs: [],
          },
        ]}
        sessionId="s-1"
      />,
    );

    await waitFor(() =>
      expect(invoke).toHaveBeenCalledWith("read_attachment", {
        path: "assets/x.png",
        sessionId: "s-1",
      }),
    );
  });

  it("forwards fallback preview clicks from a reloaded raw lead summary", async () => {
    vi.mocked(invoke).mockRejectedValueOnce(new Error("missing attachment"));
    const onOpenPreview = vi.fn();

    render(
      <MessageContent
        blocks={[
          {
            type: "lead_summary",
            run_id: "reload-preview",
            summary_source: "lead_synthesis",
            status: {
              kind: "all_succeeded",
              succeeded_count: 1,
              total: 1,
            },
            sections: [
              {
                heading: "",
                body_richtext: "![reload](assets/reload-preview.png)",
                findings: [],
                attribution: ["a1"],
                trace_ref: {
                  run_id: "reload-preview",
                  assignment_ids: ["a1"],
                },
              },
            ],
            findings: [],
            artifact_refs: [],
          },
        ]}
        onOpenPreview={onOpenPreview}
      />,
    );

    fireEvent.click(
      await screen.findByRole("button", {
        name: "assets/reload-preview.png",
      }),
    );
    expect(onOpenPreview).toHaveBeenCalledWith("assets/reload-preview.png");
  });
});

describe("MessageContent dispatch_card", () => {
  it("dispatch_card 块 → 渲 DispatchCard（.workerrow 存在·未落 markdown 默认分支）", () => {
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
      <I18nProvider>
        <MessageContent
          blocks={[{ type: "dispatch_card", run_id: "w", member: m }]}
        />
      </I18nProvider>,
    );
    expect(container.querySelector(".workerrow")).not.toBeNull();
  });
});

describe("MessageContent run_terminal", () => {
  it("run_terminal 块（error·带 message）→ 渲 .run-terminal 状态条，未落 markdown 默认分支", () => {
    const { container } = render(
      <I18nProvider>
        <MessageContent
          blocks={[
            {
              type: "run_terminal",
              run_id: "r1",
              status: "error",
              message: "网络超时",
            },
          ]}
        />
      </I18nProvider>,
    );
    expect(container.querySelector(".run-terminal")).not.toBeNull();
    expect(screen.getByText("出错")).toBeInTheDocument();
    expect(screen.getByText("网络超时")).toBeInTheDocument();
  });

  it("run_terminal 块（completed·无 message）→ 不渲染任何东西", () => {
    const { container } = render(
      <I18nProvider>
        <MessageContent
          blocks={[
            {
              type: "run_terminal",
              run_id: "r1",
              status: "completed",
              message: null,
            },
          ]}
        />
      </I18nProvider>,
    );
    expect(container.querySelector(".run-terminal")).toBeNull();
    expect(container.textContent).toBe("");
  });
});

describe("MessageContent context_compacted", () => {
  it("context_compacted 块 → 分发为一行压实提示，未落 markdown 默认分支", () => {
    const { container } = render(
      <I18nProvider initialLocale="zh">
        <MessageContent blocks={[{ type: "context_compacted" }]} />
      </I18nProvider>,
    );

    expect(container.querySelector(".context-compacted-chip")).not.toBeNull();
    expect(screen.getByText("会话上下文已自动压实")).toBeInTheDocument();
  });
});

describe("MessageContent context_truncated", () => {
  it("context_truncated 块 → 分发为一行截断告警，未落 markdown 默认分支", () => {
    const { container } = render(
      <I18nProvider initialLocale="zh">
        <MessageContent blocks={[{ type: "context_truncated" }]} />
      </I18nProvider>,
    );

    expect(container.querySelector(".context-truncated-chip")).not.toBeNull();
    expect(container.querySelector(".context-compacted-chip")).toBeNull();
    expect(
      screen.getByText("上下文超出模型窗口，已截断部分早期内容"),
    ).toBeInTheDocument();
  });
});

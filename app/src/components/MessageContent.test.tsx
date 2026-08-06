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
import { computeImageMenuPosition, MessageContent } from "./MessageContent";

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

describe("computeImageMenuPosition", () => {
  it("右侧空间足够时将菜单放在图片右侧外", () => {
    expect(
      computeImageMenuPosition(
        { left: 100, right: 220, top: 80, bottom: 200 },
        { x: 160, y: 140 },
        { width: 800, height: 600 },
      ),
    ).toEqual({ left: 228, top: 140 });
  });

  it("右侧空间不足但左侧足够时将菜单放在图片左侧外", () => {
    expect(
      computeImageMenuPosition(
        { left: 400, right: 760, top: 80, bottom: 200 },
        { x: 700, y: 140 },
        { width: 800, height: 600 },
      ),
    ).toEqual({ left: 212, top: 140 });
  });

  it("图片两侧都放不下时回退到经过 clamp 的光标位置", () => {
    expect(
      computeImageMenuPosition(
        { left: 100, right: 700, top: 80, bottom: 200 },
        { x: 790, y: 590 },
        { width: 800, height: 600 },
      ),
    ).toEqual({ left: 612, top: 496 });
  });

  it("将菜单的竖直位置 clamp 在视口边距内", () => {
    const rect = { left: 100, right: 220, top: 80, bottom: 200 };
    const viewport = { width: 800, height: 600 };

    expect(
      computeImageMenuPosition(rect, { x: 160, y: -20 }, viewport),
    ).toEqual({ left: 228, top: 8 });
    expect(
      computeImageMenuPosition(rect, { x: 160, y: 590 }, viewport),
    ).toEqual({ left: 228, top: 496 });
  });

  it("无光标位置时使用图片顶部偏移作为键盘菜单的纵向锚点", () => {
    expect(
      computeImageMenuPosition(
        { left: 100, right: 220, top: 80, bottom: 200 },
        undefined,
        { width: 800, height: 600 },
      ),
    ).toEqual({ left: 228, top: 92 });
  });

  it("无光标且左右都放不下时将菜单放在图片下方外", () => {
    const imageRect = { left: 50, right: 210, top: 80, bottom: 160 };
    const position = computeImageMenuPosition(imageRect, undefined, {
      width: 300,
      height: 400,
    });
    const menuRect = {
      left: position.left,
      right: position.left + 180,
      top: position.top,
      bottom: position.top + 96,
    };

    expect(position).toEqual({ left: 50, top: 168 });
    expect(
      menuRect.left < imageRect.right &&
        menuRect.right > imageRect.left &&
        menuRect.top < imageRect.bottom &&
        menuRect.bottom > imageRect.top,
    ).toBe(false);
  });

  it("无光标且左右下方都放不下时将菜单放在图片上方外", () => {
    const imageRect = { left: 50, right: 210, top: 140, bottom: 230 };
    const position = computeImageMenuPosition(imageRect, undefined, {
      width: 300,
      height: 260,
    });
    const menuRect = {
      left: position.left,
      right: position.left + 180,
      top: position.top,
      bottom: position.top + 96,
    };

    expect(position).toEqual({ left: 50, top: 36 });
    expect(
      menuRect.left < imageRect.right &&
        menuRect.right > imageRect.left &&
        menuRect.top < imageRect.bottom &&
        menuRect.bottom > imageRect.top,
    ).toBe(false);
  });
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

  it("command 卡 output 含图片路径 → 内联渲染有界缩略图并可点击", async () => {
    vi.mocked(invoke).mockResolvedValueOnce({
      kind: "image",
      imageBase64: "dGh1bWI=",
      mediaType: "image/png",
    });
    const onOpenPreview = vi.fn();
    const onOpenLightbox = vi.fn();
    render(
      <MessageContent
        blocks={[
          {
            type: "tool",
            id: "t-image",
            tool: "Bash",
            summary: "render image artifact",
            card: "command",
            status: "ok",
            exit_code: 0,
            output: "saved to /abs/path/moon.png",
          },
        ]}
        sessionId="session-artifact"
        onOpenPreview={onOpenPreview}
        onOpenLightbox={onOpenLightbox}
      />,
    );

    const image = await screen.findByAltText("moon.png");
    expect(image.tagName).toBe("IMG");
    expect(image).toHaveAttribute("src", "data:image/png;base64,dGh1bWI=");
    expect(image).toHaveStyle({
      maxHeight: "240px",
      maxWidth: "100%",
      objectFit: "contain",
      height: "auto",
    });
    expect(invoke).toHaveBeenCalledWith("read_attachment", {
      path: "/abs/path/moon.png",
      sessionId: "session-artifact",
    });

    fireEvent.click(image);

    expect(onOpenLightbox).toHaveBeenCalledWith("/abs/path/moon.png");
    expect(onOpenPreview).not.toHaveBeenCalled();
  });

  it("工具图片右键菜单可复制完整路径，且不影响左键预览", async () => {
    vi.mocked(invoke).mockResolvedValueOnce({
      kind: "image",
      imageBase64: "dGh1bWI=",
      mediaType: "image/png",
    });
    const writeText = vi.fn().mockResolvedValue(undefined);
    Object.defineProperty(navigator, "clipboard", {
      configurable: true,
      value: { writeText },
    });
    const onOpenPreview = vi.fn();
    const onOpenLightbox = vi.fn();
    render(
      <MessageContent
        blocks={[
          {
            type: "tool",
            id: "t-image-menu-path",
            tool: "Bash",
            summary: "render image artifact",
            card: "command",
            status: "ok",
            exit_code: 0,
            output: "saved to /abs/path/moon.png",
          },
        ]}
        onOpenPreview={onOpenPreview}
        onOpenLightbox={onOpenLightbox}
      />,
    );

    const image = await screen.findByAltText("moon.png");
    fireEvent.contextMenu(image);

    const menu = screen.getByRole("menu");
    expect(menu).toBeInTheDocument();
    expect(menu.parentElement).toBe(document.body);
    expect(
      screen.getByRole("menuitem", { name: "复制图片" }),
    ).toBeInTheDocument();
    fireEvent.click(screen.getByRole("menuitem", { name: "复制全路径" }));

    await waitFor(() =>
      expect(writeText).toHaveBeenCalledWith("/abs/path/moon.png"),
    );
    expect(screen.queryByRole("menu")).toBeNull();
    const feedback = await screen.findByText("路径已复制", {
      selector: '[role="status"]',
    });
    expect(feedback.parentElement).toBe(document.body);

    fireEvent.click(image);
    expect(onOpenLightbox).toHaveBeenCalledWith("/abs/path/moon.png");
    expect(onOpenPreview).not.toHaveBeenCalled();
  });

  it("右键另一张工具图片时只保留新图片的菜单", async () => {
    vi.mocked(invoke)
      .mockResolvedValueOnce({
        kind: "image",
        imageBase64: "Zmlyc3Q=",
        mediaType: "image/png",
      })
      .mockResolvedValueOnce({
        kind: "image",
        imageBase64: "c2Vjb25k",
        mediaType: "image/png",
      });
    render(
      <MessageContent
        blocks={[
          {
            type: "tool",
            id: "t-image-menu-single-open",
            tool: "Bash",
            summary: "render image artifacts",
            card: "command",
            status: "ok",
            exit_code: 0,
            output: "saved /abs/path/first.png and /abs/path/second.png",
          },
        ]}
        onOpenPreview={vi.fn()}
        onOpenLightbox={vi.fn()}
      />,
    );

    const firstImage = await screen.findByAltText("first.png");
    const secondImage = await screen.findByAltText("second.png");
    fireEvent.contextMenu(firstImage);
    const firstMenu = screen.getByRole("menu");

    fireEvent.contextMenu(secondImage);

    const menus = screen.getAllByRole("menu");
    expect(menus).toHaveLength(1);
    expect(firstMenu).not.toBeInTheDocument();
    expect(menus[0]).toBeInTheDocument();
  });

  it("图片菜单支持键盘打开、Esc 和外部点击关闭", async () => {
    vi.mocked(invoke).mockResolvedValueOnce({
      kind: "image",
      imageBase64: "dGh1bWI=",
      mediaType: "image/png",
    });
    render(
      <MessageContent
        blocks={[
          {
            type: "tool",
            id: "t-image-menu-keyboard",
            tool: "Bash",
            summary: "render image artifact",
            card: "command",
            status: "ok",
            exit_code: 0,
            output: "saved to /abs/path/moon.png",
          },
        ]}
        onOpenPreview={vi.fn()}
        onOpenLightbox={vi.fn()}
      />,
    );

    const preview = await screen.findByRole("button", {
      name: "预览图片 moon.png",
    });
    fireEvent.keyDown(preview, { key: "F10", shiftKey: true });
    expect(screen.getByRole("menu")).toBeInTheDocument();

    fireEvent.keyDown(document, { key: "Escape" });
    expect(screen.queryByRole("menu")).toBeNull();

    fireEvent.contextMenu(preview);
    fireEvent.pointerDown(document.body);
    expect(screen.queryByRole("menu")).toBeNull();
  });

  it("工具图片右键可写入图片；clipboard.write 不可用时禁用该项但仍可复制路径", async () => {
    vi.mocked(invoke)
      .mockResolvedValueOnce({
        kind: "image",
        imageBase64: "dGh1bWI=",
        mediaType: "image/png",
      })
      .mockResolvedValueOnce({
        kind: "image",
        imageBase64: "dGh1bWI=",
        mediaType: "image/png",
      });
    const write = vi.fn().mockResolvedValue(undefined);
    const writeText = vi.fn().mockResolvedValue(undefined);
    class MockClipboardItem {
      constructor(public items: Record<string, Blob>) {}
    }
    vi.stubGlobal("ClipboardItem", MockClipboardItem);
    Object.defineProperty(navigator, "clipboard", {
      configurable: true,
      value: { write, writeText },
    });
    const imageBlock: Block[] = [
      {
        type: "tool",
        id: "t-image-menu-copy",
        tool: "Bash",
        summary: "render image artifact",
        card: "command",
        status: "ok",
        exit_code: 0,
        output: "saved to /abs/path/moon.png",
      },
    ];
    const first = render(
      <MessageContent
        blocks={imageBlock}
        onOpenPreview={vi.fn()}
        onOpenLightbox={vi.fn()}
      />,
    );

    fireEvent.contextMenu(await screen.findByAltText("moon.png"));
    fireEvent.click(screen.getByRole("menuitem", { name: "复制图片" }));

    await waitFor(() => expect(write).toHaveBeenCalledOnce());
    expect(write.mock.calls[0][0][0]).toBeInstanceOf(MockClipboardItem);
    first.unmount();

    Object.defineProperty(navigator, "clipboard", {
      configurable: true,
      value: { writeText },
    });
    render(
      <MessageContent
        blocks={imageBlock}
        onOpenPreview={vi.fn()}
        onOpenLightbox={vi.fn()}
      />,
    );

    fireEvent.contextMenu(await screen.findByAltText("moon.png"));
    expect(screen.getByRole("menuitem", { name: "复制图片" })).toBeDisabled();
    fireEvent.click(screen.getByRole("menuitem", { name: "复制全路径" }));

    await waitFor(() =>
      expect(writeText).toHaveBeenCalledWith("/abs/path/moon.png"),
    );
  });

  it.each([
    ["invoke 失败", () => Promise.reject(new Error("read failed"))],
    ["缺少 imageBase64", () => Promise.resolve({ kind: "image" })],
  ])("工具图片%s → 回退成可点击文字 chip", async (_label, load) => {
    vi.mocked(invoke).mockReturnValueOnce(load() as ReturnType<typeof invoke>);
    const onOpenPreview = vi.fn();
    const onOpenLightbox = vi.fn();
    render(
      <MessageContent
        blocks={[
          {
            type: "tool",
            id: "t-image-fallback",
            tool: "Bash",
            summary: "render image artifact",
            card: "command",
            status: "ok",
            exit_code: 0,
            output: "saved to /abs/path/fallback.png",
          },
        ]}
        onOpenPreview={onOpenPreview}
        onOpenLightbox={onOpenLightbox}
      />,
    );

    const chip = await screen.findByRole("button", {
      name: "/abs/path/fallback.png",
    });
    expect(screen.queryByAltText("fallback.png")).toBeNull();
    expect(chip.parentElement).toHaveStyle({ alignItems: "flex-start" });

    fireEvent.click(chip);
    expect(onOpenPreview).toHaveBeenCalledWith("/abs/path/fallback.png");
    expect(onOpenLightbox).not.toHaveBeenCalled();
  });

  it("tool 卡没有图片路径 → 不渲染图片 chip", () => {
    render(
      <MessageContent
        blocks={[
          {
            type: "tool",
            id: "t-no-image",
            tool: "Bash",
            summary: "npm test",
            card: "command",
            status: "ok",
            exit_code: 0,
            output: "all tests passed",
          },
        ]}
        onOpenPreview={vi.fn()}
        onOpenLightbox={vi.fn()}
      />,
    );

    expect(screen.queryByRole("button", { name: /预览图片/ })).toBeNull();
  });

  it("cp 命令中的绝对与相对图片路径 → 捕获完整 token，不生成伪绝对路径", async () => {
    vi.mocked(invoke).mockResolvedValue({
      kind: "image",
      imageBase64: "aW1hZ2U=",
      mediaType: "image/png",
    });
    render(
      <MessageContent
        blocks={[
          {
            type: "tool",
            id: "t-cp-image-paths",
            tool: "Bash",
            summary: "copy generated image",
            card: "command",
            status: "ok",
            exit_code: 0,
            output:
              "cp /Users/dev/.codex/tmp/call_X.png output/imagegen/kitten-and-puppy.png",
          },
        ]}
        onOpenPreview={vi.fn()}
        onOpenLightbox={vi.fn()}
      />,
    );

    await waitFor(() => expect(invoke).toHaveBeenCalledTimes(2));
    const paths = vi
      .mocked(invoke)
      .mock.calls.map(([, args]) => (args as { path: string }).path);
    expect(paths).toEqual([
      "/Users/dev/.codex/tmp/call_X.png",
      "output/imagegen/kitten-and-puppy.png",
    ]);
    expect(paths).not.toContain("/imagegen/kitten-and-puppy.png");
  });

  it("同组绝对与相对路径内容相同 → 只保留相对路径缩略图", async () => {
    const onOpenLightbox = vi.fn();
    vi.mocked(invoke).mockResolvedValue({
      kind: "image",
      imageBase64: "c2FtZS1pbWFnZQ==",
      mediaType: "image/png",
    });
    render(
      <MessageContent
        blocks={[
          {
            type: "tool",
            id: "t-content-deduplicate-relative",
            tool: "Bash",
            summary: "copy generated image",
            card: "command",
            status: "ok",
            exit_code: 0,
            output:
              "cp /Users/dev/.codex/tmp/call_X.png output/imagegen/deep-space.png",
          },
        ]}
        onOpenPreview={vi.fn()}
        onOpenLightbox={onOpenLightbox}
      />,
    );

    await waitFor(() => expect(screen.getAllByRole("img")).toHaveLength(1));
    expect(screen.getByAltText("deep-space.png")).toBeInTheDocument();
    expect(screen.queryByAltText("call_X.png")).toBeNull();

    fireEvent.click(screen.getByAltText("deep-space.png"));
    expect(onOpenLightbox).toHaveBeenCalledWith(
      "output/imagegen/deep-space.png",
    );
  });

  it.each([
    ["相对路径", "output/first.png", "output/second.png"],
    ["绝对路径", "/tmp/first.png", "/tmp/second.png"],
  ])("同组同为%s且内容相同 → 保留靠后路径", async (_label, first, second) => {
    vi.mocked(invoke).mockResolvedValue({
      kind: "image",
      imageBase64: "c2FtZS1pbWFnZQ==",
      mediaType: "image/png",
    });
    render(
      <MessageContent
        blocks={[
          {
            type: "tool",
            id: `t-content-deduplicate-later-${_label}`,
            tool: "Bash",
            summary: "copy generated image",
            card: "command",
            status: "ok",
            exit_code: 0,
            output: `${first} ${second}`,
          },
        ]}
        onOpenPreview={vi.fn()}
        onOpenLightbox={vi.fn()}
      />,
    );

    await waitFor(() => expect(screen.getAllByRole("img")).toHaveLength(1));
    expect(screen.getByAltText("second.png")).toBeInTheDocument();
    expect(screen.queryByAltText("first.png")).toBeNull();
  });

  it("同组路径内容不同 → 两张缩略图都保留", async () => {
    vi.mocked(invoke).mockImplementation((_command, args) => {
      const path = (args as { path: string }).path;
      return Promise.resolve({
        kind: "image",
        imageBase64: path.includes("moon") ? "bW9vbg==" : "c3Rhcg==",
        mediaType: "image/png",
      });
    });
    render(
      <MessageContent
        blocks={[
          {
            type: "tool",
            id: "t-content-deduplicate-distinct",
            tool: "Bash",
            summary: "generate distinct images",
            card: "command",
            status: "ok",
            exit_code: 0,
            output: "/tmp/moon.png output/imagegen/star.png",
          },
        ]}
        onOpenPreview={vi.fn()}
        onOpenLightbox={vi.fn()}
      />,
    );

    expect(await screen.findAllByRole("img")).toHaveLength(2);
    expect(screen.getByAltText("moon.png")).toBeInTheDocument();
    expect(screen.getByAltText("star.png")).toBeInTheDocument();
  });

  it("同组一张加载成功一张失败 → 成功缩略图与失败降级 chip 都保留", async () => {
    vi.mocked(invoke).mockImplementation((_command, args) => {
      const path = (args as { path: string }).path;
      if (path.includes("missing")) return Promise.reject(new Error("missing"));
      return Promise.resolve({
        kind: "image",
        imageBase64: "bW9vbg==",
        mediaType: "image/png",
      });
    });
    render(
      <MessageContent
        blocks={[
          {
            type: "tool",
            id: "t-content-deduplicate-failure",
            tool: "Bash",
            summary: "load image artifacts",
            card: "command",
            status: "ok",
            exit_code: 0,
            output: "/tmp/moon.png output/imagegen/missing.png",
          },
        ]}
        onOpenPreview={vi.fn()}
        onOpenLightbox={vi.fn()}
      />,
    );

    expect(await screen.findByAltText("moon.png")).toBeInTheDocument();
    expect(
      await screen.findByRole("button", {
        name: "output/imagegen/missing.png",
      }),
    ).toBeInTheDocument();
  });

  it("同组三路径两同一异 → 保留相对路径同图赢家与异图", async () => {
    vi.mocked(invoke).mockImplementation((_command, args) => {
      const path = (args as { path: string }).path;
      return Promise.resolve({
        kind: "image",
        imageBase64: path.includes("nebula") ? "bmVidWxh" : "ZGVlcC1zcGFjZQ==",
        mediaType: "image/png",
      });
    });
    render(
      <MessageContent
        blocks={[
          {
            type: "tool",
            id: "t-content-deduplicate-three",
            tool: "Bash",
            summary: "copy and generate images",
            card: "command",
            status: "ok",
            exit_code: 0,
            output:
              "/tmp/deep-space-temp.png output/imagegen/nebula.png output/imagegen/deep-space.png",
          },
        ]}
        onOpenPreview={vi.fn()}
        onOpenLightbox={vi.fn()}
      />,
    );

    await waitFor(() => expect(screen.getAllByRole("img")).toHaveLength(2));
    expect(screen.getByAltText("deep-space.png")).toBeInTheDocument();
    expect(screen.getByAltText("nebula.png")).toBeInTheDocument();
    expect(screen.queryByAltText("deep-space-temp.png")).toBeNull();
  });

  it.each([
    ["URL", "https://example.com/a/b/pic.png"],
    ["协议相对 URL", "//cdn.example.com/a/b/pic.png"],
    ["单斜杠 scheme URL", "file:/tmp/pic.png"],
    ["grep 行号", "docs/img/diagram.png:12: some match"],
    ["裸文件名", "photo.png"],
  ])("%s 中的图片字样 → 不捕获为本地图片路径", async (_label, output) => {
    render(
      <MessageContent
        blocks={[
          {
            type: "tool",
            id: `t-reject-${_label}`,
            tool: "Bash",
            summary: "inspect output",
            card: "command",
            status: "ok",
            exit_code: 0,
            output,
          },
        ]}
        onOpenPreview={vi.fn()}
        onOpenLightbox={vi.fn()}
      />,
    );

    await waitFor(() => expect(invoke).not.toHaveBeenCalled());
  });

  it.each([
    ["点前缀相对路径", "./assets/logo.svg", "./assets/logo.svg"],
    ["中文句号收尾", "已保存到 output/pics/cat.png。", "output/pics/cat.png"],
    ["ASCII 括号包裹", "(output/pics/cat.png)", "output/pics/cat.png"],
    ["参数粘连绝对路径", "--out=/tmp/a.png", "/tmp/a.png"],
    ["Windows 正斜杠绝对路径", "C:/tmp/logo.png", "C:/tmp/logo.png"],
    ["Windows 反斜杠绝对路径", "C:\\tmp\\logo.png", "C:\\tmp\\logo.png"],
  ])("%s → 捕获清理后的完整路径", async (_label, output, expectedPath) => {
    vi.mocked(invoke).mockResolvedValue({
      kind: "image",
      imageBase64: "aW1hZ2U=",
      mediaType: "image/png",
    });
    render(
      <MessageContent
        blocks={[
          {
            type: "tool",
            id: `t-accept-${_label}`,
            tool: "Bash",
            summary: "generate image",
            card: "command",
            status: "ok",
            exit_code: 0,
            output,
          },
        ]}
        onOpenPreview={vi.fn()}
        onOpenLightbox={vi.fn()}
      />,
    );

    await waitFor(() =>
      expect(invoke).toHaveBeenCalledWith("read_attachment", {
        path: expectedPath,
        sessionId: null,
      }),
    );
    expect(invoke).toHaveBeenCalledTimes(1);
  });

  it("单个工具块含 9 个合格图片路径时只读取前 8 个", async () => {
    vi.mocked(invoke).mockResolvedValue({
      kind: "image",
      imageBase64: "aW1hZ2U=",
      mediaType: "image/png",
    });
    const paths = Array.from(
      { length: 9 },
      (_, index) => `/tmp/generated-${index + 1}.png`,
    );
    render(
      <MessageContent
        blocks={[
          {
            type: "tool",
            id: "t-image-path-limit",
            tool: "Bash",
            summary: "generate image batch",
            card: "command",
            status: "ok",
            exit_code: 0,
            output: paths.join("\n"),
          },
        ]}
        onOpenPreview={vi.fn()}
        onOpenLightbox={vi.fn()}
      />,
    );

    await waitFor(() => expect(invoke).toHaveBeenCalledTimes(8));
    const readPaths = vi
      .mocked(invoke)
      .mock.calls.map(([, args]) => (args as { path: string }).path);
    expect(readPaths).toEqual(paths.slice(0, 8));
    expect(readPaths).not.toContain(paths[8]);
  });

  it("绝对路径以相对路径结尾 → 丢短留长，只渲染绝对路径", async () => {
    vi.mocked(invoke).mockResolvedValue({
      kind: "image",
      imageBase64: "aW1hZ2U=",
      mediaType: "image/png",
    });
    render(
      <MessageContent
        blocks={[
          {
            type: "tool",
            id: "t-absolute-relative-duplicate",
            tool: "Bash",
            summary: "generate image",
            card: "command",
            status: "ok",
            exit_code: 0,
            output: "/abs/root/output/x.png output/x.png",
          },
        ]}
        onOpenPreview={vi.fn()}
        onOpenLightbox={vi.fn()}
      />,
    );

    await waitFor(() => expect(invoke).toHaveBeenCalledTimes(1));
    expect(invoke).toHaveBeenCalledWith("read_attachment", {
      path: "/abs/root/output/x.png",
      sessionId: null,
    });
  });

  it.each([
    ["相对在前", "output/x.png", "/abs/root/output/x.png"],
    ["绝对在前", "/abs/root/output/x.png", "output/x.png"],
  ])(
    "跨 item 去重（%s）→ 与顺序无关地丢短留长",
    async (_label, first, second) => {
      vi.mocked(invoke).mockResolvedValue({
        kind: "image",
        imageBase64: "aW1hZ2U=",
        mediaType: "image/png",
      });
      render(
        <MessageContent
          blocks={[
            {
              type: "tool",
              id: "t-cross-item-first",
              tool: "Bash",
              summary: "generate first reference",
              card: "command",
              status: "ok",
              exit_code: 0,
              output: first,
            },
            { type: "text", text: "分隔两个工具块" },
            {
              type: "tool",
              id: "t-cross-item-second",
              tool: "Bash",
              summary: "generate second reference",
              card: "command",
              status: "ok",
              exit_code: 0,
              output: second,
            },
          ]}
          onOpenPreview={vi.fn()}
          onOpenLightbox={vi.fn()}
        />,
      );

      await waitFor(() => expect(invoke).toHaveBeenCalledTimes(1));
      expect(invoke).toHaveBeenCalledWith("read_attachment", {
        path: "/abs/root/output/x.png",
        sessionId: null,
      });
    },
  );

  it.each([
    "Write",
    "write",
    "Edit",
    "edit",
    "fs_write",
    "fs_edit",
    "apply_patch",
    "file",
  ])("%s 工具块含图片路径 → 不扫描图片附件", async (tool) => {
    render(
      <MessageContent
        blocks={[
          {
            type: "tool",
            id: `t-content-tool-${tool}`,
            tool,
            summary: "update image reference",
            card: "command",
            status: "ok",
            exit_code: 0,
            output: "wrote /abs/path/ignored.png",
          },
        ]}
        onOpenPreview={vi.fn()}
        onOpenLightbox={vi.fn()}
      />,
    );

    await waitFor(() => expect(invoke).not.toHaveBeenCalled());
  });

  it("tool 卡多图去重且缩略图列表保持换行布局", async () => {
    vi.mocked(invoke)
      .mockResolvedValueOnce({
        kind: "image",
        imageBase64: "bW9vbg==",
        mediaType: "image/png",
      })
      .mockResolvedValueOnce({
        kind: "image",
        imageBase64: "c3Rhcg==",
        mediaType: "image/png",
      });
    render(
      <MessageContent
        blocks={[
          {
            type: "tool",
            id: "t-deduplicate-image",
            tool: "Bash",
            summary: "created /abs/path/moon.png",
            card: "command",
            status: "ok",
            exit_code: 0,
            output:
              "preview /abs/path/moon.png and /abs/path/star.png then /abs/path/moon.png",
          },
        ]}
        onOpenPreview={vi.fn()}
        onOpenLightbox={vi.fn()}
      />,
    );

    const images = await screen.findAllByRole("img");
    expect(images).toHaveLength(2);
    expect(screen.getAllByAltText("moon.png")).toHaveLength(1);
    expect(images[0].closest("button")?.parentElement).toHaveStyle({
      display: "flex",
      flexWrap: "wrap",
      alignItems: "flex-start",
    });
  });

  it("同一折叠组多个 tool 引用同一路径 → 组下只渲染一个缩略图", async () => {
    vi.mocked(invoke).mockResolvedValueOnce({
      kind: "image",
      imageBase64: "bW9vbg==",
      mediaType: "image/png",
    });
    render(
      <MessageContent
        blocks={[
          {
            type: "tool",
            id: "t-first-shared-image",
            tool: "Bash",
            summary: "generate shared image",
            card: "command",
            status: "ok",
            exit_code: 0,
            output: "created /abs/path/moon.png",
          },
          {
            type: "tool",
            id: "t-second-shared-image",
            tool: "Bash",
            summary: "confirm shared image",
            card: "command",
            status: "ok",
            exit_code: 0,
            output: "confirmed /abs/path/moon.png",
          },
        ]}
        onOpenPreview={vi.fn()}
        onOpenLightbox={vi.fn()}
      />,
    );

    const previews = await screen.findAllByRole("button", {
      name: /moon\.png/,
    });
    const fold = screen.getByText("执行了 2 步").closest(".toolfold");

    expect(previews).toHaveLength(1);
    expect(fold?.nextElementSibling).toContainElement(previews[0]);
  });

  it("同一折叠组后续 tool 引用新路径 → 两张缩略图都在组下渲染", async () => {
    vi.mocked(invoke)
      .mockResolvedValueOnce({
        kind: "image",
        imageBase64: "bW9vbg==",
        mediaType: "image/png",
      })
      .mockResolvedValueOnce({
        kind: "image",
        imageBase64: "c3Rhcg==",
        mediaType: "image/png",
      });
    render(
      <MessageContent
        blocks={[
          {
            type: "tool",
            id: "t-first-image",
            tool: "Bash",
            summary: "generate first image",
            card: "command",
            status: "ok",
            exit_code: 0,
            output: "created /abs/path/moon.png",
          },
          {
            type: "tool",
            id: "t-second-new-image",
            tool: "Bash",
            summary: "copy and create second image",
            card: "command",
            status: "ok",
            exit_code: 0,
            output: "copied /abs/path/moon.png to /abs/path/star.png",
          },
        ]}
        onOpenPreview={vi.fn()}
        onOpenLightbox={vi.fn()}
      />,
    );

    const moonPreview = await screen.findByRole("button", {
      name: /moon\.png/,
    });
    const starPreview = await screen.findByRole("button", {
      name: /star\.png/,
    });
    const fold = screen.getByText("执行了 2 步").closest(".toolfold");

    expect(screen.getAllByRole("button", { name: /moon\.png/ })).toHaveLength(
      1,
    );
    expect(fold?.nextElementSibling).toContainElement(moonPreview);
    expect(fold?.nextElementSibling).toContainElement(starPreview);
  });

  it("未提供 onOpenPreview → 不渲染图片 chip", () => {
    render(
      <MessageContent
        blocks={[
          {
            type: "tool",
            id: "t-without-preview",
            tool: "Bash",
            summary: "render image artifact",
            card: "command",
            status: "ok",
            exit_code: 0,
            output: "saved to /abs/path/moon.png",
          },
        ]}
      />,
    );

    expect(screen.queryByRole("button", { name: /moon\.png/ })).toBeNull();
  });

  it("image block 经 read_attachment 加载后渲染图片", async () => {
    vi.mocked(invoke).mockResolvedValueOnce({
      kind: "image",
      imageBase64: "aW1hZ2U=",
      mediaType: "image/png",
    });

    const onOpenLightbox = vi.fn();
    render(
      <MessageContent
        blocks={[
          {
            attachment_id: "/abs/path/moon.png",
            media_type: "image/png",
            type: "image",
          },
        ]}
        sessionId="session-1"
        onOpenLightbox={onOpenLightbox}
      />,
    );

    const image = await screen.findByRole("img");
    expect(image).toHaveAttribute("src", "data:image/png;base64,aW1hZ2U=");
    expect(image).toHaveStyle({
      maxHeight: "240px",
      maxWidth: "100%",
      objectFit: "contain",
      height: "auto",
    });
    expect(invoke).toHaveBeenCalledWith("read_attachment", {
      path: "/abs/path/moon.png",
      sessionId: "session-1",
    });

    fireEvent.click(image);
    expect(onOpenLightbox).toHaveBeenCalledWith("/abs/path/moon.png");
  });

  it("image block remount 后立即复用缓存且不重复读取", async () => {
    vi.mocked(invoke).mockResolvedValueOnce({
      kind: "image",
      imageBase64: "Y2FjaGVk",
      mediaType: "image/png",
    });
    const blocks: Block[] = [
      {
        attachment_id: "/abs/path/cached.png",
        media_type: "image/png",
        type: "image",
      },
    ];

    const first = render(
      <MessageContent blocks={blocks} sessionId="session-cache" />,
    );
    await screen.findByRole("img");
    first.unmount();

    render(<MessageContent blocks={blocks} sessionId="session-cache" />);

    expect(screen.getByRole("img")).toHaveAttribute(
      "src",
      "data:image/png;base64,Y2FjaGVk",
    );
    expect(invoke).toHaveBeenCalledTimes(1);
  });

  it("image block 加载失败 → 回退为可点击路径", async () => {
    vi.mocked(invoke).mockRejectedValueOnce(new Error("read failed"));
    const onOpenPreview = vi.fn();

    render(
      <MessageContent
        blocks={[
          {
            attachment_id: "/abs/path/moon.png",
            media_type: "image/png",
            type: "image",
          },
        ]}
        onOpenPreview={onOpenPreview}
      />,
    );

    await waitFor(() =>
      expect(
        screen.getByRole("button", { name: "/abs/path/moon.png" }),
      ).toBeInTheDocument(),
    );
    fireEvent.click(screen.getByRole("button", { name: "/abs/path/moon.png" }));

    expect(onOpenPreview).toHaveBeenCalledWith("/abs/path/moon.png");
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

describe("MessageContent 搜索类工具输出不误渲图片卡", () => {
  it("Grep 命中一堆 .png 路径 → 不渲染图片附件卡", () => {
    render(
      <MessageContent
        blocks={[
          {
            type: "tool",
            id: "t-grep-png",
            tool: "Grep",
            summary: "rg -l .png$",
            card: "command",
            status: "ok",
            exit_code: 0,
            output: "/abs/path/moon.png\n/abs/path/sun.png\n/abs/path/icon.svg",
          },
        ]}
        onOpenPreview={vi.fn()}
        onOpenLightbox={vi.fn()}
      />,
    );

    expect(screen.queryByAltText("moon.png")).toBeNull();
    expect(screen.queryByAltText("sun.png")).toBeNull();
    expect(screen.queryByAltText("icon.svg")).toBeNull();
    expect(invoke).not.toHaveBeenCalledWith(
      "read_attachment",
      expect.anything(),
    );
  });

  it("Glob 命中一堆 .svg 路径 → 不渲染图片附件卡", () => {
    render(
      <MessageContent
        blocks={[
          {
            type: "tool",
            id: "t-glob-svg",
            tool: "Glob",
            summary: "**/*.svg",
            card: "command",
            status: "ok",
            exit_code: 0,
            output: "/abs/path/logo.svg\n/abs/path/badge.svg",
          },
        ]}
        onOpenPreview={vi.fn()}
        onOpenLightbox={vi.fn()}
      />,
    );

    expect(screen.queryByAltText("logo.svg")).toBeNull();
    expect(screen.queryByAltText("badge.svg")).toBeNull();
    expect(invoke).not.toHaveBeenCalledWith(
      "read_attachment",
      expect.anything(),
    );
  });

  it("连续成功 Grep/Glob 折叠成组，output 含图片路径 → 折叠组下方不出图片卡", () => {
    const searchTool = (
      id: string,
      tool: "Grep" | "Glob",
      output: string,
    ): Block => ({
      type: "tool",
      id,
      tool,
      summary: `${tool} search`,
      card: "command",
      status: "ok",
      exit_code: 0,
      output,
    });

    render(
      <MessageContent
        blocks={[
          searchTool("s1", "Grep", "/abs/path/a.png"),
          searchTool("s2", "Glob", "/abs/path/b.png"),
          searchTool("s3", "Grep", "/abs/path/c.svg"),
        ]}
        onOpenPreview={vi.fn()}
        onOpenLightbox={vi.fn()}
      />,
    );

    expect(screen.getByText("执行了 3 步")).toBeInTheDocument();
    expect(screen.queryByAltText("a.png")).toBeNull();
    expect(screen.queryByAltText("b.png")).toBeNull();
    expect(screen.queryByAltText("c.svg")).toBeNull();
    expect(invoke).not.toHaveBeenCalledWith(
      "read_attachment",
      expect.anything(),
    );
  });
});

describe("MessageContent read 类与 verifier 工具输出不误渲图片卡", () => {
  it("Read 工具读到含 svg import 的文件内容 → 不渲染图片附件卡", () => {
    render(
      <MessageContent
        blocks={[
          {
            type: "tool",
            id: "t-read-svg-import",
            tool: "Read",
            summary: "AboutDialog.tsx",
            card: "command",
            status: "ok",
            exit_code: 0,
            output: 'import agentloomIcon from "../assets/agentloom-icon.svg";',
          },
        ]}
        onOpenPreview={vi.fn()}
        onOpenLightbox={vi.fn()}
      />,
    );

    expect(screen.queryByAltText("agentloom-icon.svg")).toBeNull();
    expect(invoke).not.toHaveBeenCalledWith(
      "read_attachment",
      expect.anything(),
    );
  });

  it("fs_read（myagent 名）读到含 svg import 的文件内容 → 不渲染图片附件卡", () => {
    render(
      <MessageContent
        blocks={[
          {
            type: "tool",
            id: "t-fs-read-svg-import",
            tool: "fs_read",
            summary: "AboutDialog.tsx",
            card: "command",
            status: "ok",
            exit_code: 0,
            output: 'import agentloomIcon from "../assets/agentloom-icon.svg";',
          },
        ]}
        onOpenPreview={vi.fn()}
        onOpenLightbox={vi.fn()}
      />,
    );

    expect(screen.queryByAltText("agentloom-icon.svg")).toBeNull();
    expect(invoke).not.toHaveBeenCalledWith(
      "read_attachment",
      expect.anything(),
    );
  });

  it("verifier 工具的测试日志含 .png 路径 → 不渲染图片附件卡", () => {
    render(
      <MessageContent
        blocks={[
          {
            type: "tool",
            id: "t-verifier-log",
            tool: "verifier",
            summary: "npm test",
            card: "command",
            status: "ok",
            exit_code: 0,
            output:
              "FAIL src/components/Snapshot.test.tsx\n  screenshot saved to /tmp/diff/mismatch.png",
          },
        ]}
        onOpenPreview={vi.fn()}
        onOpenLightbox={vi.fn()}
      />,
    );

    expect(screen.queryByAltText("mismatch.png")).toBeNull();
    expect(invoke).not.toHaveBeenCalledWith(
      "read_attachment",
      expect.anything(),
    );
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

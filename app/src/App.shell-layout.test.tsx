import { render, screen, fireEvent, waitFor } from "@testing-library/react";
import type { ComponentProps } from "react";
// @ts-expect-error - Vitest runs in Node, but this frontend tsconfig has no Node type declarations.
import { existsSync, readFileSync } from "fs";
import { describe, it, expect, vi } from "vitest";
import { makeSession } from "./test/factories";
import App from "./App";
import { setupAppTests } from "./__tests__/helpers/appTestSetup";

const { invokeMock, listenMock, openMock, sessionMainProps } = vi.hoisted(
  () => ({
    invokeMock: vi.fn(),
    listenMock: vi.fn(),
    openMock: vi.fn(),
    sessionMainProps: [] as Array<{
      onOpenPreview?: (path: string) => void;
      busy?: boolean;
      messages?: Array<{
        role: "user" | "assistant";
        content: unknown[];
        engine?: string;
        agent_id?: string | null;
        agent_name_snapshot?: string | null;
        // V3a：活尾唯一不变量断言需要读这个字段。
        stream_live?: boolean;
      }>;
    }>,
  }),
);

// VITEST_DEFER_INVOKE=1 makes every invoke settle one macrotask later, which
// deterministically exposes assertions that read state landing from a *different*
// async source than the one they awaited. CI runners are ~12x slower than a dev
// machine and lose those races for real; this switch reproduces it on purpose.
vi.mock("@tauri-apps/api/core", () => ({
  invoke: (...args: unknown[]) =>
    process.env.VITEST_DEFER_INVOKE
      ? new Promise((r) => setTimeout(r, 0)).then(() =>
          (invokeMock as (...a: unknown[]) => unknown)(...args),
        )
      : (invokeMock as (...a: unknown[]) => unknown)(...args),
}));
vi.mock("@tauri-apps/api/event", () => ({ listen: listenMock }));
vi.mock("@tauri-apps/plugin-dialog", () => ({ open: openMock }));
vi.mock("./components/SessionMain", async (importOriginal) => {
  const actual =
    await importOriginal<typeof import("./components/SessionMain")>();
  const OriginalSessionMain = actual.SessionMain;
  return {
    ...actual,
    SessionMain: (props: ComponentProps<typeof OriginalSessionMain>) => {
      sessionMainProps.push(props);
      return <OriginalSessionMain {...props} />;
    },
  };
});

declare const process: { env: Record<string, string | undefined> };

describe("App", () => {
  const { agentProfiles, mockBasicApp } = setupAppTests({
    invokeMock,
    listenMock,
    openMock,
    sessionMainProps,
  });

  it("阶段1 · 左栏 .sidebar 宽 230 + .surface 左角圆", () => {
    const css = readFileSync("src/styles/global.css", "utf-8");
    expect(css).toMatch(/\.sidebar\s*\{[^}]*flex:\s*0 0 230px/);
    expect(css).toMatch(/\.surface\s*\{[^}]*border-radius:\s*15px 0 0 15px/);
    expect(css).toMatch(
      /\.composer__input::placeholder\s*\{[^}]*color:\s*var\(--ink-4\)/,
    );
  });

  it("阶段1 收尾 · 旧三栏接缝 CSS 已删（.app--rpexpand / .body 容器 / .app__main / .topbar 容器）", () => {
    const css = readFileSync("src/styles/global.css", "utf-8");
    expect(css).not.toMatch(/\.app--rpexpand/);
    expect(css).not.toMatch(/\.app__main\s*\{/);
    expect(css).not.toMatch(/^\.body\s*\{/m);
    expect(css).not.toMatch(/^\.topbar\s*\{/m);
  });

  it("阶段1 收尾 · tabs 行横滚、sf-tabs 允许右侧语言菜单溢出", () => {
    const css = readFileSync("src/styles/global.css", "utf-8");
    expect(css).toMatch(/\.sf-tabs\s*\{[^}]*overflow:\s*visible/);
    expect(css).toMatch(/\.rptabs__tabrow\s*\{[^}]*overflow-x:\s*auto/);
  });

  it("阶段1 收尾 · .sf-tabs.expanded 最大化吃满 header（§2.D 右最大盖 main）", () => {
    const css = readFileSync("src/styles/global.css", "utf-8");
    expect(css).toMatch(/\.sf-tabs\.expanded\s*\{[^}]*flex:\s*1/);
  });

  it("设置 sheet 放大到业界尺寸 + sheet-scope 控件放大 + 全局 .st-* 基线未动", () => {
    const css = readFileSync("src/styles/global.css", "utf-8");
    // 外框放大
    expect(css).toMatch(/\.settings-sheet\s*\{[^}]*max-width:\s*1080px/);
    expect(css).toMatch(/\.settings-sheet\s*\{[^}]*max-height:\s*760px/);
    expect(css).not.toMatch(/\.settings-sheet\s*\{[^}]*max-width:\s*880px/);
    // sheet-scope 控件放大
    expect(css).toMatch(
      /\.settings-sheet\s+\.st-nav\s*\{[^}]*flex:\s*0 0 210px/,
    );
    expect(css).toMatch(
      /\.settings-sheet\s+\.st-nav-item\s*\{[^}]*font-size:\s*13px/,
    );
    expect(css).toMatch(
      /\.settings-sheet\s+\.st-content\s*\{[^}]*padding:\s*24px 30px/,
    );
    expect(css).toMatch(
      /\.settings-sheet\s+\.st-form\s*\{[^}]*max-width:\s*720px/,
    );
    // 全局基线未动（防 worker 误改全局而非 sheet-scope）
    expect(css).toMatch(/^\.st-nav\s*\{[^}]*flex:\s*0 0 200px/m);
    expect(css).toMatch(/^\.st-nav-item\s*\{[^}]*font-size:\s*12\.5px/m);
  });

  it("composer/content 阅读宽度布局回归（与 shell 翻转无关·勿随骨架迁移误删）", () => {
    const css = readFileSync("src/styles/global.css", "utf-8");
    expect(css).toMatch(/--content-max:\s*760px/);
    expect(css).toMatch(/--content-padding:\s*24px/);
    expect(css).toMatch(/\.turn\s*\{[^}]*max-width:\s*var\(--content-max\)/);
    expect(css).toMatch(
      /\.composer\s*\{[^}]*max-width:\s*calc\(var\(--content-max\) \+ var\(--content-padding\) \* 2\)/,
    );
    expect(css).toMatch(
      /\.composer__box:focus-within\s*\{[^}]*border-color:\s*var\(--accent\)/,
    );
  });

  it("方案 B 三栏配色：sidebar var(--bg) / surface var(--panel)", async () => {
    invokeMock.mockImplementation((cmd: string) => {
      if (cmd === "list_agents") return Promise.resolve(agentProfiles);
      if (cmd === "list_sessions")
        return Promise.resolve([
          makeSession({
            id: "s1",
            title: "新会话",
            repo_id: null,
            namespace_id: "local",
          }),
        ]);
      if (cmd === "get_messages") return Promise.resolve([]);
      if (cmd === "app_context")
        return Promise.resolve({
          namespaces: [
            {
              id: "local",
              kind: "local",
              name: "Local",
              is_builtin: 1,
              last_active_repo_id: "local-default",
              added_at: 0,
              last_used_at: null,
            },
          ],
          active_namespace_id: "local",
          active_repo_id: "local-default",
          repos: [
            {
              id: "r1",
              source: "local",
              owner: null,
              name: "ai-personal",
              path: "/tmp/ai-personal",
              status: "active",
              added_at: 100,
              last_used_at: 200,
              namespace_id: "local",
            },
          ],
        });
      return Promise.resolve();
    });
    const { container } = render(<App />);
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("get_messages", {
        sessionId: "s1",
      }),
    );

    const css = readFileSync("src/styles/global.css", "utf-8");
    expect(css).toMatch(/\.sidebar\s*\{[^}]*background:\s*var\(--bg\)/);
    expect(css).toMatch(/\.surface\s*\{[^}]*background:\s*var\(--panel\)/);
    expect(container.querySelector(".app-shell")).not.toBeNull();
    expect(container.querySelector(".sidebar")).not.toBeNull();
    expect(container.querySelector(".surface")).not.toBeNull();
    expect(container.querySelector(".sf-head")).not.toBeNull();
    expect(container.querySelector(".sf-body")).not.toBeNull();
    expect(container.querySelector(".topbar")).toBeNull();
  });

  describe("shell-redesign 阶段0 · 新顶层 surface CSS 基座", () => {
    const shellCss = readFileSync("src/styles/global.css", "utf-8");
    it("定义 .app-shell 横向 flex 容器", () => {
      expect(shellCss).toMatch(/\.app-shell\s*\{[^}]*display:\s*flex/);
    });
    it(".surface 填充 + 仅左侧圆角", () => {
      expect(shellCss).toMatch(
        /\.surface\s*\{[^}]*border-radius:\s*15px 0 0 15px/,
      );
    });
    it(".surface.full 收起全色接管（margin-left:0 + 无圆角）", () => {
      expect(shellCss).toMatch(/\.surface\.full\s*\{[^}]*margin-left:\s*0/);
      expect(shellCss).toMatch(/\.surface\.full\s*\{[^}]*border-radius:\s*0/);
    });
    it("--chrome-inset 默认归零，仅 macOS Overlay 为红绿灯让位 78px", () => {
      expect(shellCss).toMatch(/:root\s*\{[^}]*--chrome-inset:\s*0/);
      expect(shellCss).toMatch(
        /html\[data-os=["']macos["']\]\s*\{[^}]*--chrome-inset:\s*78px/,
      );
      expect(shellCss).toMatch(
        /\.sb-top\s*\{[^}]*padding:\s*0 8px 0 var\(--chrome-inset\)/,
      );

      const style = document.createElement("style");
      style.textContent = shellCss;
      document.head.appendChild(style);
      document.documentElement.dataset.os = "macos";
      expect(
        getComputedStyle(document.documentElement)
          .getPropertyValue("--chrome-inset")
          .trim(),
      ).toBe("78px");
      document.documentElement.dataset.os = "windows";
      expect(
        getComputedStyle(document.documentElement)
          .getPropertyValue("--chrome-inset")
          .trim(),
      ).toBe("0");
      style.remove();
      delete document.documentElement.dataset.os;
    });
    it(".session-pane.hidden 与 .tools-pane.full 组合态类存在", () => {
      expect(shellCss).toMatch(
        /\.session-pane\.hidden\s*\{[^}]*display:\s*none/,
      );
      expect(shellCss).toMatch(/\.tools-pane\.full\s*\{[^}]*flex:\s*1 1 auto/);
    });
    it("阶段1 收尾 · App 不再引用 TopBar + TopBar 文件已删", () => {
      const appSrc = readFileSync("src/App.tsx", "utf-8");
      expect(appSrc).not.toMatch(/components\/TopBar/);
      expect(existsSync("src/components/TopBar.tsx")).toBe(false);
      expect(existsSync("src/components/TopBar.test.tsx")).toBe(false);
    });
  });

  it("②a：右面板开 → .surface 挂 rpopen（消息列自适应）·关 → 无 rpopen", async () => {
    mockBasicApp();
    render(<App />);
    await screen.findByText("Claude Code");
    expect(document.querySelector(".surface.rpopen")).toBeNull();
    fireEvent.click(await screen.findByLabelText("展开右面板"));
    await waitFor(() =>
      expect(document.querySelector(".surface.rpopen")).not.toBeNull(),
    );
  });
});

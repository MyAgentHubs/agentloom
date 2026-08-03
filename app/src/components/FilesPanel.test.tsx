import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { describe, expect, it, vi, beforeEach } from "vitest";
import { invoke } from "@tauri-apps/api/core";
import { FilesPanel, highlightMatches } from "./FilesPanel";

// 渲染 highlightMatches 返回的 ReactNode[]，取出 <mark> 命中的文本内容做断言。
function renderedHighlights(content: string, query: string): string[] {
  const { container } = render(<div>{highlightMatches(content, query)}</div>);
  return Array.from(container.querySelectorAll("mark")).map(
    (el) => el.textContent ?? "",
  );
}

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("../lib/highlighter", () => ({
  CODE_THEME: "agentloom-warm-dark",
  getHighlighter: vi.fn(async () => ({
    codeToTokens: (code: string) => ({
      tokens: code.split("\n").map((line) => [
        {
          color: "#275f99",
          content: line,
        },
      ]),
    }),
    getLoadedLanguages: () => ["markdown", "typescript"],
  })),
  normalizeLang: (lang?: string) =>
    lang === "md" ? "markdown" : lang === "ts" ? "typescript" : "text",
}));

const entries = [
  { path: "README.md", name: "README.md", isDir: false, depth: 0, size: 28 },
  { path: "src", name: "src", isDir: true, depth: 0, size: null },
  { path: "src/main.ts", name: "main.ts", isDir: false, depth: 1, size: 21 },
];

const listing = { entries, truncated: false };

describe("FilesPanel", () => {
  const invokeMock = vi.mocked(invoke);

  beforeEach(() => {
    invokeMock.mockReset();
    // jsdom 未实现 scrollIntoView；当前命中定位需要它。
    Element.prototype.scrollIntoView = vi.fn();
  });

  it("lists project files and opens README as rendered markdown", async () => {
    invokeMock.mockImplementation((cmd: string, args?: any) => {
      if (cmd === "list_session_files") return Promise.resolve(listing);
      if (cmd === "read_session_file") {
        expect(args).toEqual({ sessionId: "s1", path: "README.md" });
        return Promise.resolve({
          path: "README.md",
          name: "README.md",
          content: "# Demo\n\nhello world",
          size: 19,
          language: "md",
          isMarkdown: true,
        });
      }
      throw new Error(cmd);
    });

    render(<FilesPanel sessionId="s1" repoName="demo" />);

    expect(
      await screen.findByRole("heading", { name: "Demo" }),
    ).toBeInTheDocument();
    expect(screen.getByText("demo")).toBeInTheDocument();
    expect(screen.getByLabelText("打开文件 README.md")).toBeInTheDocument();
    // Directories default to collapsed; src/main.ts is hidden until expanded.
    expect(screen.getByLabelText("展开目录 src")).toBeInTheDocument();
    expect(
      screen.queryByLabelText("打开文件 src/main.ts"),
    ).not.toBeInTheDocument();

    fireEvent.click(screen.getByLabelText("查看源码"));
    await waitFor(() => {
      expect(document.querySelector(".files-code .mm-code")).not.toBeNull();
      expect(document.querySelectorAll(".files-code .ln")).toHaveLength(3);
      expect(document.querySelector('[style*="color"]')).not.toBeNull();
    });
  });

  it("renders backend error envelopes as localized messages", async () => {
    invokeMock.mockRejectedValue("AL_ERR:wt.session.invalidId");

    render(<FilesPanel sessionId="s1" repoName="demo" />);

    expect(
      await screen.findByText("session_id 清洗后为空，无法建 worktree"),
    ).toBeInTheDocument();
    expect(screen.queryByText(/AL_ERR:/)).not.toBeInTheDocument();
  });

  it("filters tree and navigates find-in-file matches", async () => {
    invokeMock.mockImplementation((cmd: string, args?: any) => {
      if (cmd === "list_session_files") return Promise.resolve(listing);
      if (cmd === "read_session_file") {
        return Promise.resolve({
          path: args.path,
          name: args.path.split("/").pop(),
          content: "hello one\n\nhello two\n\nbye",
          size: 23,
          language: "md",
          isMarkdown: true,
        });
      }
      throw new Error(cmd);
    });

    render(<FilesPanel sessionId="s1" repoName="demo" />);

    await screen.findByText("hello one");

    fireEvent.change(screen.getByPlaceholderText("Filter files…"), {
      target: { value: "main" },
    });
    expect(
      screen.queryByLabelText("打开文件 README.md"),
    ).not.toBeInTheDocument();
    expect(screen.getByLabelText("打开文件 src/main.ts")).toBeInTheDocument();

    fireEvent.click(screen.getByLabelText("Find in file"));
    fireEvent.change(screen.getByPlaceholderText("Find in file…"), {
      target: { value: "hello" },
    });
    expect(screen.getByText("1 / 2")).toBeInTheDocument();

    // 命中高亮：正文出现与命中数相等的 <mark>，第一个带 active class。
    let marks = document.querySelectorAll(".files-src mark");
    expect(marks).toHaveLength(2);
    expect(marks[0]).toHaveClass("files-view__hit--active");
    expect(marks[1]).not.toHaveClass("files-view__hit--active");

    fireEvent.click(screen.getByLabelText("下一个命中"));
    expect(screen.getByText("2 / 2")).toBeInTheDocument();
    marks = document.querySelectorAll(".files-src mark");
    expect(marks).toHaveLength(2);
    expect(marks[1]).toHaveClass("files-view__hit--active");
    expect(marks[0]).not.toHaveClass("files-view__hit--active");

    fireEvent.click(screen.getByLabelText("上一个命中"));
    expect(screen.getByText("1 / 2")).toBeInTheDocument();
    marks = document.querySelectorAll(".files-src mark");
    expect(marks[0]).toHaveClass("files-view__hit--active");

    // 清空 query 后恢复原渲染（markdown），mark 消失。
    fireEvent.change(screen.getByPlaceholderText("Find in file…"), {
      target: { value: "" },
    });
    expect(document.querySelectorAll(".files-src mark")).toHaveLength(0);
    expect(await screen.findByText("hello one")).toBeInTheDocument();
  });

  it("folds directories and distinguishes file and folder rows", async () => {
    invokeMock.mockImplementation((cmd: string, args?: any) => {
      if (cmd === "list_session_files") return Promise.resolve(listing);
      if (cmd === "read_session_file") {
        return Promise.resolve({
          path: args.path,
          name: args.path.split("/").pop(),
          content: "# Demo",
          size: 6,
          language: "md",
          isMarkdown: true,
        });
      }
      throw new Error(cmd);
    });

    render(<FilesPanel sessionId="s1" repoName="demo" />);

    await screen.findByRole("heading", { name: "Demo" });
    // Directories default to collapsed.
    const directory = screen.getByLabelText("展开目录 src");
    expect(directory).toHaveAttribute("aria-expanded", "false");
    expect(directory.querySelector(".files-tree__kind--dir")).not.toBeNull();
    expect(screen.queryByLabelText("打开文件 src/main.ts")).toBeNull();

    fireEvent.click(directory);
    const file = screen.getByLabelText("打开文件 src/main.ts");
    expect(file.querySelector(".files-tree__kind--file")).not.toBeNull();

    const expanded = screen.getByLabelText("折叠目录 src");
    expect(expanded).toHaveAttribute("aria-expanded", "true");
    fireEvent.click(expanded);
    expect(screen.queryByLabelText("打开文件 src/main.ts")).toBeNull();
  });

  it("lists files from repo context when no session is open", async () => {
    invokeMock.mockImplementation((cmd: string, args?: any) => {
      if (cmd === "list_repo_files") {
        expect(args).toEqual({ repoId: "r1" });
        return Promise.resolve(listing);
      }
      if (cmd === "read_repo_file") {
        expect(args).toEqual({ repoId: "r1", path: "README.md" });
        return Promise.resolve({
          path: "README.md",
          name: "README.md",
          content: "# Repo Demo",
          size: 11,
          language: "md",
          isMarkdown: true,
        });
      }
      throw new Error(cmd);
    });

    render(<FilesPanel sessionId={null} repoId="r1" repoName="repo-demo" />);

    expect(
      await screen.findByRole("heading", { name: "Repo Demo" }),
    ).toBeInTheDocument();
    expect(screen.getByText("repo-demo")).toBeInTheDocument();
    expect(screen.getByLabelText("打开文件 README.md")).toBeInTheDocument();
  });

  it("shows a truncation hint when the backend reports truncated listings", async () => {
    invokeMock.mockImplementation((cmd: string, args?: any) => {
      if (cmd === "list_session_files")
        return Promise.resolve({ entries, truncated: true });
      if (cmd === "read_session_file") {
        return Promise.resolve({
          path: args.path,
          name: args.path.split("/").pop(),
          content: "# Demo",
          size: 6,
          language: "md",
          isMarkdown: true,
        });
      }
      throw new Error(cmd);
    });

    render(<FilesPanel sessionId="s1" repoName="demo" />);

    await screen.findByRole("heading", { name: "Demo" });
    expect(
      screen.getByText("项目条目较多，仅显示前 1000 项"),
    ).toBeInTheDocument();
  });

  it("does not show a truncation hint when the listing is not truncated", async () => {
    invokeMock.mockImplementation((cmd: string, args?: any) => {
      if (cmd === "list_session_files") return Promise.resolve(listing);
      if (cmd === "read_session_file") {
        return Promise.resolve({
          path: args.path,
          name: args.path.split("/").pop(),
          content: "# Demo",
          size: 6,
          language: "md",
          isMarkdown: true,
        });
      }
      throw new Error(cmd);
    });

    render(<FilesPanel sessionId="s1" repoName="demo" />);

    await screen.findByRole("heading", { name: "Demo" });
    expect(
      screen.queryByText("项目条目较多，仅显示前 1000 项"),
    ).not.toBeInTheDocument();
  });

  it("shows an empty session hint outside a session", () => {
    render(<FilesPanel sessionId={null} repoId={null} repoName={null} />);
    expect(screen.getByText("进入会话后浏览项目文件")).toBeInTheDocument();
  });
});

describe("highlightMatches", () => {
  it("does not drift when lowercasing changes code-unit length (İ regression lock)", () => {
    // 前提钉住：toLowerCase 会把 İ(U+0130) 变成 2 个码元，长度从 8 变 9。
    expect("İstanbul".toLowerCase().length).toBe(9);
    expect("İstanbul".length).toBe(8);

    const hits = renderedHighlights("İstanbul", "stan");
    expect(hits).toEqual(["stan"]);
  });

  it("matches case-insensitively while preserving original casing in the highlight", () => {
    const hits = renderedHighlights("Hello World", "hello");
    expect(hits).toEqual(["Hello"]);
  });

  it("highlights every occurrence", () => {
    const hits = renderedHighlights("aXaXa", "a");
    expect(hits).toEqual(["a", "a", "a"]);
  });

  it("handles CJK content unaffected", () => {
    const hits = renderedHighlights("你好世界你好", "你好");
    expect(hits).toEqual(["你好", "你好"]);
  });

  it("escapes regex special characters in the query", () => {
    const hits = renderedHighlights("a.c abc", "a.c");
    expect(hits).toEqual(["a.c"]);
  });

  it("returns no highlights for an empty query and terminates immediately", () => {
    const start = Date.now();
    const hits = renderedHighlights("hello world", "");
    expect(Date.now() - start).toBeLessThan(1000);
    expect(hits).toEqual([]);
  });

  it("returns no highlights when there is no match", () => {
    const hits = renderedHighlights("hello world", "xyz");
    expect(hits).toEqual([]);
  });
});

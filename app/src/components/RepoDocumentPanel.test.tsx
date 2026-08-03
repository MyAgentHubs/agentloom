import { fireEvent, render, screen } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { UseRepoDocumentResult } from "../hooks/useRepoDocument";
import { useRepoDocument } from "../hooks/useRepoDocument";
import { RepoDocumentPanel } from "./RepoDocumentPanel";

vi.mock("../hooks/useRepoDocument");
vi.mock("../lib/useMarkdown", () => ({ useMarkdown: () => null }));

const generate = vi.fn();
const mockedUseRepoDocument = vi.mocked(useRepoDocument);
function result(
  overrides: Partial<UseRepoDocumentResult> = {},
): UseRepoDocumentResult {
  return {
    doc: null,
    loading: false,
    generating: false,
    liveText: "",
    error: null,
    generate,
    ...overrides,
  };
}

describe("RepoDocumentPanel", () => {
  beforeEach(() => {
    generate.mockReset();
    mockedUseRepoDocument.mockReturnValue(result());
  });

  it("空态显示 CTA 和只读声明，点击后用 agentId 生成", () => {
    render(
      <RepoDocumentPanel repoId="repo-1" agentId="agent-1" kind="intro" />,
    );
    fireEvent.click(screen.getByRole("button", { name: "开始 AI 解析" }));
    expect(screen.getByText(/只读 · agent 只读取和搜索/)).toBeInTheDocument();
    expect(generate).toHaveBeenCalledWith("agent-1");
  });

  it("生成中显示进度标题和实时文本", () => {
    mockedUseRepoDocument.mockReturnValue(
      result({ generating: true, liveText: "部分文本" }),
    );
    render(
      <RepoDocumentPanel repoId="repo-1" agentId="agent-1" kind="intro" />,
    );
    expect(
      screen.getByRole("heading", { name: "正在解析项目" }),
    ).toBeInTheDocument();
    expect(screen.getByText("部分文本")).toBeInTheDocument();
  });

  it("完成态渲染正文和重新生成按钮，不显示过期条", () => {
    mockedUseRepoDocument.mockReturnValue(
      result({
        doc: {
          repo_id: "repo-1",
          content: "项目正文",
          generated_at: 100,
          head_sha: "1234567890",
          stale: false,
        },
      }),
    );
    render(
      <RepoDocumentPanel repoId="repo-1" agentId="agent-1" kind="intro" />,
    );
    expect(screen.getByText("项目正文")).toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: "重新生成" }),
    ).toBeInTheDocument();
    expect(screen.queryByText(/仓库已有新提交/)).not.toBeInTheDocument();
  });

  it("过期完成态显示短 sha 和重新生成", () => {
    mockedUseRepoDocument.mockReturnValue(
      result({
        doc: {
          repo_id: "repo-1",
          content: "旧正文",
          generated_at: 100,
          head_sha: "abcdef1234567890",
          stale: true,
        },
      }),
    );
    render(
      <RepoDocumentPanel repoId="repo-1" agentId="agent-1" kind="intro" />,
    );
    expect(
      screen.getByText(/基于 abcdef12 生成 · 仓库已有新提交/),
    ).toBeInTheDocument();
    expect(screen.getAllByRole("button", { name: "重新生成" })).toHaveLength(2);
  });

  it("错误态显示错误与重试按钮", () => {
    mockedUseRepoDocument.mockReturnValue(result({ error: "boom" }));
    render(
      <RepoDocumentPanel repoId="repo-1" agentId="agent-1" kind="intro" />,
    );
    expect(screen.getByRole("alert")).toHaveTextContent("生成失败boom");
    fireEvent.click(screen.getByRole("button", { name: "重试" }));
    expect(generate).toHaveBeenCalledWith("agent-1");
  });

  it("daily 使用不同的空态标题与 CTA", () => {
    const { rerender } = render(
      <RepoDocumentPanel repoId="repo-1" agentId="agent-1" kind="intro" />,
    );
    expect(screen.getByText("还没有解析过这个项目")).toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: "开始 AI 解析" }),
    ).toBeInTheDocument();
    rerender(
      <RepoDocumentPanel repoId="repo-1" agentId="agent-1" kind="daily" />,
    );
    expect(screen.getByText("还没有今天的日报")).toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: "生成今日日报" }),
    ).toBeInTheDocument();
  });
});

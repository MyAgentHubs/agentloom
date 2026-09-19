import { render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { UseRepoDocumentResult } from "../hooks/useRepoDocument";
import { useRepoDocument } from "../hooks/useRepoDocument";
import { RepoDocumentPanel } from "./RepoDocumentPanel";

// 与 RepoDocumentPanel.test.tsx 不同：这份文件不 mock useMarkdown，走真实的
// 异步加载的 MarkdownBody，才能验证「规则 B 默认关闭」在真实渲染路径上生效
// （RepoDocumentPanel.test.tsx 把 useMarkdown 整体 mock 成 null，只测得到
// `<pre>` 兜底分支，测不到 MarkdownBody 本身有没有被喂错的 prop）。
vi.mock("../hooks/useRepoDocument");

const invokeMock = vi.fn();
vi.mock("@tauri-apps/api/core", () => ({
  invoke: (...args: unknown[]) => invokeMock(...args),
}));

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
    generate: vi.fn(),
    ...overrides,
  };
}

beforeEach(() => {
  invokeMock.mockReset();
});

describe("RepoDocumentPanel（P1：非聊天场景默认关闭规则 B）", () => {
  it("仓库文档正文里的裸绝对路径不自动出图", async () => {
    mockedUseRepoDocument.mockReturnValue(
      result({
        doc: {
          repo_id: "repo-1",
          content: "详见 /Users/victim/secret.png 这张截图",
          generated_at: 100,
          head_sha: "1234567890",
          stale: false,
        },
      }),
    );

    render(
      <RepoDocumentPanel repoId="repo-1" agentId="agent-1" kind="intro" />,
    );

    await waitFor(() =>
      expect(
        screen.getByText(/详见 \/Users\/victim\/secret\.png 这张截图/),
      ).toBeInTheDocument(),
    );
    expect(invokeMock).not.toHaveBeenCalledWith(
      "read_attachment",
      expect.anything(),
    );
  });
});

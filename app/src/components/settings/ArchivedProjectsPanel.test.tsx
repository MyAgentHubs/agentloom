import {
  fireEvent,
  render,
  screen,
  waitFor,
  within,
} from "@testing-library/react";
import { invoke } from "@tauri-apps/api/core";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { RepoMeta } from "../../types/agent";
import { ArchivedProjectsPanel } from "./ArchivedProjectsPanel";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

function repo(overrides: Partial<RepoMeta> = {}): RepoMeta {
  return {
    id: "archived-1",
    source: "local",
    owner: null,
    name: "旧项目",
    path: "/tmp/old-project",
    status: "archived",
    added_at: 1,
    last_used_at: null,
    namespace_id: "local",
    icon: "🗄️",
    ...overrides,
  };
}

describe("ArchivedProjectsPanel", () => {
  const invokeMock = vi.mocked(invoke);

  beforeEach(() => {
    invokeMock.mockReset();
  });

  it("渲染已归档项目的图标与名称，并渲染空态", async () => {
    invokeMock.mockResolvedValueOnce([repo()]);
    const { unmount } = render(
      <ArchivedProjectsPanel onArchivedChanged={() => {}} />,
    );

    expect(await screen.findByText("🗄️")).toBeInTheDocument();
    expect(screen.getByText("旧项目")).toBeInTheDocument();
    unmount();

    invokeMock.mockResolvedValueOnce([]);
    render(<ArchivedProjectsPanel onArchivedChanged={() => {}} />);
    expect(await screen.findByText("没有已归档的项目")).toBeInTheDocument();
  });

  it("恢复项目后刷新列表并通知 App", async () => {
    const onArchivedChanged = vi.fn();
    invokeMock
      .mockResolvedValueOnce([repo()])
      .mockResolvedValueOnce(undefined)
      .mockResolvedValueOnce([]);
    render(<ArchivedProjectsPanel onArchivedChanged={onArchivedChanged} />);

    fireEvent.click(await screen.findByRole("button", { name: "恢复" }));

    await waitFor(() => {
      expect(invokeMock).toHaveBeenCalledWith("restore_repo", {
        id: "archived-1",
      });
      expect(onArchivedChanged).toHaveBeenCalledTimes(1);
    });
    expect(invokeMock).toHaveBeenLastCalledWith("list_repos_by_status", {
      status: "archived",
    });
  });

  it("彻底删除须先确认；取消不删除，确认后删除并通知 App", async () => {
    const onArchivedChanged = vi.fn();
    invokeMock
      .mockResolvedValueOnce([repo()])
      .mockResolvedValueOnce(undefined)
      .mockResolvedValueOnce([]);
    render(<ArchivedProjectsPanel onArchivedChanged={onArchivedChanged} />);

    const deleteButton = await screen.findByRole("button", {
      name: "彻底删除",
    });
    fireEvent.click(deleteButton);
    expect(screen.getByRole("dialog")).toBeInTheDocument();
    expect(screen.getByText(/彻底删除「旧项目」/)).toBeInTheDocument();
    expect(invokeMock).not.toHaveBeenCalledWith("delete_repo_forever", {
      id: "archived-1",
    });

    fireEvent.click(screen.getByRole("button", { name: "取消" }));
    expect(screen.queryByRole("dialog")).toBeNull();
    expect(invokeMock).not.toHaveBeenCalledWith("delete_repo_forever", {
      id: "archived-1",
    });

    fireEvent.click(deleteButton);
    const dialog = screen.getByRole("dialog");
    fireEvent.click(within(dialog).getByRole("button", { name: "彻底删除" }));

    await waitFor(() => {
      expect(invokeMock).toHaveBeenCalledWith("delete_repo_forever", {
        id: "archived-1",
      });
      expect(onArchivedChanged).toHaveBeenCalledTimes(1);
    });
  });
});

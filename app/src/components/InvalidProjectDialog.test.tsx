import { render, screen, fireEvent, waitFor } from "@testing-library/react";
import { describe, it, expect, vi, beforeEach } from "vitest";

const { invokeMock } = vi.hoisted(() => ({ invokeMock: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke: invokeMock }));

import { InvalidProjectDialog } from "./InvalidProjectDialog";

const base = {
  state: { repoId: "r1", kind: "invalid" as "invalid" | "archived" },
  onResolved: vi.fn(),
  onClose: vi.fn(),
};

describe("InvalidProjectDialog", () => {
  beforeEach(() => {
    invokeMock.mockReset();
    invokeMock.mockImplementation(() => Promise.resolve());
    base.onResolved.mockReset();
    base.onClose.mockReset();
  });

  it("invalid kind 显「路径已无效」标题 + 3 个按钮", () => {
    render(<InvalidProjectDialog {...base} />);
    expect(screen.getByText(/路径已无效/)).toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: /归档此项目/ }),
    ).toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: /跳到默认会话/ }),
    ).toBeInTheDocument();
  });

  it("archived kind 显「项目已归档」+ 恢复按钮", () => {
    render(
      <InvalidProjectDialog
        {...base}
        state={{ repoId: "r1", kind: "archived" }}
      />,
    );
    expect(
      screen.getByRole("heading", { name: "项目已归档" }),
    ).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /恢复/ })).toBeInTheDocument();
  });

  it("点归档调 archive_repo IPC + onResolved('archived')", async () => {
    render(<InvalidProjectDialog {...base} />);
    fireEvent.click(screen.getByRole("button", { name: /归档此项目/ }));
    await waitFor(() => {
      expect(invokeMock).toHaveBeenCalledWith("archive_repo", { id: "r1" });
      expect(base.onResolved).toHaveBeenCalledWith("archived");
    });
  });
});

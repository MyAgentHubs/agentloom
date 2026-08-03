import { describe, it, expect, vi } from "vitest";
import { render, screen, fireEvent } from "@testing-library/react";
import { RunCard } from "./RunCard";
import type { UndoResultRecord } from "../types/undo";

describe("RunCard", () => {
  const base = {
    type: "run_card" as const,
    run_id: "run-1",
    commit_sha: "deadbeef",
    files_changed: 3,
    insertions: 3,
    deletions: 1,
    interrupted: false,
  };

  it("显示文件数与增删行数", () => {
    render(<RunCard block={base} onView={() => {}} />);
    expect(screen.getByText(/本轮改了\s*3\s*文件/)).toBeInTheDocument();
    expect(screen.getByText("+3")).toBeInTheDocument();
    expect(screen.getByText("−1")).toBeInTheDocument();
  });

  it("增删行数全为 0 时只显示文件数", () => {
    const { container } = render(
      <RunCard
        block={{ ...base, files_changed: 2, insertions: 0, deletions: 0 }}
        onView={() => {}}
      />,
    );

    expect(screen.getByText(/本轮改了\s*2\s*文件/)).toBeInTheDocument();
    expect(
      container.querySelector(".runcard__stat--add"),
    ).not.toBeInTheDocument();
    expect(
      container.querySelector(".runcard__stat--del"),
    ).not.toBeInTheDocument();
  });

  it("中断轮显示中断标记", () => {
    render(
      <RunCard block={{ ...base, interrupted: true }} onView={() => {}} />,
    );
    expect(screen.getByText(/中断/)).toBeInTheDocument();
  });

  it("点查看触发 onView", () => {
    const onView = vi.fn();
    render(<RunCard block={base} onView={onView} />);
    fireEvent.click(screen.getByRole("button", { name: "查看" }));
    expect(onView).toHaveBeenCalledTimes(1);
  });

  it("已结束的 active 轮、且确实有可撤销记录时显示查看和撤销入口", () => {
    const onUndo = vi.fn();
    render(
      <RunCard
        block={{ ...base, state: "active", undo_total: 2 }}
        onView={() => {}}
        onUndo={onUndo}
      />,
    );

    expect(screen.getByText(/本轮改了\s*3\s*文件/)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "查看" })).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "撤销…" }));
    expect(onUndo).toHaveBeenCalledTimes(1);
  });

  it("run 未结束（没有 onUndo 能力）→ 根本不渲染撤销入口", () => {
    render(<RunCard block={base} onView={() => {}} />);
    expect(
      screen.queryByRole("button", { name: "撤销…" }),
    ).not.toBeInTheDocument();
  });

  it("已结束的 active 轮但 undo_total 为 0（没留下任何撤销记录）→ 不显示撤销入口，避免点进去是死胡同", () => {
    const onUndo = vi.fn();
    render(
      <RunCard
        block={{ ...base, state: "active", undo_total: 0 }}
        onView={() => {}}
        onUndo={onUndo}
      />,
    );

    expect(screen.getByRole("button", { name: "查看" })).toBeInTheDocument();
    expect(
      screen.queryByRole("button", { name: "撤销…" }),
    ).not.toBeInTheDocument();
  });

  it("undone 态灰显已撤销文案且无再次撤销按钮", () => {
    const { container } = render(
      <RunCard block={{ ...base, state: "undone" }} onView={() => {}} />,
    );

    expect(screen.getByText("已撤销本轮")).toBeInTheDocument();
    expect(container.firstElementChild).toHaveClass("runcard--undone");
    expect(
      screen.queryByRole("button", { name: "撤销…" }),
    ).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: "查看" })).toBeInTheDocument();
  });

  it("后端账本部分撤销 2 / 3 时不冒充全部已撤销", () => {
    const onUndo = vi.fn();
    render(
      <RunCard
        block={{
          ...base,
          state: "partially_undone",
          undo_total: 3,
          undo_undone: 2,
        }}
        onView={() => {}}
        onUndo={onUndo}
      />,
    );

    expect(screen.getByText("已撤销 2 / 3")).toBeInTheDocument();
    expect(screen.queryByText("已撤销本轮")).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "继续撤销…" }));
    expect(onUndo).toHaveBeenCalledTimes(1);
  });

  it("部分成功显示账本计数与本次临时结果摘要，不冒充全部已撤销", () => {
    const result: UndoResultRecord = {
      session_id: "s1",
      run_id: "run-1",
      report: {
        restored: ["a.ts"],
        skipped: [{ file_path: "b.ts", reason: "changed" }],
        failed: [{ file_path: "c.ts", reason: "permission denied" }],
      },
      selected_entries: [
        { file_path: "a.ts", change_kind: "modified" },
        { file_path: "b.ts", change_kind: "modified" },
        { file_path: "c.ts", change_kind: "deleted" },
      ],
      total_entries: 4,
    };
    const onUndo = vi.fn();
    const { container } = render(
      <RunCard
        block={{
          ...base,
          state: "partially_undone",
          undo_total: 4,
          undo_undone: 1,
          undo_result: result,
        }}
        onView={() => {}}
        onUndo={onUndo}
      />,
    );

    expect(screen.getByText("已撤销 1 / 4")).toBeInTheDocument();
    expect(screen.queryByText("已撤销本轮")).not.toBeInTheDocument();
    expect(
      screen.getByText(/本次已还原 1 个 · 本次未还原 1 个 · 本次失败 1 个/),
    ).toBeInTheDocument();
    expect(screen.getByText(/本次未还原 1 个、失败 1 个/)).toBeInTheDocument();
    expect(container.firstElementChild).toHaveClass("runcard--partial");
    fireEvent.click(screen.getByRole("button", { name: "查看结果" }));
    expect(onUndo).toHaveBeenCalledTimes(1);
  });

  it("第二次撤销完成后，累计账本与本次结果各自标明口径", () => {
    const secondResult: UndoResultRecord = {
      session_id: "s1",
      run_id: "run-1",
      report: {
        restored: ["b.ts", "c.ts"],
        skipped: [],
        failed: [],
      },
      selected_entries: [
        { file_path: "b.ts", change_kind: "modified" },
        { file_path: "c.ts", change_kind: "modified" },
      ],
      total_entries: 3,
    };

    render(
      <RunCard
        block={{
          ...base,
          state: "undone",
          undo_total: 3,
          undo_undone: 3,
          undo_result: secondResult,
        }}
        onView={() => {}}
        onUndo={() => {}}
      />,
    );

    expect(screen.getByText("已撤销本轮")).toBeInTheDocument();
    expect(
      screen.getByText("本次已还原 2 个 · 本次未选择 1 个"),
    ).toBeInTheDocument();
  });
});

import {
  act,
  fireEvent,
  render,
  screen,
  waitFor,
  within,
} from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { UndoEntry, UndoReport } from "../types/undo";

const { invokeMock } = vi.hoisted(() => ({ invokeMock: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke: invokeMock }));

import { UndoReviewPanel } from "./UndoReviewPanel";

const DIGESTS = {
  modified: "1".repeat(64),
  created: "2".repeat(64),
  deleted: "3".repeat(64),
  binary: "4".repeat(64),
  large: "5".repeat(64),
  undone: "6".repeat(64),
};

const entries: UndoEntry[] = [
  {
    file_path: "src/modified.ts",
    change_kind: "modified",
    preimage_preview: { kind: "text", content: "const value = 'old';\n" },
    current_preview: { kind: "text", content: "const value = 'new';\n" },
    is_binary: false,
    size_bytes: 21,
    current_digest: DIGESTS.modified,
    already_undone: false,
    stale: false,
  },
  {
    file_path: "src/created.ts",
    change_kind: "created",
    preimage_preview: { kind: "missing" },
    current_preview: { kind: "text", content: "export const fresh = true;\n" },
    is_binary: false,
    size_bytes: 27,
    current_digest: DIGESTS.created,
    already_undone: false,
    stale: false,
  },
  {
    file_path: "src/deleted.ts",
    change_kind: "deleted",
    preimage_preview: {
      kind: "text",
      content: "export const legacy = true;\n",
    },
    current_preview: { kind: "missing" },
    is_binary: false,
    size_bytes: 28,
    current_digest: DIGESTS.deleted,
    already_undone: false,
    stale: false,
  },
  {
    file_path: "public/image.png",
    change_kind: "modified",
    preimage_preview: { kind: "binary", size_bytes: 1024 },
    current_preview: { kind: "binary", size_bytes: 2048 },
    is_binary: true,
    size_bytes: 2048,
    current_digest: DIGESTS.binary,
    already_undone: false,
    stale: false,
  },
  {
    file_path: "fixtures/large.json",
    change_kind: "modified",
    preimage_preview: { kind: "too_large", size_bytes: 1_800_000 },
    current_preview: { kind: "too_large", size_bytes: 1_800_000 },
    is_binary: false,
    size_bytes: 1_800_000,
    current_digest: DIGESTS.large,
    already_undone: false,
    stale: false,
  },
  {
    file_path: "src/already.ts",
    change_kind: "modified",
    preimage_preview: { kind: "text", content: "before\n" },
    current_preview: { kind: "text", content: "before\n" },
    is_binary: false,
    size_bytes: 7,
    current_digest: DIGESTS.undone,
    already_undone: true,
    stale: false,
  },
];

function mockList(report?: UndoReport) {
  invokeMock.mockImplementation((command: string) => {
    if (command === "list_run_undo_entries") return Promise.resolve(entries);
    if (command === "undo_run_edits") {
      return Promise.resolve(
        report ?? { restored: [], skipped: [], failed: [] },
      );
    }
    return Promise.resolve();
  });
}

function renderPanel(onComplete = vi.fn()) {
  render(
    <UndoReviewPanel
      sessionId="session-1"
      runId="run-1"
      onBack={() => {}}
      onComplete={onComplete}
    />,
  );
  return onComplete;
}

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((next) => {
    resolve = next;
  });
  return { promise, resolve };
}

describe("UndoReviewPanel", () => {
  beforeEach(() => {
    invokeMock.mockReset();
  });

  it("按 run 拉清单并保持原型 header → file-list → footer DOM，diff 默认折叠", async () => {
    mockList();
    const { container } = render(
      <UndoReviewPanel
        sessionId="session-1"
        runId="run-1"
        onBack={() => {}}
        onComplete={() => {}}
      />,
    );

    expect(
      await screen.findByText("这一轮的改动 · 6 个文件"),
    ).toBeInTheDocument();
    expect(invokeMock).toHaveBeenCalledWith("list_run_undo_entries", {
      sessionId: "session-1",
      runId: "run-1",
    });
    const root = container.querySelector(".undo-review");
    expect(root?.children[0]).toHaveClass("review-head");
    expect(root?.children[1]).toHaveClass("file-list");
    expect(root?.children[2]).toHaveClass("review-foot");
    expect(container.querySelector(".file.open")).toBeNull();
    expect(screen.getByText("src/modified.ts")).toHaveAttribute(
      "title",
      "src/modified.ts",
    );
    expect(
      screen.queryByText("- const value = 'old';"),
    ).not.toBeInTheDocument();

    const footer = container.querySelector(".review-foot");
    const boundary = within(footer as HTMLElement).getByTestId(
      "undo-boundary-notice",
    );
    const button = within(footer as HTMLElement).getByRole("button", {
      name: "撤销选中的 2 个文件",
    });
    expect(boundary.nextElementSibling).toBe(button);
    expect(
      within(boundary).getByText("撤销只覆盖 agent 用编辑工具改的文件。"),
    ).toBeInTheDocument();
    expect(boundary).toHaveTextContent(
      "agent 在终端里干的事（rm / sed -i / 脚本 / 重定向）不在此列，退不回来。",
    );
    expect(boundary.querySelectorAll("code")).toHaveLength(2);
  });

  it("三类文案与折叠 diff 正确；二进制/超大不可展开但仍可勾", async () => {
    mockList();
    renderPanel();
    await screen.findByText("src/modified.ts");

    expect(screen.getByText("新建 · 撤销会删除这个文件")).toBeInTheDocument();
    expect(screen.getByText("删除 · 撤销会恢复这个文件")).toBeInTheDocument();
    expect(
      screen.getByText("二进制文件，无法预览 · 仍可勾选撤销"),
    ).toBeInTheDocument();
    expect(
      screen.getByText(/文件过大（1\.7 MB），无法预览 · 仍可勾选撤销/),
    ).toBeInTheDocument();

    await userEvent.click(
      screen.getByRole("button", { name: /src\/modified\.ts/ }),
    );
    expect(screen.getAllByText("改动前 → 现在").length).toBeGreaterThan(1);
    expect(screen.getByText("- const value = 'old';")).toBeInTheDocument();
    expect(screen.getByText("+ const value = 'new';")).toBeInTheDocument();

    expect(
      screen.getByRole("button", { name: /public\/image\.png/ }),
    ).toBeDisabled();
    expect(
      screen.getByRole("button", { name: /fixtures\/large\.json/ }),
    ).toBeDisabled();
    expect(
      screen.getByRole("checkbox", { name: "选择 public/image.png" }),
    ).toBeEnabled();
    expect(
      screen.getByRole("checkbox", { name: "选择 fixtures/large.json" }),
    ).toBeEnabled();
  });

  it("already_undone 灰化划线且不可勾；选中数联动到 0 时按钮禁用", async () => {
    mockList();
    const { container } = render(
      <UndoReviewPanel
        sessionId="session-1"
        runId="run-1"
        onBack={() => {}}
        onComplete={() => {}}
      />,
    );
    await screen.findByText("src/already.ts");

    const already = screen.getByRole("checkbox", {
      name: "选择 src/already.ts",
    });
    expect(already).toBeDisabled();
    expect(already.closest(".file")).toHaveClass("already-undone");
    expect(
      container.querySelector(".already-undone .file-name"),
    ).toHaveTextContent("src/already.ts");

    await userEvent.click(
      screen.getByRole("checkbox", { name: "选择 src/modified.ts" }),
    );
    await userEvent.click(
      screen.getByRole("checkbox", { name: "选择 src/created.ts" }),
    );
    expect(
      screen.getByRole("button", { name: "撤销选中的 0 个文件" }),
    ).toBeDisabled();
  });

  // F1 前端补测（须改 2）：stale 是后端 undo_run_edits_inner 拒绝写回之前唯一的 UI 提示——
  // 这条测试独立构造一份不含全局 entries 干扰的清单，专测 stale 条目本身：禁止勾选 +
  // 展示专门的过期文案（不再是误导性的「未留撤销记录」，因为记录明明在，只是陈旧）。
  it("stale 条目禁止勾选，且展示『已过期』文案而非其他状态文案", async () => {
    const staleEntry: UndoEntry = {
      file_path: "src/stale.ts",
      change_kind: "modified",
      preimage_preview: { kind: "text", content: "before\n" },
      current_preview: { kind: "text", content: "after\n" },
      is_binary: false,
      size_bytes: 6,
      current_digest: "7".repeat(64),
      already_undone: false,
      stale: true,
    };
    invokeMock.mockImplementation((command: string) => {
      if (command === "list_run_undo_entries")
        return Promise.resolve([staleEntry]);
      return Promise.resolve();
    });
    const { container } = render(
      <UndoReviewPanel
        sessionId="session-1"
        runId="run-stale"
        onBack={() => {}}
        onComplete={() => {}}
      />,
    );
    await screen.findByText("src/stale.ts");

    const checkbox = screen.getByRole("checkbox", {
      name: "选择 src/stale.ts",
    });
    expect(checkbox).toBeDisabled();
    expect(checkbox.closest(".file")).toHaveClass("stale");
    expect(
      screen.getByText(
        "这条记录已过期：文件在此之后又被提交，撤销会覆盖之后的提交 · 不可选择",
      ),
    ).toBeInTheDocument();
    // 不该同时显示别的状态文案（比如把陈旧误判成普通 modified）。
    expect(
      screen.queryByText("修改 · 撤销后恢复为本轮改动前的内容"),
    ).not.toBeInTheDocument();
    expect(container.querySelector(".already-undone")).not.toBeInTheDocument();
  });

  // F1 前端补测（须改 2）：默认「全选」逻辑必须排除 stale 条目——否则用户一进面板看到的
  // 选中数就是错的（把点不了的文件也算进去），而且这是最容易在重构中被静默删掉的一行
  // （`!entry.stale &&`），必须有测试守住。
  it("默认全选跳过 stale 条目：一份清单里一条新鲜、一条陈旧，默认只选中新鲜的那条", async () => {
    const freshEntry: UndoEntry = {
      file_path: "src/fresh.ts",
      change_kind: "modified",
      preimage_preview: { kind: "text", content: "before\n" },
      current_preview: { kind: "text", content: "after\n" },
      is_binary: false,
      size_bytes: 6,
      current_digest: "8".repeat(64),
      already_undone: false,
      stale: false,
    };
    const staleEntry: UndoEntry = {
      file_path: "src/stale-mixed.ts",
      change_kind: "modified",
      preimage_preview: { kind: "text", content: "before\n" },
      current_preview: { kind: "text", content: "after\n" },
      is_binary: false,
      size_bytes: 6,
      current_digest: "9".repeat(64),
      already_undone: false,
      stale: true,
    };
    invokeMock.mockImplementation((command: string) => {
      if (command === "list_run_undo_entries") {
        return Promise.resolve([freshEntry, staleEntry]);
      }
      return Promise.resolve();
    });
    render(
      <UndoReviewPanel
        sessionId="session-1"
        runId="run-mixed-stale"
        onBack={() => {}}
        onComplete={() => {}}
      />,
    );
    await screen.findByText("src/stale-mixed.ts");

    // 默认选中数只有 1（新鲜的那条）——如果 stale 被漏掉排除条件，这里会变成 2。
    expect(
      screen.getByRole("button", { name: "撤销选中的 1 个文件" }),
    ).toBeInTheDocument();
    expect(
      screen.getByRole("checkbox", { name: "选择 src/fresh.ts" }),
    ).toBeChecked();
    expect(
      screen.getByRole("checkbox", { name: "选择 src/stale-mixed.ts" }),
    ).not.toBeChecked();
  });

  // P2（reviewer 建议·比按钮直接消失更好）：一整轮都陈旧时给空态解释，而不是让用户面对
  // 一个永远点不动、也不知道为什么的确认按钮。
  it("P2：一整轮记录全部 stale 时显示『已全部过期』的空态解释", async () => {
    const staleOnly: UndoEntry = {
      file_path: "src/only-stale.ts",
      change_kind: "modified",
      preimage_preview: { kind: "text", content: "before\n" },
      current_preview: { kind: "text", content: "after\n" },
      is_binary: false,
      size_bytes: 6,
      current_digest: "a".repeat(64),
      already_undone: false,
      stale: true,
    };
    invokeMock.mockImplementation((command: string) => {
      if (command === "list_run_undo_entries")
        return Promise.resolve([staleOnly]);
      return Promise.resolve();
    });
    render(
      <UndoReviewPanel
        sessionId="session-1"
        runId="run-all-stale"
        onBack={() => {}}
        onComplete={() => {}}
      />,
    );

    expect(
      await screen.findByText(
        "这一轮记录已全部过期：改过的文件后来又被提交，撤销会覆盖那些提交，所以都不能选。",
      ),
    ).toBeInTheDocument();
    // 空态文案不该跟真正的「这轮没有任何记录」文案混在一起。
    expect(
      screen.queryByText("这一轮没有可撤销的编辑工具改动。"),
    ).not.toBeInTheDocument();
  });

  it("提交时 paths 与 expectedDigests 逐项同序传给后端", async () => {
    mockList({
      restored: ["src/created.ts", "src/deleted.ts"],
      skipped: [],
      failed: [],
    });
    renderPanel();
    await screen.findByText("src/deleted.ts");

    fireEvent.click(
      screen.getByRole("checkbox", { name: "选择 src/modified.ts" }),
    );
    fireEvent.click(
      screen.getByRole("checkbox", { name: "选择 src/deleted.ts" }),
    );
    fireEvent.click(
      screen.getByRole("button", { name: "撤销选中的 2 个文件" }),
    );

    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("undo_run_edits", {
        sessionId: "session-1",
        runId: "run-1",
        paths: ["src/created.ts", "src/deleted.ts"],
        expectedDigests: [DIGESTS.created, DIGESTS.deleted],
      }),
    );
  });

  it("撤销 refresh 后仍把 restored entry 的 diff 传给结果行并可展开", async () => {
    const initialEntry = {
      ...entries[0],
      file_path: "src/deep/restored.ts",
    };
    const refreshedEntry = {
      ...initialEntry,
      preimage_preview: {
        kind: "text" as const,
        content: "const restoredSnapshot = true;\n",
      },
      current_preview: {
        kind: "text" as const,
        content: "const restoredSnapshot = true;\n",
      },
      already_undone: true,
    };
    let listCalls = 0;
    invokeMock.mockImplementation((command: string) => {
      if (command === "list_run_undo_entries") {
        listCalls += 1;
        return Promise.resolve(
          listCalls === 1 ? [initialEntry] : [refreshedEntry],
        );
      }
      if (command === "undo_run_edits") {
        return Promise.resolve({
          restored: [initialEntry.file_path],
          skipped: [],
          failed: [],
        });
      }
      return Promise.resolve();
    });
    render(
      <UndoReviewPanel
        sessionId="session-1"
        runId="run-restored-diff"
        onBack={() => {}}
        onComplete={() => {}}
      />,
    );
    await screen.findByText(initialEntry.file_path);
    await userEvent.click(
      screen.getByRole("button", { name: "撤销选中的 1 个文件" }),
    );

    await waitFor(() => expect(listCalls).toBe(2));
    const toggle = await screen.findByRole("button", {
      name: /src\/deep\/restored\.ts/,
    });
    const restoredRow = toggle.closest<HTMLElement>("[data-result]");
    expect(toggle).toHaveClass("file-toggle--interactive");
    expect(toggle.querySelector(".chev")).toBeInTheDocument();
    await userEvent.click(toggle);
    expect(
      within(restoredRow as HTMLElement).getByText(
        "const restoredSnapshot = true;",
      ),
    ).toBeVisible();
  });

  it("无 diff 的 restored 结果行不显示 chevron、不使用手型 class，且完整路径在 title", async () => {
    const binaryEntry = {
      ...entries[3],
      file_path: "public/a/very/long/path/image.png",
    };
    invokeMock.mockImplementation((command: string) => {
      if (command === "list_run_undo_entries") {
        return Promise.resolve([binaryEntry]);
      }
      return Promise.resolve();
    });
    const { container } = render(
      <UndoReviewPanel
        sessionId="session-1"
        runId="run-restored-binary"
        initialResult={{
          session_id: "session-1",
          run_id: "run-restored-binary",
          report: {
            restored: [binaryEntry.file_path],
            skipped: [],
            failed: [],
          },
          selected_entries: [
            {
              file_path: binaryEntry.file_path,
              change_kind: binaryEntry.change_kind,
            },
          ],
          total_entries: 1,
        }}
        onBack={() => {}}
        onComplete={() => {}}
      />,
    );

    const fileName = await screen.findByText(binaryEntry.file_path);
    expect(fileName).toHaveAttribute("title", binaryEntry.file_path);
    const restoredRow = fileName.closest<HTMLElement>("[data-result]");
    const staticToggle = restoredRow?.querySelector(".file-toggle");
    expect(staticToggle).not.toHaveClass("file-toggle--interactive");
    expect(staticToggle?.querySelector(".chev")).toBeNull();
    expect(
      staticToggle?.querySelector(".chev-placeholder"),
    ).toBeInTheDocument();
    expect(
      container.querySelector('[data-result="restored"] button.file-toggle'),
    ).toBeNull();
  });

  it("跳过且文件被改过的结果行仍可展开 diff", async () => {
    const changedEntry = {
      ...entries[0],
      file_path: "src/changed-after-review.ts",
    };
    invokeMock.mockImplementation((command: string) => {
      if (command === "list_run_undo_entries") {
        return Promise.resolve([changedEntry]);
      }
      return Promise.resolve();
    });
    render(
      <UndoReviewPanel
        sessionId="session-1"
        runId="run-changed-result"
        initialResult={{
          session_id: "session-1",
          run_id: "run-changed-result",
          report: {
            restored: [],
            skipped: [
              {
                file_path: changedEntry.file_path,
                reason:
                  "file changed after the undo list was viewed; not restored",
              },
            ],
            failed: [],
          },
          selected_entries: [
            {
              file_path: changedEntry.file_path,
              change_kind: changedEntry.change_kind,
            },
          ],
          total_entries: 1,
        }}
        onBack={() => {}}
        onComplete={() => {}}
      />,
    );

    const toggle = await screen.findByRole("button", {
      name: /src\/changed-after-review\.ts/,
    });
    const fileName = within(toggle).getByText(changedEntry.file_path);
    expect(fileName).toHaveAttribute("title", changedEntry.file_path);
    const changedRow = toggle.closest<HTMLElement>("[data-result]");
    expect(toggle).toHaveClass("file-toggle--interactive");
    await userEvent.click(toggle);
    expect(
      within(changedRow as HTMLElement).getByText("- const value = 'old';"),
    ).toBeVisible();
    expect(
      within(changedRow as HTMLElement).getByText("+ const value = 'new';"),
    ).toBeVisible();
  });

  it("部分成功持久展示 skipped 精确说明与 failed 原因，不伪装全部成功", async () => {
    const report: UndoReport = {
      restored: ["src/modified.ts"],
      skipped: [
        {
          file_path: "src/created.ts",
          reason: "file changed after the undo list was viewed; not restored",
        },
      ],
      failed: [{ file_path: "src/deleted.ts", reason: "permission denied" }],
    };
    mockList(report);
    const onComplete = renderPanel();
    await screen.findByText("src/deleted.ts");
    await userEvent.click(
      screen.getByRole("checkbox", { name: "选择 src/deleted.ts" }),
    );
    await userEvent.click(
      screen.getByRole("button", { name: "撤销选中的 3 个文件" }),
    );

    expect(await screen.findByText("未还原 1 个文件")).toBeInTheDocument();
    expect(screen.getByText("失败 1 个")).toBeInTheDocument();
    expect(screen.getAllByText(/permission denied/).length).toBeGreaterThan(0);
    const skippedRow = screen
      .getByText("src/created.ts")
      .closest<HTMLElement>("[data-result]");
    expect(skippedRow).toHaveAttribute("data-result", "skipped");
    expect(within(skippedRow as HTMLElement).getByText("未还原")).toBeVisible();
    expect(
      within(skippedRow as HTMLElement).getByText(
        "未还原 · 你查看后它又变了，没有还原",
      ),
    ).toBeVisible();
    const failedRow = screen
      .getByText("src/deleted.ts")
      .closest<HTMLElement>("[data-result]");
    expect(failedRow).toHaveAttribute("data-result", "failed");
    expect(
      within(failedRow as HTMLElement).getByText(/permission denied/),
    ).toBeVisible();
    expect(screen.queryByText("已撤销本轮")).not.toBeInTheDocument();
    expect(
      screen.queryByRole("button", { name: /撤销选中的/ }),
    ).not.toBeInTheDocument();
    expect(screen.getByTestId("undo-boundary-notice")).toBeInTheDocument();
    expect(onComplete).toHaveBeenCalledWith(
      expect.objectContaining({
        session_id: "session-1",
        run_id: "run-1",
        report,
        total_entries: 6,
      }),
    );
  });

  it("按后端 reason 如实区分 skipped，未知原因不套默认文案", async () => {
    const reasons = [
      {
        file_path: "src/changed.ts",
        reason: "file changed after the undo list was viewed; not restored",
      },
      {
        file_path: "src/unsafe.ts",
        reason:
          "checkpoint path could not be safely resolved before restore; not restored: ancestor is a symlink",
      },
      {
        file_path: "src/already-undone.ts",
        reason: "checkpoint entry was already undone",
      },
      {
        file_path: "src/future.ts",
        reason: "future backend reason: policy gate",
      },
    ];
    const reasonEntries = reasons.map(({ file_path }, index) => ({
      ...entries[0],
      file_path,
      current_digest: String(index + 7).repeat(64),
    }));
    invokeMock.mockImplementation((command: string) => {
      if (command === "list_run_undo_entries") {
        return Promise.resolve(reasonEntries);
      }
      return Promise.resolve();
    });

    render(
      <UndoReviewPanel
        sessionId="session-1"
        runId="run-reasons"
        initialResult={{
          session_id: "session-1",
          run_id: "run-reasons",
          report: { restored: [], skipped: reasons, failed: [] },
          selected_entries: reasonEntries.map(({ file_path, change_kind }) => ({
            file_path,
            change_kind,
          })),
          total_entries: reasonEntries.length,
        }}
        onBack={() => {}}
        onComplete={() => {}}
      />,
    );

    expect(await screen.findByText("未还原 4 个文件")).toBeInTheDocument();
    expect(
      within(screen.getByRole("status")).queryByText(/你查看后它又变了/),
    ).not.toBeInTheDocument();
    expect(
      screen.getByText("未还原 · 你查看后它又变了，没有还原"),
    ).toBeVisible();
    expect(
      screen.getByText("未还原 · 这个路径现在无法安全访问，没有还原"),
    ).toBeVisible();
    expect(screen.getByText("之前已经撤销过了")).toBeVisible();
    expect(
      screen.getByText("未还原 · 后端原因：future backend reason: policy gate"),
    ).toBeVisible();
  });

  it("切到 run B 后不渲染仍在飞行的 run A 撤销结果", async () => {
    const undoA = deferred<UndoReport>();
    const runAEntry = { ...entries[0], file_path: "src/run-a.ts" };
    const runBEntry = { ...entries[0], file_path: "src/run-b.ts" };
    invokeMock.mockImplementation(
      (command: string, args?: { runId?: string }) => {
        if (command === "list_run_undo_entries") {
          return Promise.resolve(
            args?.runId === "run-a" ? [runAEntry] : [runBEntry],
          );
        }
        if (command === "undo_run_edits") return undoA.promise;
        return Promise.resolve();
      },
    );
    const onComplete = vi.fn();
    const { rerender } = render(
      <UndoReviewPanel
        sessionId="session-1"
        runId="run-a"
        onBack={() => {}}
        onComplete={onComplete}
      />,
    );
    await screen.findByText("src/run-a.ts");
    await userEvent.click(
      screen.getByRole("button", { name: "撤销选中的 1 个文件" }),
    );
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith(
        "undo_run_edits",
        expect.objectContaining({ runId: "run-a" }),
      ),
    );

    rerender(
      <UndoReviewPanel
        sessionId="session-1"
        runId="run-b"
        onBack={() => {}}
        onComplete={onComplete}
      />,
    );
    expect(await screen.findByText("src/run-b.ts")).toBeVisible();
    await act(async () => {
      undoA.resolve({ restored: ["src/run-a.ts"], skipped: [], failed: [] });
    });

    if (screen.queryByText("src/run-a.ts")) {
      throw new Error("STALE_RUN_A_RESULT_RENDERED_IN_RUN_B_PANEL");
    }
    expect(screen.getByText("src/run-b.ts")).toBeVisible();
    expect(onComplete).not.toHaveBeenCalled();
  });

  it("切到 run B 后丢弃晚到的 run A 清单", async () => {
    const listA = deferred<UndoEntry[]>();
    const runAEntry = { ...entries[0], file_path: "src/late-run-a.ts" };
    const runBEntry = { ...entries[0], file_path: "src/current-run-b.ts" };
    invokeMock.mockImplementation(
      (command: string, args?: { runId?: string }) => {
        if (command === "list_run_undo_entries") {
          return args?.runId === "run-a"
            ? listA.promise
            : Promise.resolve([runBEntry]);
        }
        return Promise.resolve();
      },
    );
    const { rerender } = render(
      <UndoReviewPanel
        sessionId="session-1"
        runId="run-a"
        onBack={() => {}}
        onComplete={() => {}}
      />,
    );
    rerender(
      <UndoReviewPanel
        sessionId="session-1"
        runId="run-b"
        onBack={() => {}}
        onComplete={() => {}}
      />,
    );
    expect(await screen.findByText("src/current-run-b.ts")).toBeVisible();
    await act(async () => {
      listA.resolve([runAEntry]);
    });

    expect(screen.getByText("src/current-run-b.ts")).toBeVisible();
    expect(screen.queryByText("src/late-run-a.ts")).not.toBeInTheDocument();
  });

  it("撤销后的清单刷新失败时保留后端逐文件报告", async () => {
    let listCalls = 0;
    invokeMock.mockImplementation((command: string) => {
      if (command === "list_run_undo_entries") {
        listCalls += 1;
        return listCalls === 1
          ? Promise.resolve([entries[0]])
          : Promise.reject(new Error("refresh unavailable"));
      }
      if (command === "undo_run_edits") {
        return Promise.resolve({
          restored: [entries[0].file_path],
          skipped: [],
          failed: [],
        });
      }
      return Promise.resolve();
    });
    renderPanel();
    await screen.findByText(entries[0].file_path);
    await userEvent.click(
      screen.getByRole("button", { name: "撤销选中的 1 个文件" }),
    );

    expect(await screen.findByText("已还原 1 个文件")).toBeVisible();
    expect(screen.getByText("已还原")).toBeVisible();
    expect(screen.queryByText(/refresh unavailable/)).not.toBeInTheDocument();
  });
});

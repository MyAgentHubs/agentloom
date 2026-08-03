import { describe, it, expect, vi } from "vitest";
import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { ReviewPanel } from "./ReviewPanel";
import type { ReviewResult } from "../types/agent";

const TWO_FILE_PATCH = [
  "diff --git a/src/GoalBar.tsx b/src/GoalBar.tsx",
  "--- a/src/GoalBar.tsx",
  "+++ b/src/GoalBar.tsx",
  "@@ -1 +1 @@",
  "-old",
  "+new",
  "diff --git a/tmp.txt b/tmp.txt",
  "new file mode 100644",
  "--- /dev/null",
  "+++ b/tmp.txt",
  "@@ -0,0 +1 @@",
  "+hello",
].join("\n");

const review: ReviewResult = {
  has_changes: true,
  stat: "2 files changed",
  patch: TWO_FILE_PATCH,
  files_changed: 2,
  files: [
    { path: "src/GoalBar.tsx", undoable: true },
    { path: "tmp.txt", undoable: false },
  ],
  diff_available: true,
};

const noop = () => {};

const patchFor = (path: string, body: string[], hunk = true) =>
  [
    `diff --git a/${path} b/${path}`,
    `--- a/${path}`,
    `+++ b/${path}`,
    ...(hunk ? [`@@ -1 +1,${body.length} @@`] : []),
    ...body,
  ].join("\n");

describe("ReviewPanel", () => {
  it("大量文件含巨型 jsonl 时不渲染数据正文，且每个文件仍保持可见的紧凑行", () => {
    const dataLines = Array.from(
      { length: 1_000 },
      (_, i) => `+{\"row\":${i}}`,
    );
    const patches = Array.from({ length: 135 }, (_, i) => {
      const path = i === 0 ? "data/dump.jsonl" : `src/file-${i}.ts`;
      const body = i === 0 ? dataLines : [`+export const value${i} = ${i};`];
      return [
        `diff --git a/${path} b/${path}`,
        "new file mode 100644",
        "--- /dev/null",
        `+++ b/${path}`,
        `@@ -0,0 +1,${body.length} @@`,
        ...body,
      ].join("\n");
    });
    const largeReview: ReviewResult = {
      ...review,
      patch: patches.join("\n"),
      files_changed: 135,
      files: patches.map((_, i) => ({
        path: i === 0 ? "data/dump.jsonl" : `src/file-${i}.ts`,
        undoable: true,
      })),
    };

    const { container } = render(
      <ReviewPanel review={largeReview} onClose={noop} />,
    );

    expect(screen.getByText("改动 · 135 文件")).toBeInTheDocument();
    expect(screen.getByText("data/dump.jsonl")).toBeInTheDocument();
    expect(screen.getByText("数据文件 · 不显示内容")).toBeInTheDocument();
    expect(screen.getByText("src/file-134.ts")).toBeInTheDocument();
    expect(container.querySelectorAll(".filediff__line")).toHaveLength(0);
    expect(
      Array.from(container.querySelectorAll<HTMLElement>(".filediff")).every(
        (card) => getComputedStyle(card).flexShrink === "0",
      ),
    ).toBe(true);
  });

  it("大型主流文本 diff 默认折叠，逐批展开且不一次塞入全部行", async () => {
    const changedLines = Array.from({ length: 501 }, (_, i) => `+line ${i}`);
    const path = "src/generated.ts";
    render(
      <ReviewPanel
        review={{
          ...review,
          patch: [
            `diff --git a/${path} b/${path}`,
            "new file mode 100644",
            "--- /dev/null",
            `+++ b/${path}`,
            `@@ -0,0 +1,${changedLines.length} @@`,
            ...changedLines,
          ].join("\n"),
          files_changed: 1,
          files: [{ path, undoable: true }],
        }}
        onClose={noop}
      />,
    );

    expect(screen.queryByText("+line 0")).not.toBeInTheDocument();
    await userEvent.click(screen.getByText(path));
    expect(screen.getByText("+line 0")).toBeInTheDocument();
    expect(screen.queryByText("+line 500")).not.toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: "显示更多" }));
    expect(screen.getByText("+line 500")).toBeInTheDocument();
  });

  it("review 在小文件与大文件间切换时重算默认展开态", () => {
    const smallPath = "src/small.ts";
    const largePath = "src/large.ts";
    const smallReview = {
      ...review,
      patch: patchFor(smallPath, ["+small"]),
      files: [{ path: smallPath, undoable: true }],
    };
    const largeReview = {
      ...review,
      patch: patchFor(
        largePath,
        Array.from({ length: 500 }, (_, i) => `+large ${i}`),
      ),
      files: [{ path: largePath, undoable: true }],
    };
    const { rerender } = render(
      <ReviewPanel review={smallReview} onClose={noop} />,
    );
    expect(
      screen.getByRole("button", { name: /src\/small\.ts/ }),
    ).toHaveAttribute("aria-expanded", "true");
    rerender(<ReviewPanel review={largeReview} onClose={noop} />);
    expect(
      screen.getByRole("button", { name: /src\/large\.ts/ }),
    ).toHaveAttribute("aria-expanded", "false");
    rerender(<ReviewPanel review={smallReview} onClose={noop} />);
    expect(
      screen.getByRole("button", { name: /src\/small\.ts/ }),
    ).toHaveAttribute("aria-expanded", "true");
  });

  it("文件顺序变化时，手动展开态跟随文件路径而非数组下标", async () => {
    const a = patchFor("src/a.ts", ["+a"]);
    const b = patchFor("src/b.ts", ["+b"]);
    const makeReview = (patch: string, paths: string[]) => ({
      ...review,
      patch,
      files: paths.map((path) => ({ path, undoable: true })),
    });
    const { rerender } = render(
      <ReviewPanel
        review={makeReview(`${a}\n${b}`, ["src/a.ts", "src/b.ts"])}
        onClose={noop}
      />,
    );
    await userEvent.click(screen.getByText("src/a.ts"));
    await userEvent.click(screen.getByText("src/b.ts"));

    rerender(
      <ReviewPanel
        review={makeReview(`${b}\n${a}`, ["src/b.ts", "src/a.ts"])}
        onClose={noop}
      />,
    );
    expect(screen.getByRole("button", { name: /src\/b\.ts/ })).toHaveAttribute(
      "aria-expanded",
      "true",
    );
    expect(screen.getByRole("button", { name: /src\/a\.ts/ })).toHaveAttribute(
      "aria-expanded",
      "false",
    );
  });

  it("默认折叠按实际 diff 行数覆盖 500/501/0 与多 hunk 上下文边界", () => {
    const cases = [
      { path: "src/exact-500.ts", body: 499, expected: "true", hunk: true },
      { path: "src/over-500.ts", body: 500, expected: "false", hunk: true },
      { path: "src/zero.ts", body: 0, expected: "true", hunk: false },
    ] as const;
    for (const item of cases) {
      const { unmount } = render(
        <ReviewPanel
          review={{
            ...review,
            patch: patchFor(
              item.path,
              Array.from({ length: item.body }, (_, i) => `+line ${i}`),
              item.hunk,
            ),
            files: [{ path: item.path, undoable: true }],
          }}
          onClose={noop}
        />,
      );
      expect(
        screen.getByRole("button", { name: new RegExp(item.path) }),
      ).toHaveAttribute("aria-expanded", item.expected);
      unmount();
    }

    const multiHunk = [
      patchFor(
        "src/multi.ts",
        Array.from({ length: 249 }, (_, i) => `+a ${i}`),
      ),
      "@@ -300,2 +300,251 @@",
      " context",
      ...Array.from({ length: 250 }, (_, i) => `+b ${i}`),
    ].join("\n");
    render(
      <ReviewPanel
        review={{
          ...review,
          patch: multiHunk,
          files: [{ path: "src/multi.ts", undoable: true }],
        }}
        onClose={noop}
      />,
    );
    expect(
      screen.getByRole("button", { name: /src\/multi\.ts/ }),
    ).toHaveAttribute("aria-expanded", "false");
  });

  it("无改动 → 不渲染", () => {
    const { container } = render(
      <ReviewPanel
        review={{
          has_changes: false,
          stat: "",
          patch: "",
          files_changed: 0,
          files: [],
          diff_available: true,
        }}
        onClose={noop}
      />,
    );
    expect(container.firstChild).toBeNull();
  });

  it("有改动 → 每个文件一张卡（路径可见）", () => {
    render(<ReviewPanel review={review} onClose={noop} />);
    expect(screen.getByText("src/GoalBar.tsx")).toBeInTheDocument();
    expect(screen.getByText("tmp.txt")).toBeInTheDocument();
    expect(screen.getByText("未留撤销记录 · 退不回")).toBeInTheDocument();
  });

  it("点折叠的文件卡头展开看到该文件 diff 行", async () => {
    render(<ReviewPanel review={review} onClose={noop} />);
    // 默认展开第一个文件（GoalBar）·第二个 tmp.txt 默认折叠
    expect(screen.queryByText("+hello")).not.toBeInTheDocument();
    await userEvent.click(screen.getByText("tmp.txt"));
    expect(screen.getByText("+hello")).toBeInTheDocument();
  });

  it("✕ 触发 onClose", async () => {
    const onClose = vi.fn();
    render(<ReviewPanel review={review} onClose={onClose} />);
    await userEvent.click(screen.getByLabelText("关闭"));
    expect(onClose).toHaveBeenCalledTimes(1);
  });

  it("底部状态摘要区分已提交/未提交文件数，不再说「改动保留在工作目录」这类改动已提交后会变假的话", () => {
    render(
      <ReviewPanel
        review={{
          ...review,
          committed_files_changed: 1,
          uncommitted_files_changed: 1,
        }}
        onClose={noop}
      />,
    );
    expect(
      screen.getByText("已提交 1 个文件 · 未提交 1 个文件"),
    ).toBeInTheDocument();
    expect(
      screen.queryByText("只读审查 · 改动保留在工作目录"),
    ).not.toBeInTheDocument();
    expect(
      screen.queryByText(
        "撤销只覆盖 agent 用编辑工具改的文件；终端里干的事退不回来",
      ),
    ).not.toBeInTheDocument();
  });

  it("底部状态摘要：全部已提交时只说已提交，不硬凑一句「未提交 0 个文件」", () => {
    render(
      <ReviewPanel
        review={{
          ...review,
          committed_files_changed: 2,
          uncommitted_files_changed: 0,
        }}
        onClose={noop}
      />,
    );
    expect(screen.getByText("已提交 2 个文件")).toBeInTheDocument();
  });

  it("底部状态摘要：全部未提交时只说未提交", () => {
    render(
      <ReviewPanel
        review={{
          ...review,
          committed_files_changed: 0,
          uncommitted_files_changed: 2,
        }}
        onClose={noop}
      />,
    );
    expect(screen.getByText("未提交 2 个文件")).toBeInTheDocument();
  });

  it("底部状态摘要：两个计数都缺失（旧数据/未知）时不渲染这一行——不知道就不说，不编造", () => {
    const { container } = render(
      <ReviewPanel review={review} onClose={noop} />,
    );
    expect(container.querySelector(".review__foot")).toBeNull();
  });
});

import { describe, expect, it } from "vitest";
import type { UndoEntry } from "../types/undo";
import { buildUndoDiff, buildUndoRequest } from "./undo";

function entry(
  filePath: string,
  digest: string,
  overrides: Partial<UndoEntry> = {},
): UndoEntry {
  return {
    file_path: filePath,
    change_kind: "modified",
    preimage_preview: { kind: "text", content: "same\nold\ntail\n" },
    current_preview: { kind: "text", content: "same\nnew\ntail\n" },
    is_binary: false,
    size_bytes: 14,
    current_digest: digest,
    already_undone: false,
    stale: false,
    ...overrides,
  };
}

describe("undo request safety", () => {
  it("paths 与 expectedDigests 从同一后端清单顺序一一对应", () => {
    const entries = [
      entry("z-last-clicked.ts", "digest-z"),
      entry("a-first-clicked.ts", "digest-a"),
      entry("disabled.ts", "digest-disabled", { already_undone: true }),
    ];
    const selected = new Set([
      "a-first-clicked.ts",
      "disabled.ts",
      "z-last-clicked.ts",
    ]);

    expect(buildUndoRequest(entries, selected)).toEqual({
      paths: ["z-last-clicked.ts", "a-first-clicked.ts"],
      expectedDigests: ["digest-z", "digest-a"],
    });
  });
});

describe("undo preview diff", () => {
  it("把结构化 preimage/current 转成带行号的 add/del/ctx", () => {
    const diff = buildUndoDiff(entry("src/a.ts", "digest"));
    expect(diff).toMatchObject({ insertions: 1, deletions: 1 });
    expect(diff?.lines).toEqual([
      { kind: "ctx", text: "same", oldLine: 1, newLine: 1 },
      { kind: "del", text: "old", oldLine: 2, newLine: null },
      { kind: "add", text: "new", oldLine: null, newLine: 2 },
      { kind: "ctx", text: "tail", oldLine: 3, newLine: 3 },
    ]);
  });

  it("二进制预览不生成可展开 diff", () => {
    expect(
      buildUndoDiff(
        entry("image.png", "digest", {
          is_binary: true,
          preimage_preview: { kind: "binary", size_bytes: 8 },
          current_preview: { kind: "binary", size_bytes: 9 },
        }),
      ),
    ).toBeNull();
  });
});

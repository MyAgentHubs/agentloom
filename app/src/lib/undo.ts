import type { UndoDiffLine, UndoDiffResult } from "./undoDiffTypes";
import type { UndoEntry } from "../types/undo";

export function buildUndoRequest(
  entries: readonly UndoEntry[],
  selectedPaths: ReadonlySet<string>,
): { paths: string[]; expectedDigests: string[] } {
  // One filtered array is the ordering source for both payload arrays. Never
  // iterate the selection independently: paths[i] must always use the digest
  // captured for that same list entry.
  const selectedEntries = entries.filter(
    (entry) => selectedPaths.has(entry.file_path) && !entry.already_undone,
  );
  return {
    paths: selectedEntries.map((entry) => entry.file_path),
    expectedDigests: selectedEntries.map((entry) => entry.current_digest),
  };
}

function splitTextLines(content: string): string[] {
  if (content === "") return [];
  const lines = content.replace(/\r\n/g, "\n").split("\n");
  if (content.endsWith("\n")) lines.pop();
  return lines;
}

type LineOperation = { kind: "ctx" | "add" | "del"; text: string };

function middleLineOperations(
  before: string[],
  after: string[],
): LineOperation[] {
  if (before.length === 0) {
    return after.map((text) => ({ kind: "add", text }));
  }
  if (after.length === 0) {
    return before.map((text) => ({ kind: "del", text }));
  }

  // Exact LCS for ordinary source files. If a still-previewable file contains
  // an unusually large fully-rewritten middle, use a bounded coarse diff rather
  // than allocating an unbounded browser-side matrix.
  const cells = (before.length + 1) * (after.length + 1);
  if (cells > 1_000_000) {
    return [
      ...before.map((text): LineOperation => ({ kind: "del", text })),
      ...after.map((text): LineOperation => ({ kind: "add", text })),
    ];
  }

  const width = after.length + 1;
  const lcs = new Uint32Array(cells);
  for (let oldIndex = before.length - 1; oldIndex >= 0; oldIndex -= 1) {
    for (let newIndex = after.length - 1; newIndex >= 0; newIndex -= 1) {
      const cell = oldIndex * width + newIndex;
      lcs[cell] =
        before[oldIndex] === after[newIndex]
          ? lcs[(oldIndex + 1) * width + newIndex + 1] + 1
          : Math.max(
              lcs[(oldIndex + 1) * width + newIndex],
              lcs[oldIndex * width + newIndex + 1],
            );
    }
  }

  const operations: LineOperation[] = [];
  let oldIndex = 0;
  let newIndex = 0;
  while (oldIndex < before.length && newIndex < after.length) {
    if (before[oldIndex] === after[newIndex]) {
      operations.push({ kind: "ctx", text: before[oldIndex] });
      oldIndex += 1;
      newIndex += 1;
    } else if (
      lcs[(oldIndex + 1) * width + newIndex] >=
      lcs[oldIndex * width + newIndex + 1]
    ) {
      operations.push({ kind: "del", text: before[oldIndex] });
      oldIndex += 1;
    } else {
      operations.push({ kind: "add", text: after[newIndex] });
      newIndex += 1;
    }
  }
  while (oldIndex < before.length) {
    operations.push({ kind: "del", text: before[oldIndex] });
    oldIndex += 1;
  }
  while (newIndex < after.length) {
    operations.push({ kind: "add", text: after[newIndex] });
    newIndex += 1;
  }
  return operations;
}

function lineOperations(before: string[], after: string[]): LineOperation[] {
  let prefix = 0;
  while (
    prefix < before.length &&
    prefix < after.length &&
    before[prefix] === after[prefix]
  ) {
    prefix += 1;
  }

  let suffix = 0;
  while (
    suffix < before.length - prefix &&
    suffix < after.length - prefix &&
    before[before.length - suffix - 1] === after[after.length - suffix - 1]
  ) {
    suffix += 1;
  }

  const prefixLines = before
    .slice(0, prefix)
    .map((text): LineOperation => ({ kind: "ctx", text }));
  const oldMiddle = before.slice(prefix, before.length - suffix);
  const newMiddle = after.slice(prefix, after.length - suffix);
  const suffixLines = before
    .slice(before.length - suffix)
    .map((text): LineOperation => ({ kind: "ctx", text }));
  return [
    ...prefixLines,
    ...middleLineOperations(oldMiddle, newMiddle),
    ...suffixLines,
  ];
}

function previewText(
  entry: UndoEntry,
): { before: string; after: string } | null {
  const before =
    entry.preimage_preview.kind === "missing"
      ? ""
      : entry.preimage_preview.kind === "text"
        ? entry.preimage_preview.content
        : null;
  const after =
    entry.current_preview.kind === "missing"
      ? ""
      : entry.current_preview.kind === "text"
        ? entry.current_preview.content
        : null;
  return before !== null && after !== null ? { before, after } : null;
}

export function buildUndoDiff(entry: UndoEntry): UndoDiffResult | null {
  const content = previewText(entry);
  if (!content || entry.is_binary) return null;

  const operations = lineOperations(
    splitTextLines(content.before),
    splitTextLines(content.after),
  );
  let oldLine = 1;
  let newLine = 1;
  let insertions = 0;
  let deletions = 0;
  const lines: UndoDiffLine[] = operations.map((operation) => {
    if (operation.kind === "add") {
      insertions += 1;
      const line = {
        ...operation,
        oldLine: null,
        newLine,
      };
      newLine += 1;
      return line;
    }
    if (operation.kind === "del") {
      deletions += 1;
      const line = {
        ...operation,
        oldLine,
        newLine: null,
      };
      oldLine += 1;
      return line;
    }
    const line = { ...operation, oldLine, newLine };
    oldLine += 1;
    newLine += 1;
    return line;
  });
  return { lines, insertions, deletions };
}

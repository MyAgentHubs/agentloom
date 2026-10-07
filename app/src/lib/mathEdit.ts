import type { Edit } from "./markdownEdit";

// Explicit TeX delimiters avoid the ambiguity between dollars and currency.
export function insertMath(
  value: string,
  start: number,
  end: number,
  display: boolean,
): Edit {
  const selection = value.slice(start, end);
  const before = display
    ? `${start > 0 && value[start - 1] !== "\n" ? "\n" : ""}\\[\n`
    : "\\(";
  const after = display
    ? `\n\\]${end < value.length && value[end] !== "\n" ? "\n" : ""}`
    : "\\)";
  return {
    from: start,
    to: end,
    insert: before + selection + after,
    selStart: start + before.length,
    selEnd: start + before.length + selection.length,
  };
}

// Pure helpers that describe lightweight Markdown edits for a plain textarea.
// They never touch the DOM: each returns a single replacement of
// value[from, to) plus the selection to restore afterwards. All offsets are
// UTF-16 code units, matching textarea.selectionStart / selectionEnd.
// A null result means "not handled, let the browser do its default thing".

export type Edit = {
  from: number;
  to: number;
  insert: string;
  selStart: number;
  selEnd: number;
};

// Indent, "1." or a bullet, at least one space, then an optional task box.
// "1.5" and "-a" are not list items.
const LIST_LINE = /^([ \t]*)(\d+\.|[-*])( +)(\[[ xX]\](?: +|$))?/;
const OPEN_FENCE_LINE = /^( {0,3})```[^`]*$/;
const FENCE_START = /^ {0,3}```/;

function lineStartOf(value: string, pos: number): number {
  return pos === 0 ? 0 : value.lastIndexOf("\n", pos - 1) + 1;
}

function lineEndOf(value: string, pos: number): number {
  const i = value.indexOf("\n", pos);
  return i < 0 ? value.length : i;
}

function countFenceLines(text: string): number {
  return text.split("\n").filter((line) => FENCE_START.test(line)).length;
}

export function toggleWrap(
  value: string,
  selStart: number,
  selEnd: number,
  marker: string,
): Edit {
  const m = marker.length;
  const sel = value.slice(selStart, selEnd);
  // Markers sit just outside the selection (or around an empty caret): remove them.
  // A selection that contains further markers spans several wrapped runs, so
  // stripping the outer pair would leave stray markers behind: wrap it instead.
  const hasInnerMarker = sel.includes(marker);
  if (
    !hasInnerMarker &&
    selStart >= m &&
    value.slice(selStart - m, selStart) === marker &&
    value.slice(selEnd, selEnd + m) === marker
  ) {
    const from = selStart - m;
    return {
      from,
      to: selEnd + m,
      insert: sel,
      selStart: from,
      selEnd: from + sel.length,
    };
  }
  // The selection itself starts and ends with the markers: remove them.
  const inner = sel.slice(m, sel.length - m);
  if (
    sel.length >= 2 * m &&
    sel.startsWith(marker) &&
    sel.endsWith(marker) &&
    !inner.includes(marker)
  ) {
    return {
      from: selStart,
      to: selEnd,
      insert: inner,
      selStart,
      selEnd: selStart + inner.length,
    };
  }
  return {
    from: selStart,
    to: selEnd,
    insert: marker + sel + marker,
    selStart: selStart + m,
    selEnd: selStart + m + sel.length,
  };
}

export function continueList(
  value: string,
  selStart: number,
  selEnd: number,
): Edit | null {
  if (value.slice(selStart, selEnd).includes("\n")) return null;
  const ls = lineStartOf(value, selStart);
  const le = lineEndOf(value, selEnd);
  const line = value.slice(ls, le);
  // Inside a fenced code block a "- " line is code, not a list.
  if (countFenceLines(value.slice(0, ls)) % 2 === 1) return null;
  const match = LIST_LINE.exec(line);
  if (!match || selStart < ls + match[0].length) return null;
  // An item with nothing after its prefix ends the list.
  if (line.slice(match[0].length).trim() === "") {
    return { from: ls, to: le, insert: "", selStart: ls, selEnd: ls };
  }
  const [, indent, marker, spaces, task] = match;
  const nextMarker = marker.endsWith(".")
    ? `${BigInt(marker.slice(0, -1)) + 1n}.`
    : marker;
  const next = `${indent}${nextMarker}${spaces}${task ? "[ ] " : ""}`;
  const insert = `\n${next}`;
  const caret = selStart + insert.length;
  return { from: selStart, to: selEnd, insert, selStart: caret, selEnd: caret };
}

export function indentListLine(
  value: string,
  selStart: number,
  selEnd: number,
  dir: 1 | -1,
): Edit | null {
  if (value.slice(selStart, selEnd).includes("\n")) return null;
  const ls = lineStartOf(value, selStart);
  const match = LIST_LINE.exec(value.slice(ls, lineEndOf(value, selEnd)));
  if (!match) return null;
  if (dir === 1) {
    return {
      from: ls,
      to: ls,
      insert: "  ",
      selStart: selStart + 2,
      selEnd: selEnd + 2,
    };
  }
  const leading = match[1];
  // Nothing to outdent: still report it as handled so Shift+Tab keeps focus in the box.
  if (leading === "") {
    return { from: ls, to: ls, insert: "", selStart, selEnd };
  }
  const n = leading.startsWith("\t") ? 1 : Math.min(2, leading.length);
  return {
    from: ls,
    to: ls + n,
    insert: "",
    selStart: Math.max(ls, selStart - n),
    selEnd: Math.max(ls, selEnd - n),
  };
}

export function autoCloseFence(
  value: string,
  selStart: number,
  selEnd: number,
): Edit | null {
  if (selStart !== selEnd) return null;
  const ls = lineStartOf(value, selStart);
  const le = lineEndOf(value, selStart);
  if (selStart !== le) return null;
  const match = OPEN_FENCE_LINE.exec(value.slice(ls, le));
  if (!match) return null;
  // An odd number of earlier fences means this line closes a block; an odd
  // number of later ones means this opener is already closed further down.
  if (countFenceLines(value.slice(0, ls)) % 2 === 1) return null;
  if (countFenceLines(value.slice(le + 1)) % 2 === 1) return null;
  const indent = match[1];
  const caret = selStart + 1 + indent.length;
  return {
    from: selStart,
    to: selStart,
    insert: `\n${indent}\n${indent}\`\`\``,
    selStart: caret,
    selEnd: caret,
  };
}

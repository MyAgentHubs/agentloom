import { describe, expect, it } from "vitest";
import {
  autoCloseFence,
  continueList,
  indentListLine,
  toggleWrap,
  type Edit,
} from "./markdownEdit";

// Input notation: "|" is a caret, "«" and "»" delimit a selection.
function parse(src: string): { value: string; s: number; e: number } {
  const caret = src.indexOf("|");
  if (caret >= 0) {
    const value = src.slice(0, caret) + src.slice(caret + 1);
    return { value, s: caret, e: caret };
  }
  const s = src.indexOf("«");
  const e = src.indexOf("»") - 1;
  return { value: src.replace("«", "").replace("»", ""), s, e };
}

// Applies an edit and renders the result in the same notation.
function render(value: string, edit: Edit): string {
  const next = value.slice(0, edit.from) + edit.insert + value.slice(edit.to);
  if (edit.selStart === edit.selEnd) {
    return `${next.slice(0, edit.selStart)}|${next.slice(edit.selStart)}`;
  }
  return (
    next.slice(0, edit.selStart) +
    "«" +
    next.slice(edit.selStart, edit.selEnd) +
    "»" +
    next.slice(edit.selEnd)
  );
}

type Fn = (value: string, s: number, e: number) => Edit | null;
function run(fn: Fn, src: string): string | null {
  const { value, s, e } = parse(src);
  const edit = fn(value, s, e);
  return edit ? render(value, edit) : null;
}

const cont: Fn = continueList;
const fence: Fn = autoCloseFence;
const indent: Fn = (v, s, e) => indentListLine(v, s, e, 1);
const outdent: Fn = (v, s, e) => indentListLine(v, s, e, -1);
const bold: Fn = (v, s, e) => toggleWrap(v, s, e, "**");
const code: Fn = (v, s, e) => toggleWrap(v, s, e, "`");

describe("toggleWrap", () => {
  it("wraps a selection and keeps the inner text selected", () => {
    expect(run(bold, "a «hi» b")).toBe("a **«hi»** b");
    expect(run(code, "a «hi» b")).toBe("a `«hi»` b");
  });
  it("inserts an empty pair with the caret in the middle", () => {
    expect(run(bold, "a |")).toBe("a **|**");
    expect(run(code, "|")).toBe("`|`");
  });
  it("unwraps when the markers surround the selection", () => {
    expect(run(bold, "a **«hi»** b")).toBe("a «hi» b");
    expect(run(code, "a `«hi»` b")).toBe("a «hi» b");
  });
  it("unwraps when the selection itself includes the markers", () => {
    expect(run(bold, "a «**hi**» b")).toBe("a «hi» b");
  });
  it("never leaves stray markers when the selection spans several wrapped runs", () => {
    expect(run(bold, "«**a** and **b**»")).toBe("**«**a** and **b**»**");
    expect(run(code, "«`a` and `b`»")).toBe("`«`a` and `b`»`");
    expect(run(bold, "**«a** and **b»**")).toBe("****«a** and **b»****");
  });
  it("wraps a selection that holds only one side of a marker", () => {
    expect(run(bold, "«**a»")).toBe("**«**a»**");
    expect(run(bold, "«a**»")).toBe("**«a**»**");
    expect(run(code, "«`a»")).toBe("`«`a»`");
  });
  it("removes an empty pair when the caret sits inside it", () => {
    expect(run(bold, "a **|** b")).toBe("a | b");
  });
  it("does not treat a single backtick pair as bold markers", () => {
    expect(run(bold, "`«hi»`")).toBe("`**«hi»**`");
  });
  it("counts offsets in UTF-16 units for CJK and emoji", () => {
    expect(run(bold, "你好 «世界😀»！")).toBe("你好 **«世界😀»**！");
    expect(run(bold, "😀|")).toBe("😀**|**");
    expect(run(bold, "你好 **«世界😀»**！")).toBe("你好 «世界😀»！");
  });
  it("wraps a multi-line selection as a whole", () => {
    expect(run(bold, "«a\nb»")).toBe("**«a\nb»**");
  });
});

describe("continueList", () => {
  it("continues an ordered list", () => {
    expect(run(cont, "1. a|")).toBe("1. a\n2. |");
  });
  it("carries multi-digit numbers over", () => {
    expect(run(cont, "9. a|")).toBe("9. a\n10. |");
    expect(run(cont, "99. a|")).toBe("99. a\n100. |");
  });
  it("continues bullet lists with the same marker", () => {
    expect(run(cont, "- a|")).toBe("- a\n- |");
    expect(run(cont, "* a|")).toBe("* a\n* |");
  });
  it("keeps the indentation", () => {
    expect(run(cont, "  - a|")).toBe("  - a\n  - |");
    expect(run(cont, "    3. a|")).toBe("    3. a\n    4. |");
  });
  it("works on a later line of a multi-line value", () => {
    expect(run(cont, "intro\n1. a\n2. b|")).toBe("intro\n1. a\n2. b\n3. |");
    expect(run(cont, "1. a|\nafter")).toBe("1. a\n2. |\nafter");
  });
  it("moves the text after the caret onto the new item", () => {
    expect(run(cont, "1. ab|cd")).toBe("1. ab\n2. |cd");
  });
  it("exits the list on an empty item", () => {
    expect(run(cont, "1. a\n2. |")).toBe("1. a\n|");
    expect(run(cont, "- |")).toBe("|");
    expect(run(cont, "  * |")).toBe("|");
  });
  it("replaces a single-line selection with the line break", () => {
    expect(run(cont, "1. a«bc»d")).toBe("1. a\n2. |d");
  });
  it("handles CJK and emoji content", () => {
    expect(run(cont, "1. 你好😀|")).toBe("1. 你好😀\n2. |");
    expect(run(cont, "你好\n- 世界|")).toBe("你好\n- 世界\n- |");
  });
  it("exits the list when text follows the empty item", () => {
    expect(run(cont, "- |\nnext")).toBe("|\nnext");
    expect(run(cont, "1. a\n2. |\nafter\nmore")).toBe("1. a\n|\nafter\nmore");
  });
  it("continues and exits task list items", () => {
    expect(run(cont, "- [ ] a|")).toBe("- [ ] a\n- [ ] |");
    expect(run(cont, "- [x] a|")).toBe("- [x] a\n- [ ] |");
    expect(run(cont, "* [X] a|")).toBe("* [X] a\n* [ ] |");
    expect(run(cont, "1. [ ] a|")).toBe("1. [ ] a\n2. [ ] |");
    expect(run(cont, "  - [ ] a|")).toBe("  - [ ] a\n  - [ ] |");
    expect(run(cont, "- [ ] |")).toBe("|");
    expect(run(cont, "- [x] |")).toBe("|");
    expect(run(cont, "1. [ ] |")).toBe("|");
  });
  it("treats a link-like bracket after a bullet as plain item text", () => {
    expect(run(cont, "- [link](u)|")).toBe("- [link](u)\n- |");
    expect(run(cont, "- [ab] c|")).toBe("- [ab] c\n- |");
  });
  it("returns null inside a fenced code block", () => {
    expect(run(cont, "```yaml\n- old|")).toBeNull();
    expect(run(cont, "```yaml\n- |")).toBeNull();
    expect(run(cont, "```yaml\n1. old|")).toBeNull();
    expect(run(cont, "```yaml\n1. |")).toBeNull();
    expect(run(cont, "```\nx\n```\n- a|")).toBe("```\nx\n```\n- a\n- |");
  });
  it("returns null for plain text lines", () => {
    expect(run(cont, "hello|")).toBeNull();
    expect(run(cont, "")).toBeNull();
    expect(run(cont, "|")).toBeNull();
  });
  it("does not mistake decimals or bare markers for lists", () => {
    expect(run(cont, "1.5 倍|")).toBeNull();
    expect(run(cont, "3.14|")).toBeNull();
    expect(run(cont, "1.|")).toBeNull();
    expect(run(cont, "-a|")).toBeNull();
    expect(run(cont, "**bold**|")).toBeNull();
    expect(run(cont, "-- x|")).toBeNull();
  });
  it("returns null for a multi-line selection", () => {
    expect(run(cont, "1. «a\n2. b»")).toBeNull();
  });
  it("returns null when the caret is inside the prefix", () => {
    expect(run(cont, "1|. a")).toBeNull();
    expect(run(cont, "|- a")).toBeNull();
  });
});

describe("indentListLine", () => {
  it("indents a list line by two spaces and shifts the caret", () => {
    expect(run(indent, "- a|")).toBe("  - a|");
    expect(run(indent, "1. a|b")).toBe("  1. a|b");
  });
  it("indents at any caret position on the line", () => {
    expect(run(indent, "|- a")).toBe("  |- a");
  });
  it("only touches the line holding the caret", () => {
    expect(run(indent, "- a\n- b|\n- c")).toBe("- a\n  - b|\n- c");
  });
  it("outdents by up to two spaces", () => {
    expect(run(outdent, "    - a|")).toBe("  - a|");
    expect(run(outdent, "  - a|")).toBe("- a|");
    expect(run(outdent, " - a|")).toBe("- a|");
  });
  it("reports a no-op edit when a list line has nothing to outdent", () => {
    expect(run(outdent, "- a|")).toBe("- a|");
    expect(run(outdent, "1. «a»")).toBe("1. «a»");
  });
  it("returns null when there is nothing to outdent", () => {
    expect(run(outdent, "plain text|")).toBeNull();
    expect(run(outdent, "1.5 倍|")).toBeNull();
  });
  it("does not report a no-op for multi-line selections or plain lines", () => {
    expect(run(outdent, "«- a\n- b»")).toBeNull();
    expect(run(outdent, "a|")).toBeNull();
  });
  it("returns null for non-list lines", () => {
    expect(run(indent, "hello|")).toBeNull();
    expect(run(indent, "1.5 倍|")).toBeNull();
    expect(run(outdent, "  hello|")).toBeNull();
  });
  it("returns null for a multi-line selection", () => {
    expect(run(indent, "«- a\n- b»")).toBeNull();
  });
  it("keeps a selection inside the line selected", () => {
    expect(run(indent, "- «a»b")).toBe("  - «a»b");
  });
});

describe("autoCloseFence", () => {
  it("closes an opening fence and parks the caret on the blank line", () => {
    expect(run(fence, "```|")).toBe("```\n|\n```");
  });
  it("keeps a language tag on the opening line", () => {
    expect(run(fence, "```ts|")).toBe("```ts\n|\n```");
  });
  it("closes after an earlier, already balanced fence", () => {
    expect(run(fence, "```\nx\n```\ntext\n```py|")).toBe(
      "```\nx\n```\ntext\n```py\n|\n```",
    );
  });
  it("does not add a second closing fence to a closed block", () => {
    expect(run(fence, "```ts|\ncode\n```")).toBeNull();
  });
  it("treats the second fence in a document as a closer", () => {
    expect(run(fence, "```ts\ncode\n```|")).toBeNull();
  });
  it("closes an opening fence that is followed by more text", () => {
    expect(run(fence, "```ts|\nmore text")).toBe("```ts\n|\n```\nmore text");
  });
  it("returns null when the fence is not at the line start", () => {
    expect(run(fence, "use ```|")).toBeNull();
    expect(run(fence, "a ```ts|")).toBeNull();
  });
  it("returns null for inline code and backticks in the info string", () => {
    expect(run(fence, "`code`|")).toBeNull();
    expect(run(fence, "```a`b|")).toBeNull();
  });
  it("returns null when the caret is not at the end of the line", () => {
    expect(run(fence, "```|ts")).toBeNull();
  });
  it("returns null with a selection", () => {
    expect(run(fence, "```«ts»")).toBeNull();
  });
  it("returns null for plain text", () => {
    expect(run(fence, "hello|")).toBeNull();
  });
  it("counts UTF-16 offsets correctly after CJK and emoji lines", () => {
    expect(run(fence, "你好😀\n```js|")).toBe("你好😀\n```js\n|\n```");
  });
});

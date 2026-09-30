import { afterEach, describe, expect, it, vi } from "vitest";
import { insertText } from "./insertText";
import type { Edit } from "./markdownEdit";

const doc = document as unknown as { execCommand?: unknown };

function makeTextarea(value: string): HTMLTextAreaElement {
  const el = document.createElement("textarea");
  el.value = value;
  document.body.appendChild(el);
  return el;
}

// Mimics a browser's insertText: edits the selected range and fires input.
function browserExec(el: HTMLTextAreaElement) {
  return vi.fn((cmd: string, _ui?: boolean, text?: string) => {
    el.setRangeText(
      cmd === "delete" ? "" : (text ?? ""),
      el.selectionStart,
      el.selectionEnd,
      "end",
    );
    el.dispatchEvent(new Event("input", { bubbles: true }));
    return true;
  });
}

const edit = (over: Partial<Edit> = {}): Edit => ({
  from: 1,
  to: 3,
  insert: "XY",
  selStart: 2,
  selEnd: 4,
  ...over,
});

afterEach(() => {
  delete doc.execCommand;
  document.body.innerHTML = "";
});

describe("insertText", () => {
  it("routes through execCommand so the native undo stack is kept", () => {
    const el = makeTextarea("abcdef");
    const exec = browserExec(el);
    doc.execCommand = exec;
    const onInput = vi.fn();
    el.addEventListener("input", onInput);
    insertText(el, edit());
    expect(exec).toHaveBeenCalledWith("insertText", false, "XY");
    // The browser fires the input event itself; no manual one on top of it.
    expect(onInput).toHaveBeenCalledTimes(1);
    expect(el.value).toBe("aXYdef");
    expect(el.selectionStart).toBe(2);
    expect(el.selectionEnd).toBe(4);
  });

  it("selects the replaced range before running the command", () => {
    let seen: [number, number] = [-1, -1];
    doc.execCommand = () => {
      seen = [el.selectionStart, el.selectionEnd];
      return true;
    };
    const el = makeTextarea("abcdef");
    insertText(el, edit());
    expect(seen).toEqual([1, 3]);
  });

  it("uses the delete command for a pure removal", () => {
    const el = makeTextarea("abcdef");
    const exec = browserExec(el);
    doc.execCommand = exec;
    insertText(el, edit({ insert: "", selStart: 1, selEnd: 1 }));
    expect(exec).toHaveBeenCalledWith("delete");
    expect(el.value).toBe("adef");
  });

  it("does nothing for an empty edit", () => {
    const exec = vi.fn(() => true);
    doc.execCommand = exec;
    const el = makeTextarea("abc");
    insertText(el, edit({ from: 1, to: 1, insert: "" }));
    expect(exec).not.toHaveBeenCalled();
    expect(el.value).toBe("abc");
  });

  it("falls back to setRangeText plus a bubbling input event when execCommand is missing", () => {
    const el = makeTextarea("abcdef");
    const onInput = vi.fn();
    document.body.addEventListener("input", onInput);
    insertText(el, edit());
    expect(el.value).toBe("aXYdef");
    expect(onInput).toHaveBeenCalledTimes(1);
    expect(el.selectionStart).toBe(2);
    expect(el.selectionEnd).toBe(4);
    document.body.removeEventListener("input", onInput);
  });

  it("falls back when execCommand returns false", () => {
    doc.execCommand = vi.fn(() => false);
    const el = makeTextarea("abcdef");
    const onInput = vi.fn();
    el.addEventListener("input", onInput);
    insertText(el, edit());
    expect(el.value).toBe("aXYdef");
    expect(onInput).toHaveBeenCalledTimes(1);
  });

  it("repairs a result that execCommand reports as done but got wrong", () => {
    const el = makeTextarea("abcdef");
    // Simulates smart-delete style engines: success, but a different text.
    doc.execCommand = vi.fn(() => {
      el.value = "garbled";
      el.dispatchEvent(new Event("input", { bubbles: true }));
      return true;
    });
    const seen: string[] = [];
    el.addEventListener("input", () => seen.push(el.value));
    insertText(el, edit());
    expect(el.value).toBe("aXYdef");
    // The last input event carries the corrected text, so listeners end in sync.
    expect(seen[seen.length - 1]).toBe("aXYdef");
    expect([el.selectionStart, el.selectionEnd]).toEqual([2, 4]);
  });

  it("falls back when execCommand throws", () => {
    doc.execCommand = () => {
      throw new Error("unsupported");
    };
    const el = makeTextarea("abcdef");
    insertText(el, edit());
    expect(el.value).toBe("aXYdef");
  });
});

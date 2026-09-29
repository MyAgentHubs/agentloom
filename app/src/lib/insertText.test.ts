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
    const exec = vi.fn(() => true);
    doc.execCommand = exec;
    const el = makeTextarea("abcdef");
    const onInput = vi.fn();
    el.addEventListener("input", onInput);
    insertText(el, edit());
    expect(exec).toHaveBeenCalledWith("insertText", false, "XY");
    // The browser fires the input event itself; no manual one on top of it.
    expect(onInput).not.toHaveBeenCalled();
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
    const exec = vi.fn(() => true);
    doc.execCommand = exec;
    const el = makeTextarea("abcdef");
    insertText(el, edit({ insert: "", selStart: 1, selEnd: 1 }));
    expect(exec).toHaveBeenCalledWith("delete");
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

  it("falls back when execCommand throws", () => {
    doc.execCommand = () => {
      throw new Error("unsupported");
    };
    const el = makeTextarea("abcdef");
    insertText(el, edit());
    expect(el.value).toBe("aXYdef");
  });
});

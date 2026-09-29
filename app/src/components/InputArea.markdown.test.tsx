import { fireEvent, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { InputArea } from "./InputArea";
import { I18nProvider } from "../i18n";
import type { Mode } from "./ModeDropdown";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/plugin-dialog", () => ({ open: vi.fn() }));

const doc = document as unknown as { execCommand?: unknown };

beforeEach(() => {
  localStorage.clear();
});

afterEach(() => {
  delete doc.execCommand;
  vi.restoreAllMocks();
});

function setup() {
  const onSend = vi.fn();
  render(
    <I18nProvider initialLocale="zh">
      <InputArea
        composerBusy={false}
        running={false}
        memberRunning={false}
        agentId="claude"
        onAgentChange={() => {}}
        mode={"normal" as Mode}
        onModeChange={() => {}}
        onSend={onSend}
        onStop={() => {}}
      />
    </I18nProvider>,
  );
  const ta = screen.getByPlaceholderText(/输入消息/) as HTMLTextAreaElement;
  // Types `value` and puts the selection at [from, to] (defaults to the end).
  const type = (value: string, from = value.length, to = from) => {
    fireEvent.change(ta, { target: { value } });
    ta.setSelectionRange(from, to);
  };
  return { ta, onSend, type };
}

const shiftEnter = { key: "Enter", shiftKey: true };

describe("composer Markdown keys", () => {
  it("Shift+Enter continues an ordered list item", () => {
    const { ta, onSend, type } = setup();
    type("1. a");
    // fireEvent returns false when the handler called preventDefault.
    expect(fireEvent.keyDown(ta, shiftEnter)).toBe(false);
    expect(ta.value).toBe("1. a\n2. ");
    expect(ta.selectionStart).toBe(ta.value.length);
    expect(onSend).not.toHaveBeenCalled();
  });

  it("Shift+Enter on an empty item leaves the list", () => {
    const { ta, type } = setup();
    type("1. a\n2. ");
    fireEvent.keyDown(ta, shiftEnter);
    expect(ta.value).toBe("1. a\n");
  });

  it("Shift+Enter auto-closes an opening code fence", () => {
    const { ta, type } = setup();
    type("```ts");
    fireEvent.keyDown(ta, shiftEnter);
    expect(ta.value).toBe("```ts\n\n```");
    expect(ta.selectionStart).toBe("```ts\n".length);
  });

  it("Shift+Enter on a plain line keeps the native line break", () => {
    const { ta, type } = setup();
    type("hello");
    expect(fireEvent.keyDown(ta, shiftEnter)).toBe(true);
    expect(ta.value).toBe("hello");
  });

  it("Enter still sends inside a list and does not continue it", () => {
    const { ta, onSend, type } = setup();
    type("1. a");
    fireEvent.keyDown(ta, { key: "Enter" });
    expect(onSend).toHaveBeenCalledTimes(1);
    expect(onSend.mock.calls[0][0]).toBe("1. a");
  });

  it("Cmd+B wraps the selection and the draft state follows", () => {
    const { ta, onSend, type } = setup();
    type("hello", 0, 5);
    expect(fireEvent.keyDown(ta, { key: "b", metaKey: true })).toBe(false);
    expect(ta.value).toBe("**hello**");
    expect([ta.selectionStart, ta.selectionEnd]).toEqual([2, 7]);
    // Sending proves React state saw the programmatic edit.
    fireEvent.keyDown(ta, { key: "Enter" });
    expect(onSend.mock.calls[0][0]).toBe("**hello**");
  });

  it("Ctrl+B and Cmd+E work; a second Cmd+B toggles back", () => {
    const { ta, type } = setup();
    type("hi", 0, 2);
    fireEvent.keyDown(ta, { key: "e", metaKey: true });
    expect(ta.value).toBe("`hi`");
    type("x", 0, 1);
    fireEvent.keyDown(ta, { key: "b", ctrlKey: true });
    expect(ta.value).toBe("**x**");
    fireEvent.keyDown(ta, { key: "b", ctrlKey: true });
    expect(ta.value).toBe("x");
  });

  it("Cmd+B with no selection inserts an empty pair around the caret", () => {
    const { ta, type } = setup();
    type("");
    fireEvent.keyDown(ta, { key: "b", metaKey: true });
    expect(ta.value).toBe("****");
    expect(ta.selectionStart).toBe(2);
  });

  it("leaves Alt combinations and Ctrl+Shift+B alone", () => {
    const { ta, type } = setup();
    type("hello", 0, 5);
    expect(
      fireEvent.keyDown(ta, { key: "b", metaKey: true, altKey: true }),
    ).toBe(true);
    expect(
      fireEvent.keyDown(ta, { key: "b", ctrlKey: true, shiftKey: true }),
    ).toBe(true);
    expect(ta.value).toBe("hello");
  });

  it("does not hijack Ctrl+B or Ctrl+E on macOS (native cursor movement)", () => {
    vi.spyOn(navigator, "platform", "get").mockReturnValue("MacIntel");
    const { ta, type } = setup();
    type("hello", 0, 5);
    expect(fireEvent.keyDown(ta, { key: "b", ctrlKey: true })).toBe(true);
    expect(fireEvent.keyDown(ta, { key: "e", ctrlKey: true })).toBe(true);
    expect(ta.value).toBe("hello");
    expect(fireEvent.keyDown(ta, { key: "b", metaKey: true })).toBe(false);
    expect(ta.value).toBe("**hello**");
  });

  it("Tab indents a list line and Shift+Tab outdents it", () => {
    const { ta, type } = setup();
    type("- a");
    expect(fireEvent.keyDown(ta, { key: "Tab" })).toBe(false);
    expect(ta.value).toBe("  - a");
    fireEvent.keyDown(ta, { key: "Tab", shiftKey: true });
    expect(ta.value).toBe("- a");
  });

  it("Tab on a plain line is not intercepted", () => {
    const { ta, type } = setup();
    type("hello");
    expect(fireEvent.keyDown(ta, { key: "Tab" })).toBe(true);
    expect(fireEvent.keyDown(ta, { key: "Tab", shiftKey: true })).toBe(true);
    expect(ta.value).toBe("hello");
  });

  it("does nothing during IME composition (isComposing, keyCode 229, compositionstart)", () => {
    const { ta, onSend, type } = setup();
    type("1. a");
    const keys = [
      { ...shiftEnter },
      { key: "Tab" },
      { key: "b", metaKey: true },
    ];
    for (const key of keys) {
      expect(fireEvent.keyDown(ta, { ...key, isComposing: true })).toBe(true);
      expect(fireEvent.keyDown(ta, { ...key, keyCode: 229 })).toBe(true);
    }
    fireEvent.compositionStart(ta);
    for (const key of keys) {
      expect(fireEvent.keyDown(ta, key)).toBe(true);
    }
    expect(ta.value).toBe("1. a");
    expect(onSend).not.toHaveBeenCalled();
    fireEvent.compositionEnd(ta);
    expect(fireEvent.keyDown(ta, shiftEnter)).toBe(false);
    expect(ta.value).toBe("1. a\n2. ");
  });

  it("uses execCommand when available, so the browser owns the edit and undo", () => {
    const exec = vi.fn(() => true);
    doc.execCommand = exec;
    const { ta, type } = setup();
    type("1. a");
    fireEvent.keyDown(ta, shiftEnter);
    expect(exec).toHaveBeenCalledWith("insertText", false, "\n2. ");
    // The stub did not edit anything: no manual setRangeText on this path.
    expect(ta.value).toBe("1. a");
  });

  it("falls back to setRangeText and an input event when execCommand is missing", () => {
    const { ta, onSend, type } = setup();
    expect(typeof document.execCommand).not.toBe("function");
    const onInput = vi.fn();
    ta.addEventListener("input", onInput);
    type("1. a");
    onInput.mockClear();
    fireEvent.keyDown(ta, shiftEnter);
    expect(onInput).toHaveBeenCalledTimes(1);
    fireEvent.keyDown(ta, { key: "Enter" });
    // Sending trims trailing whitespace.
    expect(onSend.mock.calls[0][0]).toBe("1. a\n2.");
  });
});

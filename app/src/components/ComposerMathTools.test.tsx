import { useRef, useState } from "react";
import { fireEvent, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it } from "vitest";
import { ComposerMathTools } from "./ComposerMathTools";
import { useComposerMarkdownKeys } from "../hooks/useComposerMarkdownKeys";

function Editor({ initial = "", disabled = false }) {
  const textarea = useRef<HTMLTextAreaElement>(null);
  const composing = useRef(false);
  const [value, setValue] = useState(initial);
  const keys = useComposerMarkdownKeys();
  return (
    <>
      <textarea
        ref={textarea}
        aria-label="draft"
        value={value}
        disabled={disabled}
        onChange={(event) => setValue(event.target.value)}
        onCompositionStart={() => {
          composing.current = true;
        }}
        onCompositionEnd={() => {
          composing.current = false;
        }}
        onKeyDown={(event) =>
          keys(
            event,
            composing.current ||
              event.nativeEvent.isComposing ||
              event.keyCode === 229,
          )
        }
      />
      <ComposerMathTools
        textarea={textarea}
        composing={composing}
        disabled={disabled}
      />
    </>
  );
}

describe("composer formula commands", () => {
  it.each([
    ["行内公式", "a \\(x^2\\) z", 4],
    ["独立公式", "a \n\\[\nx^2\n\\]\n z", 6],
  ])("wraps the selection using %s", (label, expected, caret) => {
    render(<Editor initial="a x^2 z" />);
    const el = screen.getByRole("textbox") as HTMLTextAreaElement;
    el.focus();
    el.setSelectionRange(2, 5);
    fireEvent.click(screen.getByRole("button", { name: "插入公式" }));
    fireEvent.click(screen.getByRole("button", { name: new RegExp(label) }));
    expect(el.value).toBe(expected);
    expect([el.selectionStart, el.selectionEnd]).toEqual([caret, caret + 3]);
    expect(el).toHaveFocus();
  });

  it.each([
    ["m", "\\(\\)", 2],
    ["e", "\\[\n\n\\]", 3],
  ])(
    "places an empty selection inside delimiters with shortcut %s",
    (key, expected, caret) => {
      render(<Editor />);
      const el = screen.getByRole("textbox") as HTMLTextAreaElement;
      fireEvent.keyDown(el, { key, metaKey: true, shiftKey: true });
      expect(el.value).toBe(expected);
      expect([el.selectionStart, el.selectionEnd]).toEqual([caret, caret]);
    },
  );

  it("does not intercept IME composition, then accepts the committed selection", () => {
    render(<Editor initial="公式" />);
    const el = screen.getByRole("textbox") as HTMLTextAreaElement;
    fireEvent.compositionStart(el);
    fireEvent.keyDown(el, { key: "m", metaKey: true, shiftKey: true });
    fireEvent.click(screen.getByRole("button", { name: "插入公式" }));
    expect(el.value).toBe("公式");
    expect(screen.queryByText("公式示例（源码与预览）")).toBeNull();
    fireEvent.compositionEnd(el);
    el.setSelectionRange(0, 2);
    fireEvent.keyDown(el, { key: "m", metaKey: true, shiftKey: true });
    expect(el.value).toBe("\\(公式\\)");
  });

  it("shows product examples, supports keyboard access and dismisses with Escape", async () => {
    const user = userEvent.setup();
    const { container } = render(<Editor />);
    const trigger = screen.getByRole("button", { name: "插入公式" });
    trigger.focus();
    await user.keyboard("{Enter}{Tab}");
    expect(screen.getByRole("button", { name: /行内公式/ })).toHaveFocus();
    await user.click(screen.getByText("公式示例（源码与预览）"));
    expect(container.querySelectorAll(".katex")).toHaveLength(5);
    expect(container.querySelector(".katex-error")).not.toBeNull();
    await user.keyboard("{Escape}");
    expect(trigger).toHaveFocus();
    expect(trigger).toHaveAttribute("aria-expanded", "false");
  });

  it("keeps formula tools disabled for a readonly composer", () => {
    render(<Editor disabled />);
    expect(screen.getByRole("button", { name: "插入公式" })).toBeDisabled();
  });
});

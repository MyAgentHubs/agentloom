import { render } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { MarkdownBody } from "./MarkdownBody";

describe("math streaming and input boundaries", () => {
  it.each([
    "$x^2$",
    "$$x^2$$",
    "$$\nx^2\n$$",
    String.raw`\(\frac{1}{2}\)`,
    String.raw`\[\frac{1}{2}\]`,
  ])("waits for closure at every possible chunk boundary: %s", (formula) => {
    const prefix = formula.includes("\n") ? "before\n\n" : "before ";
    const suffix = formula.includes("\n") ? "\n\nafter" : " after";
    const { container, rerender } = render(
      <MarkdownBody streaming>{prefix}</MarkdownBody>,
    );
    for (let end = 1; end < formula.length; end++) {
      rerender(
        <MarkdownBody
          streaming
        >{`${prefix}${formula.slice(0, end)}`}</MarkdownBody>,
      );
      expect(container.querySelector(".katex"), `prefix ${end}`).toBeNull();
      expect(container.textContent).toContain("before");
    }
    const source = `${prefix}${formula}${suffix}`;
    rerender(<MarkdownBody streaming>{source}</MarkdownBody>);
    const streamed = container.innerHTML;
    const full = render(
      <MarkdownBody streaming={false}>{source}</MarkdownBody>,
    );
    expect(streamed).toBe(full.container.innerHTML);
    expect(container.textContent).toContain("after");
    expect(container.querySelectorAll(".katex")).toHaveLength(1);
  });

  it("waits for the explicit block closer across blank lines", () => {
    const source = "before\n\n$$\nx^2\n\nAFTER normal prose";
    const { container, rerender } = render(
      <MarkdownBody streaming>{source}</MarkdownBody>,
    );
    expect(container.querySelector(".katex")).toBeNull();
    expect(container.textContent).toContain("AFTER normal prose");
    rerender(
      <MarkdownBody streaming>{source + "\n$$\n\nafter closure"}</MarkdownBody>,
    );
    expect(container.querySelector(".katex-display")).not.toBeNull();
    expect(container.textContent).toContain("after closure");
  });

  it.each([
    String.raw`\$5 and \$10; \\(x\)`,
    "`$x$` and `\\[x\\]`",
    "```tex\n$$x$$\n\\(x\\)\n```",
    "[link](https://example.com/$x$/%5C(x%5C))",
    "[array, value]",
    "$ x$ and $x $",
    "$5.00 and $10.00",
    "Cost $5\r\nNext $10.",
  ])("leaves non-math input alone: %s", (source) => {
    const { container } = render(
      <MarkdownBody streaming={false}>{source}</MarkdownBody>,
    );
    expect(container.querySelector(".katex")).toBeNull();
  });

  it.each([
    String.raw`$\htmlClass{injected}{x}$`,
    String.raw`$\href{javascript:alert(1)}{click}$`,
    String.raw`$\includegraphics{https://example.com/tracker.png}$`,
    String.raw`$\def\a{\a}\a$`,
  ])("contains untrusted or excessive commands: %s", (source) => {
    const { container } = render(
      <MarkdownBody streaming={false}>{`before ${source} after`}</MarkdownBody>,
    );
    expect(container.querySelector("a, img, .injected")).toBeNull();
    expect(container.textContent).toContain("before");
    expect(container.textContent).toContain("after");
  });

  it("preserves block math inside list and quote containers", () => {
    const { container } = render(
      <MarkdownBody streaming={false}>
        {"> $$\n> x^2\n> $$\n\n- $$\n  y^2\n  $$"}
      </MarkdownBody>,
    );
    expect(container.querySelectorAll(".katex-display")).toHaveLength(2);
  });
});

import { render } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { MarkdownBody } from "./MarkdownBody";

const formula = String.raw`D_{\mathrm{KL}}(P \parallel Q) \neq D_{\mathrm{KL}}(Q \parallel P)`;

describe("chat math", () => {
  it.each([
    [`$${formula}$`, false],
    [`$$\n${formula}\n$$`, true],
    [String.raw`\(${formula}\)`, false],
    [String.raw`\[${formula}\]`, true],
    [String.raw`\[\begin{aligned}a &= b \\ c &= d\end{aligned}\]`, true],
  ])("typesets %s", (source, display) => {
    const { container } = render(
      <MarkdownBody streaming={false}>{source}</MarkdownBody>,
    );
    expect(container.querySelector(".katex")).not.toBeNull();
    expect(container.querySelector(".katex-error")).toBeNull();
    expect(!!container.querySelector(".katex-display")).toBe(display);
    expect(container.querySelector("annotation")?.textContent).toBe(
      source.includes("aligned")
        ? String.raw`\begin{aligned}a &= b \\ c &= d\end{aligned}`
        : formula,
    );
  });

  it.each([
    "[1, 2, 3] and [ordinary brackets]",
    String.raw`Price: $5. Escaped prices: \$5 and \$10.`,
    "\\\\(x\\)",
    "`\\(x\\)` and `$x$`",
    "```tex\n\\[x\\]\n$x$\n```",
    "    \\[x\\]\n",
    "[link](https://example.com/$x$)",
  ])("preserves non-math content: %s", (source) => {
    const { container } = render(
      <MarkdownBody streaming={false}>{source}</MarkdownBody>,
    );
    expect(container.querySelector(".katex")).toBeNull();
  });

  it("keeps incomplete streamed math readable, then typesets the complete formula", () => {
    const { container, rerender } = render(
      <MarkdownBody streaming>{"prefix \\[D_"}</MarkdownBody>,
    );
    expect(container.textContent).toContain("prefix");
    expect(container.querySelector(".katex")).toBeNull();
    const source = `prefix \\[${formula}\\] suffix`;
    rerender(<MarkdownBody streaming>{source}</MarkdownBody>);
    expect(container.querySelector(".katex")).not.toBeNull();
    rerender(<MarkdownBody streaming={false}>{source}</MarkdownBody>);
    expect(container.querySelectorAll(".katex")).toHaveLength(1);
    expect(container.textContent).toContain("suffix");
  });

  it("contains errors locally and does not enable trusted HTML commands", () => {
    const { container } = render(
      <MarkdownBody streaming={false}>
        {String.raw`before $\frac{$ after $\htmlClass{injected}{x}$`}
      </MarkdownBody>,
    );
    expect(container.textContent).toContain("before");
    expect(container.textContent).toContain("after");
    expect(container.querySelector(".katex-error")).not.toBeNull();
    expect(container.querySelector(".injected")).toBeNull();
  });
});

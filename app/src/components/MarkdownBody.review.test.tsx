import { render } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { MarkdownBody } from "./MarkdownBody";

describe("math review regressions", () => {
  it("bounds rule dimensions", () => {
    const { container } = render(
      <MarkdownBody
        streaming={false}
      >{String.raw`$\rule{10000em}{10000em}$`}</MarkdownBody>,
    );
    expect(container.querySelector(".katex")).not.toBeNull();
    expect(container.innerHTML).not.toMatch(
      /(?:height|width|bottom):\s*10000em/,
    );
  });

  it.each(["Price: $5 and $10.", "Cost $5\nNext cost $10."])(
    "preserves currency: %s",
    (source) => {
      const { container } = render(
        <MarkdownBody streaming={false}>{source}</MarkdownBody>,
      );
      expect(container.querySelector(".katex")).toBeNull();
      expect(container.textContent).toBe(source);
    },
  );

  it.each(["$$x^2$$", "$$\nx^2\n$$"])(
    "displays double dollars: %s",
    (source) => {
      const { container } = render(
        <MarkdownBody streaming={false}>{source}</MarkdownBody>,
      );
      expect(container.querySelector(".katex-display")).not.toBeNull();
    },
  );

  it.each([true, false])(
    "leaves an unclosed block readable (streaming=%s)",
    (streaming) => {
      const source = "before\n\n$$\nx^2\n\nAFTER normal prose";
      const { container } = render(
        <MarkdownBody streaming={streaming}>{source}</MarkdownBody>,
      );
      expect(container.querySelector(".katex")).toBeNull();
      expect(container.textContent).toContain("$$\nx^2\n\nAFTER normal prose");
    },
  );

  it("keeps numeric math and valid math after prices", () => {
    const { container } = render(
      <MarkdownBody streaming={false}>
        {"$2$; price $5 and $10; $x$"}
      </MarkdownBody>,
    );
    expect(
      [...container.querySelectorAll("annotation")].map((el) => el.textContent),
    ).toEqual(["2", "x"]);
    expect(container.textContent).toContain("price $5 and $10");
  });

  it("bounds repeated failed delimiter scans during streaming", () => {
    const source = String.raw`\(x `.repeat(8000);
    const start = performance.now();
    const { container, rerender } = render(
      <MarkdownBody streaming>{source}</MarkdownBody>,
    );
    rerender(<MarkdownBody streaming>{source + " more"}</MarkdownBody>);
    expect(container.querySelector(".katex")).toBeNull();
    // A generous ceiling for 64 KB total input; the quadratic implementation takes seconds.
    expect(performance.now() - start).toBeLessThan(1500);
  });
});

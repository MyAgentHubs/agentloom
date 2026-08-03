import { describe, expect, it } from "vitest";
import { deriveSessionTitle } from "./sessionTitle";

describe("deriveSessionTitle", () => {
  it("trims and truncates plain text to 30 characters", () => {
    expect(deriveSessionTitle(`  ${"a".repeat(35)}  `)).toBe("a".repeat(30));
  });

  it("removes angle-bracket image attachment markdown", () => {
    expect(
      deriveSessionTitle(
        "能理解这个图片不 ![附加图片](</Users/dev/Desktop/example.png>)",
      ),
    ).toBe("能理解这个图片不");
  });

  it("removes plain-parentheses image attachment markdown", () => {
    expect(
      deriveSessionTitle("Please explain this ![x](/tmp/example.png)"),
    ).toBe("Please explain this");
  });

  it.each([
    [
      "Attached image: /tmp/example.png\nDescribe this screenshot",
      "Describe this screenshot",
    ],
    [
      "Attached file: /tmp/example.pdf\nSummarize this document",
      "Summarize this document",
    ],
  ])("removes attachment prefix lines from %s", (text, expected) => {
    expect(deriveSessionTitle(text)).toBe(expected);
  });

  it("returns an empty fallback when the message only contains an image", () => {
    expect(deriveSessionTitle("![x](<path/to/image.png>)")).toBe("");
  });
});

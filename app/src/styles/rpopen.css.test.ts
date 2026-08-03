import { describe, it, expect } from "vitest";
// @ts-expect-error - Vitest runs in Node, but this frontend tsconfig has no Node type declarations.
import { readFileSync } from "fs";

describe("②a 消息列两态 CSS", () => {
  it("global.css 含 .surface.rpopen 切 .turn/.composer max-width 规则", () => {
    const css = readFileSync("src/styles/global.css", "utf8");
    expect(css).toMatch(
      /\.surface\.rpopen\s+\.turn\s*\{[^}]*max-width:\s*none/,
    );
    expect(css).toMatch(
      /\.surface\.rpopen\s+\.composer\s*\{[^}]*max-width:\s*none/,
    );
  });
});

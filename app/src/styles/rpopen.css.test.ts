import { describe, it, expect } from "vitest";
import { readGlobalCss } from "./readGlobalCss";

describe("②a 消息列两态 CSS", () => {
  it("global.css 含 .surface.rpopen 切 .turn/.composer max-width 规则", () => {
    const css = readGlobalCss();
    expect(css).toMatch(
      /\.surface\.rpopen\s+\.turn\s*\{[^}]*max-width:\s*none/,
    );
    expect(css).toMatch(
      /\.surface\.rpopen\s+\.composer\s*\{[^}]*max-width:\s*none/,
    );
  });
});

import { describe, expect, it } from "vitest";
import { isReviewPanelVisible, shouldFetchOnSwitch } from "./reviewRefreshGate";

describe("isReviewPanelVisible", () => {
  it("returns true for an open review panel in the session view", () => {
    expect(isReviewPanelVisible("session", true, "review")).toBe(true);
  });

  it("returns false outside the session view", () => {
    expect(isReviewPanelVisible("overview", true, "review")).toBe(false);
  });

  it("returns false when the right panel is closed", () => {
    expect(isReviewPanelVisible("session", false, "review")).toBe(false);
  });

  it("returns false when another right-panel tab is selected", () => {
    expect(isReviewPanelVisible("session", true, "files")).toBe(false);
  });
});

describe("shouldFetchOnSwitch", () => {
  it("returns false for a visible panel without a cached result", () => {
    expect(shouldFetchOnSwitch(true, false)).toBe(false);
  });

  it("returns false for a visible panel with a cached result", () => {
    expect(shouldFetchOnSwitch(true, true)).toBe(false);
  });

  it("returns true for a hidden panel without a cached result", () => {
    expect(shouldFetchOnSwitch(false, false)).toBe(true);
  });

  it("returns false for a hidden panel with a cached result", () => {
    expect(shouldFetchOnSwitch(false, true)).toBe(false);
  });
});

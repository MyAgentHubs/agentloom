import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { loadLastAgentId, saveLastAgentId } from "./agentPrefStore";

describe("agentPrefStore", () => {
  beforeEach(() => {
    localStorage.clear();
  });

  afterEach(() => {
    vi.restoreAllMocks();
    localStorage.clear();
  });

  it("returns null when no agent id has been saved", () => {
    expect(loadLastAgentId()).toBeNull();
  });

  it("saves and loads the last selected agent id", () => {
    saveLastAgentId("codex");

    expect(loadLastAgentId()).toBe("codex");
  });

  it("returns null when localStorage getItem throws", () => {
    vi.spyOn(globalThis.localStorage, "getItem").mockImplementation(() => {
      throw new Error("storage unavailable");
    });

    expect(loadLastAgentId()).toBeNull();
  });

  it("does not throw when localStorage setItem throws", () => {
    vi.spyOn(globalThis.localStorage, "setItem").mockImplementation(() => {
      throw new Error("quota exceeded");
    });

    expect(() => saveLastAgentId("deepseek")).not.toThrow();
  });
});

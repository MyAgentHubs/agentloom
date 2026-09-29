import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  clearDraft,
  discardDraft,
  loadDraft,
  saveDraft,
} from "./composerDraftStore";

describe("composerDraftStore", () => {
  beforeEach(() => localStorage.clear());
  afterEach(() => vi.restoreAllMocks());

  it("round-trips text and attachments per session", () => {
    const att = [{ path: "/tmp/a.png", name: "a.png" }];
    saveDraft("s1", { text: "hello", attachments: att });
    expect(loadDraft("s1")).toEqual({ text: "hello", attachments: att });
    expect(loadDraft("s2")).toEqual({ text: "", attachments: [] });
  });

  it("stores the new-session draft under its own key", () => {
    saveDraft(null, { text: "fresh", attachments: [] });
    expect(localStorage.getItem("agentloom.draft.__new__")).not.toBeNull();
    expect(loadDraft(null).text).toBe("fresh");
    expect(loadDraft("s1").text).toBe("");
  });

  it("removes the key for an empty draft instead of writing an empty value", () => {
    saveDraft("s1", { text: "x", attachments: [] });
    saveDraft("s1", { text: "", attachments: [] });
    expect(localStorage.getItem("agentloom.draft.s1")).toBeNull();
  });

  it("clearDraft removes the stored draft", () => {
    saveDraft("s1", { text: "x", attachments: [] });
    clearDraft("s1");
    expect(loadDraft("s1").text).toBe("");
  });

  it("treats corrupt JSON as no draft", () => {
    localStorage.setItem("agentloom.draft.s1", "{not json");
    expect(loadDraft("s1")).toEqual({ text: "", attachments: [] });
  });

  it("treats a version mismatch as no draft", () => {
    localStorage.setItem(
      "agentloom.draft.s1",
      JSON.stringify({ v: 2, text: "x", attachments: [] }),
    );
    expect(loadDraft("s1").text).toBe("");
  });

  it("drops malformed attachment entries", () => {
    localStorage.setItem(
      "agentloom.draft.s1",
      JSON.stringify({
        v: 1,
        text: "x",
        attachments: [{ path: "/a", name: "a" }, { path: 1 }, null],
      }),
    );
    expect(loadDraft("s1").attachments).toEqual([{ path: "/a", name: "a" }]);
  });

  it("degrades silently when localStorage throws", () => {
    saveDraft("s1", { text: "kept", attachments: [] });
    // Spy on the Storage prototype: the instance may be jsdom's Storage or the test-setup fallback.
    const proto = Object.getPrototypeOf(localStorage);
    const denied = () => {
      throw new Error("denied");
    };
    const spies = (["getItem", "setItem", "removeItem"] as const).map((m) =>
      vi.spyOn(proto, m).mockImplementation(denied),
    );
    expect(() => saveDraft("s1", { text: "x", attachments: [] })).not.toThrow();
    expect(() => clearDraft("s1")).not.toThrow();
    // Empty result although "kept" is stored proves the throwing getItem was actually hit.
    expect(loadDraft("s1")).toEqual({ text: "", attachments: [] });
    spies.forEach((spy) => expect(spy).toHaveBeenCalled());
  });

  it("discardDraft drops the stored draft and refuses later writes for that session", () => {
    saveDraft("gone", { text: "x", attachments: [] });
    discardDraft("gone");
    expect(localStorage.getItem("agentloom.draft.gone")).toBeNull();
    saveDraft("gone", { text: "late flush", attachments: [] });
    expect(localStorage.getItem("agentloom.draft.gone")).toBeNull();
    saveDraft("other", { text: "ok", attachments: [] });
    expect(loadDraft("other").text).toBe("ok");
  });
});

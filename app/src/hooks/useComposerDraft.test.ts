import { act, renderHook } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { useComposerDraft } from "./useComposerDraft";

const KEY = (id: string) => `agentloom.draft.${id}`;

describe("useComposerDraft", () => {
  beforeEach(() => {
    localStorage.clear();
    vi.useFakeTimers();
  });
  afterEach(() => vi.useRealTimers());

  it("loads the stored draft for the session on mount", () => {
    localStorage.setItem(
      KEY("a"),
      JSON.stringify({ v: 1, text: "stored", attachments: [] }),
    );
    const { result } = renderHook(() => useComposerDraft("a"));
    expect(result.current.draft).toBe("stored");
  });

  it("debounces saves and writes only the last value", () => {
    const setItem = vi.spyOn(localStorage, "setItem");
    const { result } = renderHook(() => useComposerDraft("a"));
    act(() => result.current.setDraft("h"));
    act(() => result.current.setDraft("he"));
    act(() => result.current.setDraft("hey"));
    expect(localStorage.getItem(KEY("a"))).toBeNull();
    act(() => {
      vi.advanceTimersByTime(300);
    });
    expect(setItem).toHaveBeenCalledTimes(1);
    expect(JSON.parse(localStorage.getItem(KEY("a")) ?? "{}").text).toBe("hey");
    setItem.mockRestore();
  });

  it("supports functional updates", () => {
    const { result } = renderHook(() => useComposerDraft("a"));
    act(() => {
      result.current.setDraft("a");
      result.current.setDraft((p) => p + "b");
    });
    expect(result.current.draft).toBe("ab");
  });

  it("keeps sessions apart on a quick A to B switch", () => {
    const { result, rerender } = renderHook(
      ({ id }: { id: string | null }) => useComposerDraft(id),
      { initialProps: { id: "a" as string | null } },
    );
    act(() => result.current.setDraft("text for A"));
    rerender({ id: "b" });
    expect(result.current.draft).toBe("");
    act(() => {
      vi.advanceTimersByTime(1000);
    });
    expect(JSON.parse(localStorage.getItem(KEY("a")) ?? "{}").text).toBe(
      "text for A",
    );
    expect(localStorage.getItem(KEY("b"))).toBeNull();
    rerender({ id: "a" });
    expect(result.current.draft).toBe("text for A");
  });

  it("keeps attachments per session", () => {
    const { result, rerender } = renderHook(
      ({ id }: { id: string }) => useComposerDraft(id),
      { initialProps: { id: "a" } },
    );
    act(() => result.current.setAttachments([{ path: "/x/1", name: "1" }]));
    rerender({ id: "b" });
    expect(result.current.attachments).toEqual([]);
    rerender({ id: "a" });
    expect(result.current.attachments).toEqual([{ path: "/x/1", name: "1" }]);
  });

  it("clear empties state and removes the stored key immediately", () => {
    const { result } = renderHook(() => useComposerDraft("a"));
    act(() => result.current.setDraft("x"));
    act(() => {
      vi.advanceTimersByTime(300);
    });
    expect(localStorage.getItem(KEY("a"))).not.toBeNull();
    act(() => result.current.setDraft("y"));
    act(() => result.current.clear());
    expect(result.current.draft).toBe("");
    expect(localStorage.getItem(KEY("a"))).toBeNull();
    act(() => {
      vi.advanceTimersByTime(1000);
    });
    expect(localStorage.getItem(KEY("a"))).toBeNull();
  });

  it("flushes pending text on pagehide", () => {
    const { result } = renderHook(() => useComposerDraft("a"));
    act(() => result.current.setDraft("unsaved"));
    expect(localStorage.getItem(KEY("a"))).toBeNull();
    window.dispatchEvent(new Event("pagehide"));
    expect(JSON.parse(localStorage.getItem(KEY("a")) ?? "{}").text).toBe(
      "unsaved",
    );
  });

  it("flushes pending text on unmount", () => {
    const { result, unmount } = renderHook(() => useComposerDraft("a"));
    act(() => result.current.setDraft("bye"));
    unmount();
    expect(JSON.parse(localStorage.getItem(KEY("a")) ?? "{}").text).toBe("bye");
  });

  it("uses a separate draft for the new-session page", () => {
    const { result } = renderHook(() => useComposerDraft(null));
    act(() => result.current.setDraft("new"));
    window.dispatchEvent(new Event("pagehide"));
    expect(localStorage.getItem("agentloom.draft.__new__")).not.toBeNull();
  });

  it("editFor writes to the origin session's stored draft without touching the live one", () => {
    localStorage.setItem(
      KEY("a"),
      JSON.stringify({ v: 1, text: "old", attachments: [] }),
    );
    const { result } = renderHook(() => useComposerDraft("b"));
    act(() => result.current.setDraft("live"));
    act(() =>
      result.current.editFor("a", (d) => ({ ...d, text: d.text + "+more" })),
    );
    expect(result.current.draft).toBe("live");
    expect(JSON.parse(localStorage.getItem(KEY("a")) ?? "{}").text).toBe(
      "old+more",
    );
  });

  it("clear for another session only drops that stored draft", () => {
    localStorage.setItem(
      KEY("a"),
      JSON.stringify({ v: 1, text: "sent", attachments: [] }),
    );
    const { result } = renderHook(() => useComposerDraft("b"));
    act(() => result.current.setDraft("live"));
    let live = true;
    act(() => {
      live = result.current.clear("a");
    });
    expect(live).toBe(false);
    expect(result.current.draft).toBe("live");
    expect(localStorage.getItem(KEY("a"))).toBeNull();
  });
});

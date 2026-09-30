import { renderHook, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { invoke } from "@tauri-apps/api/core";
import { readModelCache, writeModelCache } from "./agentFormHelpers";
import { useCodexLiveModels } from "./useCodexLiveModels";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

const invokeMock = vi.mocked(invoke);
const info = (slug: unknown) => ({ slug, display_name: "x" });

describe("useCodexLiveModels", () => {
  beforeEach(() => {
    invokeMock.mockReset();
    invokeMock.mockResolvedValue(undefined);
    localStorage.clear();
  });

  it("成功返回时给出 slug 数组（去重保序）并写缓存", async () => {
    invokeMock.mockResolvedValue([info("b"), info("a"), info("b")]);
    const { result } = renderHook(() => useCodexLiveModels(true));
    await waitFor(() => expect(result.current).toEqual(["b", "a"]));
    expect(invokeMock).toHaveBeenCalledWith("list_codex_models");
    expect(readModelCache("codex", "")).toEqual(["b", "a"]);
  });

  it("先同步读缓存作初值，再被新结果覆盖", async () => {
    writeModelCache("codex", "", ["cached"]);
    let resolve!: (v: unknown) => void;
    invokeMock.mockReturnValue(new Promise((r) => (resolve = r)));
    const { result } = renderHook(() => useCodexLiveModels(true));
    expect(result.current).toEqual(["cached"]);
    resolve([info("fresh")]);
    await waitFor(() => expect(result.current).toEqual(["fresh"]));
  });

  it("invoke 失败时保持 null 且不抛错", async () => {
    invokeMock.mockRejectedValue("codex not found");
    const { result } = renderHook(() => useCodexLiveModels(true));
    await waitFor(() => expect(invokeMock).toHaveBeenCalled());
    expect(result.current).toBeNull();
  });

  it("invoke 失败时保持缓存值", async () => {
    writeModelCache("codex", "", ["cached"]);
    invokeMock.mockRejectedValue("boom");
    const { result } = renderHook(() => useCodexLiveModels(true));
    await waitFor(() => expect(invokeMock).toHaveBeenCalled());
    expect(result.current).toEqual(["cached"]);
  });

  it.each([
    ["undefined", undefined],
    ["空数组", []],
    ["非数组", { slug: "a" }],
    ["缺 slug", [{ display_name: "x" }, info(""), info(3), null]],
  ])("返回 %s 时保持当前值", async (_name, payload) => {
    writeModelCache("codex", "", ["cached"]);
    invokeMock.mockResolvedValue(payload);
    const { result } = renderHook(() => useCodexLiveModels(true));
    await waitFor(() => expect(invokeMock).toHaveBeenCalled());
    await Promise.resolve();
    expect(result.current).toEqual(["cached"]);
    expect(readModelCache("codex", "")).toEqual(["cached"]);
  });

  it("混有无效项时只取有效 slug", async () => {
    invokeMock.mockResolvedValue([info("a"), { display_name: "x" }, info("c")]);
    const { result } = renderHook(() => useCodexLiveModels(true));
    await waitFor(() => expect(result.current).toEqual(["a", "c"]));
  });

  it("enabled 为假时返回 null 且不调用 invoke", async () => {
    writeModelCache("codex", "", ["cached"]);
    const { result } = renderHook(() => useCodexLiveModels(false));
    await Promise.resolve();
    expect(result.current).toBeNull();
    expect(invokeMock).not.toHaveBeenCalled();
  });

  it("卸载后到达的结果被丢弃（不写缓存）", async () => {
    let resolve!: (v: unknown) => void;
    invokeMock.mockReturnValue(new Promise((r) => (resolve = r)));
    const { unmount } = renderHook(() => useCodexLiveModels(true));
    unmount();
    resolve([info("late")]);
    await Promise.resolve();
    await Promise.resolve();
    expect(readModelCache("codex", "")).toBeNull();
  });

  it("enabled 变化后到达的旧结果被丢弃", async () => {
    let resolve!: (v: unknown) => void;
    invokeMock.mockReturnValue(new Promise((r) => (resolve = r)));
    const { result, rerender } = renderHook(
      ({ on }) => useCodexLiveModels(on),
      { initialProps: { on: true } },
    );
    rerender({ on: false });
    resolve([info("late")]);
    await Promise.resolve();
    await Promise.resolve();
    expect(result.current).toBeNull();
    expect(readModelCache("codex", "")).toBeNull();
  });
});

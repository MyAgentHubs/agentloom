import { renderHook, act } from "@testing-library/react";
import { describe, it, expect } from "vitest";
import { useDropdown } from "./useDropdown";

describe("useDropdown", () => {
  it("初始 closed", () => {
    const { result } = renderHook(() => useDropdown());
    expect(result.current.open).toBe(false);
  });

  it("setOpen / toggle / close 状态机", () => {
    const { result } = renderHook(() => useDropdown());
    act(() => result.current.setOpen(true));
    expect(result.current.open).toBe(true);
    act(() => result.current.toggle());
    expect(result.current.open).toBe(false);
    act(() => result.current.toggle());
    expect(result.current.open).toBe(true);
    act(() => result.current.close());
    expect(result.current.open).toBe(false);
  });

  it("triggerProps 暴露 aria-expanded 随 open 变", () => {
    const { result } = renderHook(() => useDropdown());
    expect(result.current.triggerProps["aria-expanded"]).toBe(false);
    act(() => result.current.setOpen(true));
    expect(result.current.triggerProps["aria-expanded"]).toBe(true);
  });
});

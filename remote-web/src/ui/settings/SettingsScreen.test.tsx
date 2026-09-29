// SettingsScreen.test.tsx — TDD 覆盖 src/ui/settings/SettingsScreen.tsx（msgfix2 U3 verbose 开关 +
// msgfix2 U4 在同屏第二行扩展的缓存开关）。

import { cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import { SettingsScreen } from "./SettingsScreen.tsx";

afterEach(() => {
  cleanup();
});

function renderScreen(overrides: Partial<Parameters<typeof SettingsScreen>[0]> = {}) {
  return render(
    <SettingsScreen
      verboseEnabled={false}
      onToggleVerbose={() => {}}
      cacheEnabled={true}
      onToggleCache={() => {}}
      {...overrides}
    />,
  );
}

describe("SettingsScreen · verbose 开关（msgfix2 U3）", () => {
  it("verboseEnabled=false renders the toggle in the off state (aria-checked=false, no --on class)", () => {
    renderScreen({ verboseEnabled: false });
    const toggle = screen.getByTestId("settings-verbose-toggle");
    expect(toggle.getAttribute("aria-checked")).toBe("false");
    expect(toggle.getAttribute("role")).toBe("switch");
    expect(toggle.className).not.toContain("settings-toggle--on");
  });

  it("verboseEnabled=true renders the toggle in the on state", () => {
    renderScreen({ verboseEnabled: true });
    const toggle = screen.getByTestId("settings-verbose-toggle");
    expect(toggle.getAttribute("aria-checked")).toBe("true");
    expect(toggle.className).toContain("settings-toggle--on");
  });

  it("clicking the toggle calls onToggleVerbose with the flipped value (off → true, on → false)", async () => {
    const user = userEvent.setup();
    const onToggleVerbose = vi.fn();
    const { rerender } = renderScreen({ verboseEnabled: false, onToggleVerbose });
    await user.click(screen.getByTestId("settings-verbose-toggle"));
    expect(onToggleVerbose).toHaveBeenCalledWith(true);

    onToggleVerbose.mockClear();
    rerender(<SettingsScreen verboseEnabled={true} onToggleVerbose={onToggleVerbose} cacheEnabled={true} onToggleCache={() => {}} />);
    await user.click(screen.getByTestId("settings-verbose-toggle"));
    expect(onToggleVerbose).toHaveBeenCalledWith(false);
  });

  it("the verbose row's label/hint text render (i18n keys resolve, not raw key fallback)", () => {
    renderScreen();
    const toggle = screen.getByTestId("settings-verbose-toggle");
    expect(toggle.textContent).not.toContain("settings.verbose.label");
    expect(toggle.textContent).not.toContain("settings.verbose.hint");
  });
});

describe("SettingsScreen · 缓存开关（msgfix2 U4，同屏第二行）", () => {
  it("cacheEnabled=true renders the toggle in the on state (设计稿默认开)", () => {
    renderScreen({ cacheEnabled: true });
    const toggle = screen.getByTestId("settings-cache-toggle");
    expect(toggle.getAttribute("aria-checked")).toBe("true");
    expect(toggle.getAttribute("role")).toBe("switch");
    expect(toggle.className).toContain("settings-toggle--on");
  });

  it("cacheEnabled=false renders the toggle in the off state", () => {
    renderScreen({ cacheEnabled: false });
    const toggle = screen.getByTestId("settings-cache-toggle");
    expect(toggle.getAttribute("aria-checked")).toBe("false");
    expect(toggle.className).not.toContain("settings-toggle--on");
  });

  it("clicking the toggle calls onToggleCache with the flipped value (on → false, off → true)", async () => {
    const user = userEvent.setup();
    const onToggleCache = vi.fn();
    const { rerender } = renderScreen({ cacheEnabled: true, onToggleCache });
    await user.click(screen.getByTestId("settings-cache-toggle"));
    expect(onToggleCache).toHaveBeenCalledWith(false);

    onToggleCache.mockClear();
    rerender(<SettingsScreen verboseEnabled={false} onToggleVerbose={() => {}} cacheEnabled={false} onToggleCache={onToggleCache} />);
    await user.click(screen.getByTestId("settings-cache-toggle"));
    expect(onToggleCache).toHaveBeenCalledWith(true);
  });

  it("clicking the cache toggle does not affect the verbose toggle's state (two independent rows)", async () => {
    const user = userEvent.setup();
    const onToggleCache = vi.fn();
    const onToggleVerbose = vi.fn();
    renderScreen({ verboseEnabled: true, onToggleVerbose, cacheEnabled: true, onToggleCache });
    await user.click(screen.getByTestId("settings-cache-toggle"));
    expect(onToggleCache).toHaveBeenCalledTimes(1);
    expect(onToggleVerbose).not.toHaveBeenCalled();
  });

  it("the cache row's label/hint text render (i18n keys resolve, not raw key fallback)", () => {
    renderScreen();
    const toggle = screen.getByTestId("settings-cache-toggle");
    expect(toggle.textContent).not.toContain("settings.cache.label");
    expect(toggle.textContent).not.toContain("settings.cache.hint");
  });

  it("verbose 行与 cache 行同时渲染在同一个 settings-list 里（结构上是同屏第二行，不是另开一屏）", () => {
    renderScreen();
    const list = screen.getByTestId("settings-list");
    expect(list.querySelector('[data-testid="settings-verbose-toggle"]')).not.toBeNull();
    expect(list.querySelector('[data-testid="settings-cache-toggle"]')).not.toBeNull();
  });
});

describe("SettingsScreen · 解除配对（msgfix2 U4 修单 H2，四触发点②唯一入口）", () => {
  it("onUnpair omitted: no unpair button rendered (same degrade pattern as onBack)", () => {
    renderScreen();
    expect(screen.queryByTestId("settings-unpair-button")).toBeNull();
  });

  it("onUnpair provided: button renders and clicking it calls onUnpair", async () => {
    const user = userEvent.setup();
    const onUnpair = vi.fn();
    renderScreen({ onUnpair });
    await user.click(screen.getByTestId("settings-unpair-button"));
    expect(onUnpair).toHaveBeenCalledTimes(1);
  });

  it("the unpair row's label/hint text render (i18n keys resolve, not raw key fallback)", () => {
    renderScreen({ onUnpair: () => {} });
    const button = screen.getByTestId("settings-unpair-button");
    expect(button.textContent).not.toContain("settings.unpair.label");
    expect(button.textContent).not.toContain("settings.unpair.hint");
  });

  it("clicking unpair does not affect the verbose/cache toggles (independent row)", async () => {
    const user = userEvent.setup();
    const onUnpair = vi.fn();
    const onToggleVerbose = vi.fn();
    const onToggleCache = vi.fn();
    renderScreen({ onUnpair, onToggleVerbose, onToggleCache });
    await user.click(screen.getByTestId("settings-unpair-button"));
    expect(onUnpair).toHaveBeenCalledTimes(1);
    expect(onToggleVerbose).not.toHaveBeenCalled();
    expect(onToggleCache).not.toHaveBeenCalled();
  });
});

describe("SettingsScreen · onBack", () => {
  it("onBack omitted: no back button rendered (same degrade pattern as SessionStreamScreen.onBack)", () => {
    renderScreen();
    expect(screen.queryByTestId("settings-back")).toBeNull();
  });

  it("onBack provided: back button renders and clicking it calls onBack", async () => {
    const user = userEvent.setup();
    const onBack = vi.fn();
    renderScreen({ onBack });
    await user.click(screen.getByTestId("settings-back"));
    expect(onBack).toHaveBeenCalledTimes(1);
  });
});

import { act, cleanup, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { ConnectionSessionPhase } from "../../connection/types.ts";
import { ConnectionBanner } from "./ConnectionBanner.tsx";

afterEach(() => {
  cleanup();
  vi.useRealTimers();
});

describe("ConnectionBanner", () => {
  it("phase=open 时零渲染", () => {
    render(<ConnectionBanner phase="open" phaseChangedAtMs={0} locale="zh" />);
    expect(screen.queryByTestId("connection-banner")).toBeNull();
  });

  // C1-PS（dogfood 修障第二批）：desktopPresence 维度——phase=open 时的行为不再是恒零渲染，
  // 而是取决于桌面在线态（online/unknown 仍零渲染，只有确认 offline 才额外提示）。
  it("phase=open + desktopPresence 省略（默认 unknown）：仍零渲染，与旧版行为一致", () => {
    render(<ConnectionBanner phase="open" phaseChangedAtMs={0} locale="zh" />);
    expect(screen.queryByTestId("connection-banner")).toBeNull();
  });

  it("phase=open + desktopPresence=online：零渲染", () => {
    render(<ConnectionBanner phase="open" phaseChangedAtMs={0} desktopPresence="online" locale="zh" />);
    expect(screen.queryByTestId("connection-banner")).toBeNull();
  });

  it("phase=open + desktopPresence=unknown：零渲染", () => {
    render(<ConnectionBanner phase="open" phaseChangedAtMs={0} desktopPresence="unknown" locale="zh" />);
    expect(screen.queryByTestId("connection-banner")).toBeNull();
  });

  it("phase=open + desktopPresence=offline：渲染弱担保横幅（不是错误态，不断言未连接）", () => {
    render(<ConnectionBanner phase="open" phaseChangedAtMs={0} desktopPresence="offline" locale="zh" />);
    const banner = screen.getByTestId("connection-banner");
    expect(banner.textContent).toContain("电脑似乎不在线");
    expect(banner.getAttribute("data-desktop-presence")).toBe("offline");
  });

  it("phase!==open 时不受 desktopPresence 影响——仍走既有的连接相位提示（连接本身都没打通，presence 信息本就无意义）", () => {
    render(
      <ConnectionBanner
        phase="reconnect_scheduled"
        phaseChangedAtMs={93_000}
        now={() => 100_000}
        desktopPresence="offline"
        locale="zh"
      />,
    );
    const banner = screen.getByTestId("connection-banner");
    expect(banner.textContent).toContain("未连接·重连中");
    expect(banner.textContent).not.toContain("电脑似乎不在线");
  });

  it.each<[ConnectionSessionPhase, string]>([
    ["idle", "未连接"],
    ["connecting", "连接中"],
    ["reconnect_scheduled", "未连接·重连中"],
    ["needs_repair", "配对已失效"],
    ["closed", "未连接"],
  ])("phase=%s 时显示对应提示", (phase, expected) => {
    render(<ConnectionBanner phase={phase} phaseChangedAtMs={93_000} now={() => 100_000} locale="zh" />);
    const banner = screen.getByTestId("connection-banner");
    expect(banner.textContent).toContain(expected);
    if (phase === "connecting" || phase === "reconnect_scheduled") {
      expect(banner.textContent).toContain("7 秒");
    }
  });

  it.each<ConnectionSessionPhase>(["connecting", "reconnect_scheduled"])(
    "%s 持续满 60 秒时升级为重新扫码提示",
    (phase) => {
      vi.useFakeTimers();
      let nowMs = 1_000_000;
      render(<ConnectionBanner phase={phase} phaseChangedAtMs={nowMs} now={() => nowMs} locale="zh" />);
      expect(screen.getByTestId("connection-banner").textContent).not.toContain("重新扫码配对");

      nowMs += 60_000;
      act(() => {
        vi.advanceTimersByTime(60_000);
      });
      expect(screen.getByTestId("connection-banner").textContent).toContain(
        "若持续无法连接，请在电脑上重新扫码配对",
      );
    },
  );
});

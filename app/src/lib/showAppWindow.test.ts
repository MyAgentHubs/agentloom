import { beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("@tauri-apps/api/window", () => ({
  getCurrentWindow: vi.fn(),
}));

import { getCurrentWindow } from "@tauri-apps/api/window";
import { showAppWindow } from "./showAppWindow";

const mockedGetCurrentWindow = vi.mocked(getCurrentWindow);

describe("showAppWindow", () => {
  beforeEach(() => {
    mockedGetCurrentWindow.mockReset();
  });

  it("shows and focuses the window, show 先于 setFocus，各恰好调一次", async () => {
    const callOrder: string[] = [];
    const show = vi.fn().mockImplementation(async () => {
      callOrder.push("show");
    });
    const setFocus = vi.fn().mockImplementation(async () => {
      callOrder.push("setFocus");
    });
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    mockedGetCurrentWindow.mockReturnValue({ show, setFocus } as any);

    await expect(showAppWindow()).resolves.toBeNull();
    expect(show).toHaveBeenCalledTimes(1);
    expect(setFocus).toHaveBeenCalledTimes(1);
    expect(callOrder).toEqual(["show", "setFocus"]);
  });

  it("getCurrentWindow 同步抛异常 → resolves 错误描述字符串", async () => {
    mockedGetCurrentWindow.mockImplementation(() => {
      throw new Error("not in a tauri context");
    });

    await expect(showAppWindow()).resolves.toBe("not in a tauri context");
  });

  it("show() 以字符串 reject（Tauri invoke 权限拒绝）→ resolves 该字符串", async () => {
    const rejection =
      "window.show not allowed. Permissions associated with this command: window:allow-show";
    const show = vi.fn().mockRejectedValue(rejection);
    const setFocus = vi.fn();
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    mockedGetCurrentWindow.mockReturnValue({ show, setFocus } as any);

    await expect(showAppWindow()).resolves.toBe(rejection);
  });

  it("show resolve 但 setFocus reject → resolves null（窗口已显示即成功）", async () => {
    const show = vi.fn().mockResolvedValue(undefined);
    const setFocus = vi.fn().mockRejectedValue(new Error("focus failed"));
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    mockedGetCurrentWindow.mockReturnValue({ show, setFocus } as any);

    await expect(showAppWindow()).resolves.toBeNull();
  });
});

import { invoke } from "@tauri-apps/api/core";
import { waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

declare const process: { readonly env: Record<string, string | undefined> };

vi.mock("@tauri-apps/api/core", () => {
  const base = vi.fn();
  // VITEST_DEFER_INVOKE=1 makes every invoke settle one macrotask later, which
  // deterministically exposes assertions that read state landing from a *different*
  // async source than the one they awaited. CI runners are ~12x slower than a dev
  // machine and lose those races for real; this switch reproduces it on purpose.
  return {
    invoke: process.env.VITEST_DEFER_INVOKE
      ? new Proxy(base, {
          apply: (t, self, args) =>
            new Promise((r) => setTimeout(r, 0)).then(() =>
              Reflect.apply(t, self, args),
            ),
        })
      : base,
  };
});

const invokeMock = vi.mocked(invoke);

import {
  installFatalErrorHandlers,
  resetFatalErrorPageStateForTest,
} from "./fatalErrorHandlers";

describe("installFatalErrorHandlers", () => {
  beforeEach(() => {
    invokeMock.mockReset();
    invokeMock.mockResolvedValue(undefined);
    resetFatalErrorPageStateForTest();
    document.body.innerHTML = '<div id="root"></div>';
    installFatalErrorHandlers();
  });

  afterEach(() => {
    document.body.innerHTML = "";
  });

  it("renders the fatal error page into #root when #root is empty on window.onerror", async () => {
    const root = document.getElementById("root") as HTMLElement;
    expect(root.childElementCount).toBe(0);

    const err = new Error("boot exploded");
    const event = new ErrorEvent("error", { error: err, message: err.message });
    window.dispatchEvent(event);

    const page = root.querySelector('[data-testid="fatal-error-page"]');
    expect(page).not.toBeNull();
    expect(page?.textContent).toContain("boot exploded");

    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith(
        "boot_trace",
        expect.objectContaining({
          label: expect.stringContaining("window.onerror"),
        }),
      ),
    );
  });

  it("does not take over #root on window.onerror when #root already has content", () => {
    const root = document.getElementById("root") as HTMLElement;
    root.appendChild(document.createElement("div"));

    const err = new Error("already mounted, this is a stray error");
    window.dispatchEvent(
      new ErrorEvent("error", { error: err, message: err.message }),
    );

    expect(root.querySelector('[data-testid="fatal-error-page"]')).toBeNull();
  });

  it("on unhandledrejection: reports but does NOT show the fatal error page", async () => {
    const root = document.getElementById("root") as HTMLElement;

    const rejectionEvent = Object.assign(new Event("unhandledrejection"), {
      reason: new Error("benign rejection"),
      promise: Promise.reject(new Error("benign rejection")).catch(() => {}),
    });
    window.dispatchEvent(rejectionEvent);

    expect(root.querySelector('[data-testid="fatal-error-page"]')).toBeNull();

    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith(
        "boot_trace",
        expect.objectContaining({
          label: expect.stringContaining("unhandledrejection"),
        }),
      ),
    );
  });

  it("only shows the fatal error page once even if multiple errors fire", () => {
    const root = document.getElementById("root") as HTMLElement;
    window.dispatchEvent(
      new ErrorEvent("error", { error: new Error("first") }),
    );
    window.dispatchEvent(
      new ErrorEvent("error", { error: new Error("second") }),
    );
    expect(
      root.querySelectorAll('[data-testid="fatal-error-page"]').length,
    ).toBe(1);
  });
});

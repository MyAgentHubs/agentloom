import { render, screen } from "@testing-library/react";
import { describe, expect, it, vi, beforeEach } from "vitest";

const { invokeMock } = vi.hoisted(() => ({ invokeMock: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke: invokeMock }));

import { FatalErrorBoundary } from "./FatalErrorBoundary";

function Boom(): never {
  throw new Error("kaboom from child render");
}

describe("FatalErrorBoundary", () => {
  beforeEach(() => {
    invokeMock.mockReset();
    invokeMock.mockResolvedValue(undefined);
  });

  it("renders children normally when nothing throws", () => {
    render(
      <FatalErrorBoundary>
        <div>all good</div>
      </FatalErrorBoundary>,
    );
    expect(screen.getByText("all good")).toBeInTheDocument();
  });

  it("catches a render error, shows the fatal error page with the message, and reports via boot_trace", async () => {
    const consoleSpy = vi.spyOn(console, "error").mockImplementation(() => {});
    try {
      render(
        <FatalErrorBoundary>
          <Boom />
        </FatalErrorBoundary>,
      );

      const page = await screen.findByTestId("fatal-error-page");
      expect(page.textContent).toContain("kaboom from child render");

      await Promise.resolve();
      expect(invokeMock).toHaveBeenCalledWith(
        "boot_trace",
        expect.objectContaining({
          label: expect.stringContaining("kaboom from child render"),
        }),
      );
    } finally {
      consoleSpy.mockRestore();
    }
  });
});

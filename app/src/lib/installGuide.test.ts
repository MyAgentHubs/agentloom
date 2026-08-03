import { beforeEach, describe, expect, it, vi } from "vitest";
import { openUrl } from "@tauri-apps/plugin-opener";
import { installGuideUrl, openInstallGuide } from "./installGuide";

vi.mock("@tauri-apps/plugin-opener", () => ({
  openUrl: vi.fn(),
}));

describe("installGuide", () => {
  beforeEach(() => {
    vi.mocked(openUrl).mockReset();
    vi.mocked(openUrl).mockResolvedValue(undefined);
  });

  it.each([
    ["claude", "https://claude.com/claude-code"],
    ["codex", "https://github.com/openai/codex"],
  ] as const)("maps %s to its official install page", (cli, expected) => {
    expect(installGuideUrl(cli)).toBe(expected);
  });

  it("opens the mapped install page in the system browser", async () => {
    await openInstallGuide("codex");

    expect(openUrl).toHaveBeenCalledWith("https://github.com/openai/codex");
  });

  it("silently ignores opener failures", async () => {
    vi.mocked(openUrl).mockRejectedValueOnce(new Error("opener unavailable"));

    await expect(openInstallGuide("claude")).resolves.toBeUndefined();
  });
});

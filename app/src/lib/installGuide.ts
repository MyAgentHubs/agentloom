import { openUrl } from "@tauri-apps/plugin-opener";

type InstallGuideCli = "claude" | "codex";

const INSTALL_GUIDE_URLS: Record<InstallGuideCli, string> = {
  claude: "https://claude.com/claude-code",
  codex: "https://github.com/openai/codex",
};

export function installGuideUrl(cli: InstallGuideCli): string {
  return INSTALL_GUIDE_URLS[cli];
}

export function openInstallGuide(cli: InstallGuideCli): Promise<void> {
  return openUrl(installGuideUrl(cli)).catch(() => {});
}

import { useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import type { useI18n } from "../../i18n";
import { renderBackendError } from "../../lib/backendMsg";
import type { RepoMeta } from "../../types/agent";
import type { RemoteControlSettings } from "./remoteControlTypes";

export function useRemoteControlProjects(
  refreshRemoteStatus: () => Promise<void>,
  loadDevices: () => Promise<void>,
  setDevicesError: (error: string | null) => void,
  t: ReturnType<typeof useI18n>["t"],
) {
  // The served project determines which pairing and device state the UI shows.
  const [repos, setRepos] = useState<RepoMeta[]>([]);
  const [activeRepoId, setActiveRepoId] = useState<string | null>(null);
  const [activeProjectSaving, setActiveProjectSaving] = useState(false);
  const [activeProjectError, setActiveProjectError] = useState<string | null>(
    null,
  );
  // The "re-scan needed" notice shown after switching the served project relies purely on local state.
  // It disappears as soon as the page is re-entered (unmount/remount resets it to false), so no timer or persistence is needed.
  const [switchNotice, setSwitchNotice] = useState(false);

  async function changeActiveProject(nextRepoId: string) {
    if (activeProjectSaving) return;
    const previousActiveRepoId = activeRepoId;
    const normalizedRepoId = nextRepoId === "" ? null : nextRepoId;
    if (normalizedRepoId === previousActiveRepoId) return;

    setActiveRepoId(normalizedRepoId);
    setActiveProjectSaving(true);
    setActiveProjectError(null);
    try {
      await invoke<void>("remote_set_active_project", {
        repoId: normalizedRepoId,
      });
      const settings = await invoke<RemoteControlSettings>(
        "remote_control_get_settings",
      );
      setActiveRepoId(settings.active_repo_id ?? null);
      // Only a switch between two already-configured projects counts as a real change
      // that warrants the re-scan reminder; a project carried over from "unset" (including the auto-default on enable) has no prior room to disconnect.
      if (previousActiveRepoId !== null) {
        setSwitchNotice(true);
      }
      await refreshRemoteStatus();
      // The backend filters remote_devices_list by the active room. Refresh after switching
      // so devices from the old room do not remain visible with a misleading revoke action.
      // A stale row's revoke can fail to delete the relay token when the new room cannot
      // resolve it, leaving only a local revocation. Reuse loadDevices for this refresh.
      // Handle this separately: a device-list failure must not roll back a successful project switch.
      try {
        await loadDevices();
      } catch (cause) {
        setDevicesError(renderBackendError(String(cause), t));
      }
    } catch (cause) {
      setActiveRepoId(previousActiveRepoId);
      setActiveProjectError(renderBackendError(String(cause), t));
    } finally {
      setActiveProjectSaving(false);
    }
  }

  return {
    repos,
    activeRepoId,
    activeProjectSaving,
    activeProjectError,
    switchNotice,
    setRepos,
    setActiveRepoId,
    setActiveProjectError,
    setSwitchNotice,
    changeActiveProject,
  };
}

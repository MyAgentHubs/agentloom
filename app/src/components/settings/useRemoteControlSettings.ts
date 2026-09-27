import { useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import type { useI18n } from "../../i18n";
import { renderBackendError } from "../../lib/backendMsg";

export function useRemoteControlSettings(
  currentRepoId: string | null,
  activeRepoId: string | null,
  changeActiveProject: (nextRepoId: string) => Promise<void>,
  startPolling: () => void,
  stopPolling: () => void,
  setGatewayStoppedReason: (reason: string | null) => void,
  setGatewayStatusError: (error: string | null) => void,
  t: ReturnType<typeof useI18n>["t"],
) {
  const [enabled, setEnabled] = useState(false);
  const [relayUrl, setRelayUrl] = useState("");
  const savedRelayUrlRef = useRef("");
  // The public relay URL applies when the field is empty. It comes from settings for
  // the placeholder and pairing fallback, and is not a second writable state.
  // Keep it empty until settings load, then fill it immediately.
  const [defaultRelayUrl, setDefaultRelayUrl] = useState("");
  const [settingsLoading, setSettingsLoading] = useState(true);
  const [settingsSaving, setSettingsSaving] = useState(false);
  const [relaySaving, setRelaySaving] = useState(false);
  const [settingsError, setSettingsError] = useState<string | null>(null);
  const [relayError, setRelayError] = useState<string | null>(null);

  async function toggleEnabled() {
    if (settingsLoading || settingsSaving || relaySaving) return;
    const previousEnabled = enabled;
    const nextEnabled = !previousEnabled;
    setEnabled(nextEnabled);
    setSettingsSaving(true);
    setSettingsError(null);
    try {
      await invoke<void>("remote_control_set_settings", {
        enabled: nextEnabled,
        relayUrl: savedRelayUrlRef.current,
      });
      if (nextEnabled) {
        startPolling();
        // When no served project is set, default to the app's current active project.
        // Keep an existing served project even if it differs; the mismatch notice allows manual switching.
        if (activeRepoId === null && currentRepoId !== null) {
          await changeActiveProject(currentRepoId);
        }
      } else {
        stopPolling();
        setGatewayStoppedReason(null);
        setGatewayStatusError(null);
      }
    } catch (cause) {
      setEnabled(previousEnabled);
      setSettingsError(renderBackendError(String(cause), t));
    } finally {
      setSettingsSaving(false);
    }
  }

  async function saveRelayUrl() {
    const nextRelayUrl = relayUrl.trim();
    if (
      settingsLoading ||
      settingsSaving ||
      relaySaving ||
      nextRelayUrl === savedRelayUrlRef.current
    ) {
      return;
    }

    setRelaySaving(true);
    setRelayError(null);
    try {
      await invoke<void>("remote_control_set_settings", {
        enabled,
        relayUrl: nextRelayUrl,
      });
      savedRelayUrlRef.current = nextRelayUrl;
      setRelayUrl(nextRelayUrl);
    } catch (cause) {
      setRelayError(renderBackendError(String(cause), t));
    } finally {
      setRelaySaving(false);
    }
  }

  return {
    enabled,
    setEnabled,
    relayUrl,
    setRelayUrl,
    savedRelayUrlRef,
    defaultRelayUrl,
    setDefaultRelayUrl,
    settingsLoading,
    setSettingsLoading,
    settingsSaving,
    relaySaving,
    settingsError,
    setSettingsError,
    relayError,
    setRelayError,
    toggleEnabled,
    saveRelayUrl,
  };
}

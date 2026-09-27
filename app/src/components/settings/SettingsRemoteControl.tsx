import { useEffect } from "react";
import type { ChangeEvent } from "react";
import { invoke } from "@tauri-apps/api/core";
import { localProjectDisplayName, useI18n } from "../../i18n";
import { renderBackendError } from "../../lib/backendMsg";
import { ConfirmDialog } from "../ConfirmDialog";
import type { RepoMeta } from "../../types/agent";
import { styles } from "./remoteControlStyles";
import {
  RemoteControlHeader,
  RemoteProjectSection,
  RemoteRelayField,
  RemotePairingSection,
  RemoteDevicesSection,
  RemoteDiagnostics,
} from "./remoteControlSections";
import type { RemoteControlSettings } from "./remoteControlTypes";
import { isValidOrEmptyRelayUrl } from "./remoteControlRelayQr";
import { useRemoteControlDevices } from "./useRemoteControlDevices";
import { useRemoteControlPairing } from "./useRemoteControlPairing";
import { useRemoteControlProjects } from "./useRemoteControlProjects";
import { useRemoteControlSettings } from "./useRemoteControlSettings";

type SettingsRemoteControlProps = {
  /** The app's current active project id, used to detect a mismatch with the served project.
   *  An omitted id is treated as null and does not show a mismatch notice. */
  currentRepoId?: string | null;
};

export function SettingsRemoteControl({
  currentRepoId = null,
}: SettingsRemoteControlProps = {}) {
  const { locale, t } = useI18n();
  const devices = useRemoteControlDevices(t);
  const pairing = useRemoteControlPairing(
    devices.loadDevices,
    devices.setDevicesError,
    t,
  );
  const projects = useRemoteControlProjects(
    pairing.refreshRemoteStatus,
    devices.loadDevices,
    devices.setDevicesError,
    t,
  );
  const settings = useRemoteControlSettings(
    currentRepoId,
    projects.activeRepoId,
    projects.changeActiveProject,
    pairing.startPolling,
    pairing.stopPolling,
    pairing.setGatewayStoppedReason,
    pairing.setGatewayStatusError,
    t,
  );
  const {
    setEnabled,
    setRelayUrl,
    savedRelayUrlRef,
    setDefaultRelayUrl,
    setSettingsError,
    setSettingsLoading,
  } = settings;
  const { setActiveRepoId, setActiveProjectError, setRepos } = projects;
  const { startPolling, stopPolling } = pairing;
  const { loadDevices, setDevicesError, setDevicesLoading } = devices;

  useEffect(() => {
    let cancelled = false;

    void invoke<RemoteControlSettings>("remote_control_get_settings")
      .then((settings) => {
        if (cancelled) return;
        setEnabled(settings.enabled);
        setRelayUrl(settings.relay_url);
        savedRelayUrlRef.current = settings.relay_url;
        setDefaultRelayUrl(settings.default_relay_url);
        setActiveRepoId(settings.active_repo_id ?? null);
        setSettingsError(null);
        if (settings.enabled) startPolling();
      })
      .catch((cause) => {
        if (!cancelled) {
          setSettingsError(renderBackendError(String(cause), t));
        }
      })
      .finally(() => {
        if (!cancelled) setSettingsLoading(false);
      });

    void invoke<RepoMeta[]>("list_repos")
      .then((result) => {
        if (!cancelled) setRepos(result ?? []);
      })
      .catch((cause) => {
        if (!cancelled) {
          setActiveProjectError(renderBackendError(String(cause), t));
        }
      });

    void loadDevices()
      .catch((cause) => {
        if (!cancelled) {
          setDevicesError(renderBackendError(String(cause), t));
        }
      })
      .finally(() => {
        if (!cancelled) setDevicesLoading(false);
      });

    return () => {
      cancelled = true;
      stopPolling();
    };
  }, [loadDevices, startPolling, stopPolling, t]);

  return (
    <RemoteControlView
      currentRepoId={currentRepoId}
      locale={locale}
      t={t}
      devices={devices}
      pairing={pairing}
      projects={projects}
      settings={settings}
    />
  );
}

function getGatewayStoppedMessage(
  gatewayStoppedReason: string | null,
  t: ReturnType<typeof useI18n>["t"],
) {
  return gatewayStoppedReason === "room_claim_conflict"
    ? t("settings.remoteControl.stopped.roomClaimConflict")
    : gatewayStoppedReason === "room_claim_conflict_project"
      ? t("settings.remoteControl.stopped.roomClaimConflictProject")
      : gatewayStoppedReason === "room_tombstoned"
        ? t("settings.remoteControl.stopped.roomTombstoned")
        : gatewayStoppedReason === "room_device_status_unavailable"
          ? t("settings.remoteControl.stopped.roomDeviceStatusUnavailable")
          : gatewayStoppedReason === "registry_rebase_limit"
            ? t("settings.remoteControl.stopped.registryRebaseLimit")
            : gatewayStoppedReason
              ? t("settings.remoteControl.stopped.unknown", {
                  code: gatewayStoppedReason,
                })
              : null;
}

type RemoteControlViewProps = {
  currentRepoId: string | null;
  locale: ReturnType<typeof useI18n>["locale"];
  t: ReturnType<typeof useI18n>["t"];
  devices: ReturnType<typeof useRemoteControlDevices>;
  pairing: ReturnType<typeof useRemoteControlPairing>;
  projects: ReturnType<typeof useRemoteControlProjects>;
  settings: ReturnType<typeof useRemoteControlSettings>;
};

function RemoteControlView({
  currentRepoId,
  locale,
  t,
  devices: deviceState,
  pairing,
  projects,
  settings,
}: RemoteControlViewProps) {
  const {
    enabled,
    relayUrl,
    savedRelayUrlRef,
    defaultRelayUrl,
    settingsLoading,
    settingsSaving,
    relaySaving,
    settingsError,
    relayError,
    setRelayUrl,
    setRelayError,
    toggleEnabled,
    saveRelayUrl,
  } = settings;
  const {
    repos,
    activeRepoId,
    activeProjectSaving,
    activeProjectError,
    switchNotice,
    setSwitchNotice,
    changeActiveProject,
  } = projects;
  const {
    gatewayStoppedReason,
    gatewayStatusError,
    pairingStatus,
    qrMarkup,
    pairingPayload,
    pairingStringCopied,
    pairingAction,
    pairingError,
    copyPairingString,
    cancelPairing,
  } = pairing;
  const { devices } = deviceState;

  const normalizedRelayUrl = relayUrl.trim();
  const relayReady =
    normalizedRelayUrl === savedRelayUrlRef.current &&
    isValidOrEmptyRelayUrl(normalizedRelayUrl);
  const gatewayStoppedMessage = getGatewayStoppedMessage(
    gatewayStoppedReason,
    t,
  );
  // Resolve project ids to display names; use the id while the repository list is unavailable.
  function repoDisplayName(repoId: string | null): string {
    if (repoId === null) return t("settings.remoteControl.activeProjectUnset");
    const repo = repos.find((candidate) => candidate.id === repoId);
    return repo ? localProjectDisplayName(repo, t) : repoId;
  }
  const servingProjectName = repoDisplayName(activeRepoId);
  // Compare only when the app has a current project to switch to.
  const hasProjectMismatch =
    currentRepoId !== null && currentRepoId !== activeRepoId;
  const currentProjectName = hasProjectMismatch
    ? repoDisplayName(currentRepoId)
    : "";

  function switchToCurrentProject() {
    if (currentRepoId !== null) void changeActiveProject(currentRepoId);
  }

  function changeRelayUrl(event: ChangeEvent<HTMLInputElement>) {
    setRelayUrl(event.target.value);
    setRelayError(null);
  }

  return (
    <div style={styles.root}>
      <RemoteControlHeader
        enabled={enabled}
        settingsLoading={settingsLoading}
        settingsSaving={settingsSaving}
        relaySaving={relaySaving}
        settingsError={settingsError}
        gatewayStoppedMessage={gatewayStoppedMessage}
        gatewayStatusError={gatewayStatusError}
        toggleEnabled={toggleEnabled}
      />

      <RemoteProjectSection
        servingProjectName={servingProjectName}
        hasProjectMismatch={hasProjectMismatch}
        currentProjectName={currentProjectName}
        activeProjectSaving={activeProjectSaving}
        settingsLoading={settingsLoading}
        activeRepoId={activeRepoId}
        repos={repos}
        devices={devices}
        activeProjectError={activeProjectError}
        switchNotice={switchNotice}
        switchToCurrentProject={switchToCurrentProject}
        changeActiveProject={changeActiveProject}
        dismissSwitchNotice={() => setSwitchNotice(false)}
      />

      <RemoteRelayField
        relayUrl={relayUrl}
        defaultRelayUrl={defaultRelayUrl}
        settingsLoading={settingsLoading}
        settingsSaving={settingsSaving}
        relayError={relayError}
        changeRelayUrl={changeRelayUrl}
        saveRelayUrl={saveRelayUrl}
      />

      <RemotePairingSection
        enabled={enabled}
        qrMarkup={qrMarkup}
        pairingPayload={pairingPayload}
        pairingStringCopied={pairingStringCopied}
        pairingAction={pairingAction}
        relayReady={relayReady}
        relaySaving={relaySaving}
        activeRepoId={activeRepoId}
        pairingStatus={pairingStatus}
        pairingError={pairingError}
        copyPairingString={copyPairingString}
        cancelPairing={cancelPairing}
        beginPairing={() =>
          pairing.beginPairing({
            enabled,
            relaySaving,
            relayUrl,
            savedRelayUrl: savedRelayUrlRef.current,
            defaultRelayUrl,
            activeRepoId,
          })
        }
      />

      <RemoteDeviceControls
        deviceState={deviceState}
        gatewayDiagnostics={pairing.gatewayDiagnostics}
        activeRepoId={activeRepoId}
        locale={locale}
        t={t}
      />
    </div>
  );
}

type RemoteDeviceControlsProps = {
  deviceState: ReturnType<typeof useRemoteControlDevices>;
  gatewayDiagnostics: ReturnType<
    typeof useRemoteControlPairing
  >["gatewayDiagnostics"];
  activeRepoId: string | null;
  locale: ReturnType<typeof useI18n>["locale"];
  t: ReturnType<typeof useI18n>["t"];
};

function RemoteDeviceControls({
  deviceState,
  gatewayDiagnostics,
  activeRepoId,
  locale,
  t,
}: RemoteDeviceControlsProps) {
  const {
    devices,
    devicesLoading,
    devicesError,
    revokeTarget,
    revokingDeviceId,
    setRevokeTarget,
    revokeDevice,
  } = deviceState;
  const dateLocale = locale === "zh" ? "zh-CN" : "en-US";
  const formatTime = (milliseconds: number) =>
    new Date(milliseconds).toLocaleString(dateLocale);
  const deviceRows = devices.map((device) => ({
    device,
    createdAt: formatTime(device.created_at * 1000),
    expiresAt: formatTime(device.access_expires_at),
  }));

  return (
    <>
      <RemoteDevicesSection
        devicesError={devicesError}
        activeRepoId={activeRepoId}
        devicesLoading={devicesLoading}
        deviceRows={deviceRows}
        revokingDeviceId={revokingDeviceId}
        setRevokeTarget={setRevokeTarget}
      />
      <RemoteDiagnostics gatewayDiagnostics={gatewayDiagnostics} />

      <ConfirmDialog
        open={revokeTarget !== null}
        title={t("settings.remoteControl.revokeConfirm.title")}
        body={
          <>
            {t("settings.remoteControl.revokeConfirm.body", {
              name: revokeTarget?.name ?? "",
            })}
            <br />
            {t("settings.remoteControl.revokeConfirm.consequence")}
          </>
        }
        confirmLabel={t("settings.remoteControl.revokeConfirm.confirm")}
        cancelLabel={t("settings.remoteControl.revokeConfirm.cancel")}
        tone="danger"
        onConfirm={() => void revokeDevice()}
        onCancel={() => {
          if (!revokingDeviceId) setRevokeTarget(null);
        }}
      />
    </>
  );
}

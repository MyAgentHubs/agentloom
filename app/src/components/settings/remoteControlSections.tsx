import type { ChangeEventHandler } from "react";
import { useI18n } from "../../i18n";
import type { RepoMeta } from "../../types/agent";
import { styles } from "./remoteControlStyles";
import { GATEWAY_COUNTER_NAMES } from "./remoteControlTypes";
import type {
  GatewayCounters,
  RemoteDeviceView,
  RemotePairingPayload,
  RemotePairingStatus,
} from "./remoteControlTypes";

export interface RemoteControlHeaderProps {
  enabled: boolean;
  settingsLoading: boolean;
  settingsSaving: boolean;
  relaySaving: boolean;
  settingsError: string | null;
  gatewayStoppedMessage: string | null;
  gatewayStatusError: string | null;
  toggleEnabled: () => void | Promise<void>;
}

export function RemoteControlHeader({
  enabled,
  settingsLoading,
  settingsSaving,
  relaySaving,
  settingsError,
  gatewayStoppedMessage,
  gatewayStatusError,
  toggleEnabled,
}: RemoteControlHeaderProps) {
  const { t } = useI18n();

  return (
    <>
      <div className="ob-disc-h" style={styles.header}>
        <div style={styles.headerCopy}>
          <span className="t">{t("settings.remoteControl.title")}</span>
          <span style={styles.description}>
            {t("settings.remoteControl.intro")}
          </span>
        </div>
        <div style={styles.switchGroup}>
          <span style={styles.switchStatus}>
            {t(
              enabled
                ? "settings.remoteControl.enabled"
                : "settings.remoteControl.disabled",
            )}
          </span>
          <button
            type="button"
            role="switch"
            aria-checked={enabled}
            aria-label={t("settings.remoteControl.enabledLabel")}
            disabled={settingsLoading || settingsSaving || relaySaving}
            style={{
              ...styles.switch,
              background: enabled ? "var(--green)" : "var(--ink-4)",
              cursor:
                settingsLoading || settingsSaving || relaySaving
                  ? "not-allowed"
                  : "pointer",
              opacity: settingsLoading ? 0.6 : 1,
            }}
            onClick={() => void toggleEnabled()}
          >
            <span
              aria-hidden="true"
              style={{
                ...styles.switchKnob,
                transform: enabled ? "translateX(18px)" : "translateX(0)",
              }}
            />
          </button>
        </div>
      </div>

      {settingsError ? (
        <div role="alert" style={styles.error}>
          {settingsError}
        </div>
      ) : null}

      {enabled && (gatewayStoppedMessage || gatewayStatusError) ? (
        <div role="alert" className="st-test-state err" style={styles.error}>
          {gatewayStoppedMessage ?? gatewayStatusError}
        </div>
      ) : null}
    </>
  );
}

export interface RemoteProjectSectionProps {
  servingProjectName: string;
  hasProjectMismatch: boolean;
  currentProjectName: string;
  activeProjectSaving: boolean;
  settingsLoading: boolean;
  activeRepoId: string | null;
  repos: RepoMeta[];
  devices: RemoteDeviceView[];
  activeProjectError: string | null;
  switchNotice: boolean;
  switchToCurrentProject: () => void;
  changeActiveProject: (repoId: string) => void | Promise<void>;
  dismissSwitchNotice: () => void;
}

export function RemoteProjectSection({
  servingProjectName,
  hasProjectMismatch,
  currentProjectName,
  activeProjectSaving,
  settingsLoading,
  activeRepoId,
  repos,
  devices,
  activeProjectError,
  switchNotice,
  switchToCurrentProject,
  changeActiveProject,
  dismissSwitchNotice,
}: RemoteProjectSectionProps) {
  const { t } = useI18n();

  return (
    <>
      <div style={styles.currentServingRow}>
        <span style={styles.currentServingLabel}>
          {t("settings.remoteControl.currentServingLabel")}
        </span>
        <span
          data-testid="remote-control-current-serving-value"
          style={styles.currentServingValue}
        >
          {servingProjectName}
        </span>
      </div>

      {hasProjectMismatch ? (
        <div role="status" style={styles.mismatchBanner}>
          <span style={styles.mismatchText}>
            {t("settings.remoteControl.projectMismatch", {
              serving: servingProjectName,
              current: currentProjectName,
            })}
          </span>
          <button
            type="button"
            className="ob-btn"
            disabled={activeProjectSaving}
            onClick={() => switchToCurrentProject()}
          >
            {t("settings.remoteControl.projectMismatchSwitch")}
          </button>
        </div>
      ) : null}

      <div style={styles.field}>
        <label htmlFor="remote-control-active-project" style={styles.label}>
          {t("settings.remoteControl.activeProjectLabel")}
        </label>
        <select
          id="remote-control-active-project"
          value={activeRepoId ?? ""}
          disabled={settingsLoading || activeProjectSaving}
          style={styles.selectInput}
          onChange={(event) => void changeActiveProject(event.target.value)}
        >
          <option value="">
            {t("settings.remoteControl.activeProjectUnset")}
          </option>
          {repos.map((repo) => (
            <option key={repo.id} value={repo.id}>
              {repo.name}
            </option>
          ))}
        </select>
        {!activeRepoId || devices.length === 0 ? (
          // Switching projects disconnects paired phones only when paired devices exist.
          // With no devices, a disconnection warning would overstate the risk.
          // Use activeProjectHint to explain that pairing and devices belong to the active project's room,
          // and show the disconnection warning only when it applies.
          <span className="st-form-note plain" style={styles.hint}>
            {t("settings.remoteControl.activeProjectHint")}
          </span>
        ) : (
          <span className="st-form-note plain" style={styles.hint}>
            {t("settings.remoteControl.activeProjectSwitchHint")}
          </span>
        )}
        {activeProjectError ? (
          <span role="alert" style={styles.error}>
            {activeProjectError}
          </span>
        ) : null}
      </div>

      {switchNotice ? (
        <div role="status" style={styles.switchNotice}>
          <span style={styles.switchNoticeText}>
            {t("settings.remoteControl.switchNotice")}
          </span>
          <button
            type="button"
            className="ob-btn"
            onClick={() => dismissSwitchNotice()}
          >
            {t("settings.remoteControl.switchNoticeClose")}
          </button>
        </div>
      ) : null}
    </>
  );
}

export interface RemoteRelayFieldProps {
  relayUrl: string;
  defaultRelayUrl: string;
  settingsLoading: boolean;
  settingsSaving: boolean;
  relayError: string | null;
  changeRelayUrl: ChangeEventHandler<HTMLInputElement>;
  saveRelayUrl: () => void | Promise<void>;
}

export function RemoteRelayField({
  relayUrl,
  defaultRelayUrl,
  settingsLoading,
  settingsSaving,
  relayError,
  changeRelayUrl,
  saveRelayUrl,
}: RemoteRelayFieldProps) {
  const { t } = useI18n();

  return (
    <div style={styles.field}>
      <label htmlFor="remote-control-relay-url" style={styles.label}>
        {t("settings.remoteControl.relayLabel")}
      </label>
      <input
        id="remote-control-relay-url"
        value={relayUrl}
        placeholder={defaultRelayUrl}
        disabled={settingsLoading || settingsSaving}
        aria-invalid={relayError ? "true" : undefined}
        style={styles.input}
        onChange={(event) => changeRelayUrl(event)}
        onBlur={() => void saveRelayUrl()}
      />
      <span className="st-form-note plain" style={styles.hint}>
        {t("settings.remoteControl.relayHint")}
      </span>
      {relayError ? (
        <span role="alert" style={styles.error}>
          {relayError}
        </span>
      ) : null}
    </div>
  );
}

export interface RemotePairingSectionProps {
  enabled: boolean;
  qrMarkup: string | null;
  pairingPayload: RemotePairingPayload | null;
  pairingStringCopied: boolean;
  pairingAction: "idle" | "generating" | "cancelling";
  relayReady: boolean;
  relaySaving: boolean;
  activeRepoId: string | null;
  pairingStatus: RemotePairingStatus | null;
  pairingError: string | null;
  copyPairingString: () => void | Promise<void>;
  cancelPairing: () => void | Promise<void>;
  beginPairing: () => void | Promise<void>;
}

export function RemotePairingSection({
  enabled,
  qrMarkup,
  pairingPayload,
  pairingStringCopied,
  pairingAction,
  relayReady,
  relaySaving,
  activeRepoId,
  pairingStatus,
  pairingError,
  copyPairingString,
  cancelPairing,
  beginPairing,
}: RemotePairingSectionProps) {
  const { t } = useI18n();

  return (
    <section
      aria-labelledby="remote-control-pairing-title"
      aria-disabled={!enabled}
      style={{
        ...styles.section,
        opacity: enabled ? 1 : 0.52,
      }}
    >
      <div style={styles.sectionHeader}>
        <span id="remote-control-pairing-title" style={styles.sectionTitle}>
          {t("settings.remoteControl.pairingTitle")}
        </span>
        <span style={styles.hint}>
          {t("settings.remoteControl.pairingIntro")}
        </span>
      </div>

      {qrMarkup ? (
        <>
          <div
            style={styles.qrWrap}
            dangerouslySetInnerHTML={{ __html: qrMarkup }}
          />
          <div style={styles.pairingActions}>
            <span style={styles.pairingStatus}>
              {t("settings.remoteControl.validity")}
            </span>
            <button
              type="button"
              className="ob-btn"
              disabled={!pairingPayload}
              onClick={() => void copyPairingString()}
            >
              {t(
                pairingStringCopied
                  ? "settings.remoteControl.copyPairingStringCopied"
                  : "settings.remoteControl.copyPairingString",
              )}
            </button>
            <button
              type="button"
              className="ob-btn"
              disabled={!enabled || pairingAction !== "idle"}
              onClick={() => void cancelPairing()}
            >
              {t(
                pairingAction === "cancelling"
                  ? "settings.remoteControl.cancelling"
                  : "settings.remoteControl.cancel",
              )}
            </button>
          </div>
        </>
      ) : (
        <button
          type="button"
          className="ob-btn primary"
          disabled={
            !enabled ||
            !relayReady ||
            relaySaving ||
            pairingAction !== "idle" ||
            !activeRepoId
          }
          onClick={() => void beginPairing()}
        >
          {t(
            pairingAction === "generating"
              ? "settings.remoteControl.generating"
              : "settings.remoteControl.generate",
          )}
        </button>
      )}

      {pairingStatus?.state === "WaitingForHello" ? (
        <div className="st-test-state ok" aria-live="polite">
          {t("settings.remoteControl.waiting")}
        </div>
      ) : pairingStatus?.state === "Done" ? (
        <div className="st-test-state ok" aria-live="polite">
          {t("settings.remoteControl.done", {
            deviceId: pairingStatus.device_id,
          })}
        </div>
      ) : null}
      {pairingError ? (
        <div role="alert" style={styles.error}>
          {pairingError}
        </div>
      ) : null}
    </section>
  );
}

export interface RemoteDevicesSectionProps {
  devicesError: string | null;
  activeRepoId: string | null;
  devicesLoading: boolean;
  deviceRows: {
    device: RemoteDeviceView;
    createdAt: string;
    expiresAt: string;
  }[];
  revokingDeviceId: string | null;
  setRevokeTarget: (device: RemoteDeviceView) => void;
}

export function RemoteDevicesSection({
  devicesError,
  activeRepoId,
  devicesLoading,
  deviceRows,
  revokingDeviceId,
  setRevokeTarget,
}: RemoteDevicesSectionProps) {
  const { t } = useI18n();

  return (
    <section
      aria-labelledby="remote-control-devices-title"
      style={{ ...styles.section, border: 0, padding: 0 }}
    >
      <div style={styles.sectionHeader}>
        <span id="remote-control-devices-title" style={styles.sectionTitle}>
          {t("settings.remoteControl.devicesTitle")}
        </span>
        <span style={styles.hint}>
          {t("settings.remoteControl.devicesIntro")}
        </span>
      </div>

      {devicesError ? (
        <div role="alert" style={styles.error}>
          {devicesError}
        </div>
      ) : null}
      {!activeRepoId ? (
        <div style={styles.empty}>
          {t("settings.remoteControl.devicesActiveProjectHint")}
        </div>
      ) : devicesLoading ? (
        <div className="ob-sk line w2" />
      ) : deviceRows.length === 0 ? (
        <div style={styles.empty}>
          {t("settings.remoteControl.devicesEmpty")}
        </div>
      ) : (
        <ul style={styles.deviceList}>
          {deviceRows.map(({ device, createdAt, expiresAt }) => (
            <li
              key={device.device_id}
              data-testid={`remote-device-row-${device.device_id}`}
              style={styles.deviceRow}
            >
              <div style={styles.deviceBody}>
                <span style={styles.deviceName}>{device.name}</span>
                <span style={styles.deviceMeta}>
                  {t("settings.remoteControl.createdAt", {
                    date: createdAt,
                  })}
                </span>
                <span style={styles.deviceMeta}>
                  {t("settings.remoteControl.expiresAt", {
                    date: expiresAt,
                  })}
                </span>
              </div>
              <button
                type="button"
                className="ob-btn"
                disabled={revokingDeviceId !== null}
                onClick={() => setRevokeTarget(device)}
              >
                {t(
                  revokingDeviceId === device.device_id
                    ? "settings.remoteControl.revoking"
                    : "settings.remoteControl.revoke",
                )}
              </button>
            </li>
          ))}
        </ul>
      )}
    </section>
  );
}

export interface RemoteDiagnosticsProps {
  gatewayDiagnostics: {
    lastError: string | null;
    counters?: Partial<GatewayCounters>;
  };
}

export function RemoteDiagnostics({
  gatewayDiagnostics,
}: RemoteDiagnosticsProps) {
  const { t } = useI18n();

  return (
    <details style={styles.diagnostics}>
      <summary style={styles.diagnosticsSummary}>
        {t("settings.remoteControl.diagnostics.title")}
      </summary>
      <div style={styles.diagnosticsBody}>
        <span style={styles.hint}>
          {t("settings.remoteControl.diagnostics.description")}
        </span>
        {gatewayDiagnostics.lastError !== null ||
        gatewayDiagnostics.counters !== undefined ? (
          <div style={styles.diagnosticsGrid}>
            {gatewayDiagnostics.lastError !== null ? (
              <>
                <span style={styles.diagnosticsName}>last_error</span>
                <span style={styles.diagnosticsValue}>
                  {gatewayDiagnostics.lastError}
                </span>
              </>
            ) : null}
            {gatewayDiagnostics.counters ? (
              <>
                {GATEWAY_COUNTER_NAMES.map((name) => (
                  <div key={name} style={{ display: "contents" }}>
                    <span style={styles.diagnosticsName}>{name}</span>
                    <span style={styles.diagnosticsValue}>
                      {gatewayDiagnostics.counters?.[name] ?? 0}
                    </span>
                  </div>
                ))}
                <div style={{ display: "contents" }}>
                  <span style={styles.diagnosticsName}>
                    last_disconnect_reason
                  </span>
                  <span style={styles.diagnosticsValue}>
                    {gatewayDiagnostics.counters.last_disconnect_reason || "—"}
                  </span>
                </div>
              </>
            ) : null}
          </div>
        ) : (
          <span style={styles.hint}>
            {t("settings.remoteControl.diagnostics.empty")}
          </span>
        )}
      </div>
    </details>
  );
}

import { useCallback, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { renderBackendError } from "../../lib/backendMsg";
import type { useI18n } from "../../i18n";
import type { RemoteDeviceView } from "./remoteControlTypes";

export function useRemoteControlDevices(t: ReturnType<typeof useI18n>["t"]) {
  const [devices, setDevices] = useState<RemoteDeviceView[]>([]);
  const [devicesLoading, setDevicesLoading] = useState(true);
  const [devicesError, setDevicesError] = useState<string | null>(null);
  const [revokeTarget, setRevokeTarget] = useState<RemoteDeviceView | null>(
    null,
  );
  const [revokingDeviceId, setRevokingDeviceId] = useState<string | null>(null);

  const loadDevices = useCallback(async () => {
    const result = await invoke<RemoteDeviceView[]>("remote_devices_list");
    setDevices(result.filter((device) => device.revoked_at === null));
    setDevicesError(null);
  }, []);

  async function revokeDevice() {
    if (!revokeTarget || revokingDeviceId) return;
    const deviceId = revokeTarget.device_id;
    setRevokingDeviceId(deviceId);
    setDevicesError(null);
    try {
      await invoke<void>("remote_device_revoke", { deviceId });
      setRevokeTarget(null);
      await loadDevices();
    } catch (cause) {
      setDevicesError(renderBackendError(String(cause), t));
    } finally {
      setRevokingDeviceId(null);
    }
  }

  return {
    devices,
    devicesLoading,
    devicesError,
    revokeTarget,
    revokingDeviceId,
    setDevicesLoading,
    setDevicesError,
    setRevokeTarget,
    loadDevices,
    revokeDevice,
  };
}

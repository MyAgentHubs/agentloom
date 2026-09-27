import { useCallback, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import QRCode from "qrcode";
import type { useI18n } from "../../i18n";
import { renderBackendError } from "../../lib/backendMsg";
import type {
  GatewayCounters,
  RemoteGatewayStatus,
  RemotePairingPayload,
  RemotePairingStatus,
} from "./remoteControlTypes";
import {
  QR_ERROR_CORRECTION_LEVEL,
  QR_MARGIN,
  QR_RENDER_SIZE_PX,
  buildPairingQrUrl,
  isValidOrEmptyRelayUrl,
} from "./remoteControlRelayQr";

type Translate = ReturnType<typeof useI18n>["t"];

function usePairingState() {
  const [gatewayStoppedReason, setGatewayStoppedReason] = useState<
    string | null
  >(null);
  const [gatewayStatusError, setGatewayStatusError] = useState<string | null>(
    null,
  );
  const [gatewayDiagnostics, setGatewayDiagnostics] = useState<{
    lastError: string | null;
    counters?: Partial<GatewayCounters>;
  }>({ lastError: null });
  const [pairingStatus, setPairingStatus] =
    useState<RemotePairingStatus | null>(null);
  const [qrMarkup, setQrMarkup] = useState<string | null>(null);
  // The raw payload supports the fallback copy button and future phone-side paste
  // parsing; the QR code contains only the URL.
  const [pairingPayload, setPairingPayload] =
    useState<RemotePairingPayload | null>(null);
  const [pairingStringCopied, setPairingStringCopied] = useState(false);
  const [pairingAction, setPairingAction] = useState<
    "idle" | "generating" | "cancelling"
  >("idle");
  const [pairingError, setPairingError] = useState<string | null>(null);
  const pollingRef = useRef<ReturnType<typeof setInterval> | null>(null);
  const pollingGenerationRef = useRef(0);
  const pairingPollingRef = useRef(false);
  const previousPairingStateRef = useRef<RemotePairingStatus["state"] | null>(
    null,
  );

  return {
    gatewayStoppedReason,
    setGatewayStoppedReason,
    gatewayStatusError,
    setGatewayStatusError,
    gatewayDiagnostics,
    setGatewayDiagnostics,
    pairingStatus,
    setPairingStatus,
    qrMarkup,
    setQrMarkup,
    pairingPayload,
    setPairingPayload,
    pairingStringCopied,
    setPairingStringCopied,
    pairingAction,
    setPairingAction,
    pairingError,
    setPairingError,
    pollingRef,
    pollingGenerationRef,
    pairingPollingRef,
    previousPairingStateRef,
  };
}

function usePairingPolling(
  state: ReturnType<typeof usePairingState>,
  loadDevices: () => Promise<void>,
  setDevicesError: (error: string | null) => void,
  t: Translate,
) {
  const {
    pollingRef,
    pollingGenerationRef,
    pairingPollingRef,
    previousPairingStateRef,
    setGatewayStoppedReason,
    setGatewayDiagnostics,
    setGatewayStatusError,
    setPairingError,
    setPairingStatus,
    setQrMarkup,
    setPairingPayload,
  } = state;

  const stopPolling = useCallback(() => {
    pollingGenerationRef.current += 1;
    if (pollingRef.current !== null) {
      clearInterval(pollingRef.current);
      pollingRef.current = null;
    }
  }, []);

  const refreshRemoteStatus = useCallback(async () => {
    const pollingGeneration = pollingGenerationRef.current;
    try {
      const gatewayStatus = await invoke<RemoteGatewayStatus>(
        "remote_gateway_status",
      );
      if (pollingGeneration !== pollingGenerationRef.current) return;
      setGatewayStoppedReason(gatewayStatus.stopped_reason);
      setGatewayDiagnostics({
        lastError: gatewayStatus.last_error ?? null,
        counters: gatewayStatus.counters,
      });
      setGatewayStatusError(null);
    } catch (cause) {
      if (pollingGeneration !== pollingGenerationRef.current) return;
      setGatewayStatusError(renderBackendError(String(cause), t));
    }

    if (!pairingPollingRef.current) return;

    try {
      const status = await invoke<RemotePairingStatus>("remote_pairing_status");
      if (pollingGeneration !== pollingGenerationRef.current) return;
      setPairingError(null);
      if (status.state === "Idle") {
        pairingPollingRef.current = false;
        previousPairingStateRef.current = "Idle";
        setPairingStatus(null);
        setQrMarkup(null);
        setPairingPayload(null);
        return;
      }

      setPairingStatus(status);
      const enteredDone =
        status.state === "Done" && previousPairingStateRef.current !== "Done";
      previousPairingStateRef.current = status.state;
      if (status.state === "Done") {
        setQrMarkup(null);
        setPairingPayload(null);
      }
      if (enteredDone) {
        try {
          await loadDevices();
        } catch (cause) {
          setDevicesError(renderBackendError(String(cause), t));
        }
      }
    } catch (cause) {
      if (pollingGeneration !== pollingGenerationRef.current) return;
      pairingPollingRef.current = false;
      setPairingError(renderBackendError(String(cause), t));
    }
  }, [loadDevices, t]);

  const startPolling = useCallback(() => {
    if (pollingRef.current !== null) return;
    void refreshRemoteStatus();
    pollingRef.current = setInterval(() => {
      void refreshRemoteStatus();
    }, 3000);
  }, [refreshRemoteStatus]);

  return { stopPolling, refreshRemoteStatus, startPolling };
}

type BeginPairingSettings = {
  enabled: boolean;
  relaySaving: boolean;
  relayUrl: string;
  savedRelayUrl: string;
  defaultRelayUrl: string;
  activeRepoId: string | null;
};

function usePairingActions(
  state: ReturnType<typeof usePairingState>,
  polling: ReturnType<typeof usePairingPolling>,
  t: Translate,
) {
  const {
    pairingPayload,
    setPairingPayload,
    setPairingStringCopied,
    pairingAction,
    setPairingAction,
    setPairingError,
    setPairingStatus,
    setQrMarkup,
    pollingGenerationRef,
    pairingPollingRef,
    previousPairingStateRef,
  } = state;
  const { startPolling, refreshRemoteStatus } = polling;

  async function copyPairingString() {
    if (!pairingPayload) return;
    try {
      await navigator.clipboard?.writeText(JSON.stringify(pairingPayload));
      setPairingStringCopied(true);
      setTimeout(() => setPairingStringCopied(false), 2000);
    } catch {
      // Ignore clipboard failures, as in AboutDialog, without adding an error state.
    }
  }

  async function beginPairing(settings: BeginPairingSettings) {
    const {
      enabled,
      relaySaving,
      relayUrl,
      savedRelayUrl,
      defaultRelayUrl,
      activeRepoId,
    } = settings;
    if (
      !enabled ||
      pairingAction !== "idle" ||
      relaySaving ||
      relayUrl.trim() !== savedRelayUrl ||
      !isValidOrEmptyRelayUrl(savedRelayUrl) ||
      !activeRepoId
    ) {
      return;
    }

    const pollingGeneration = pollingGenerationRef.current;
    setPairingAction("generating");
    setPairingError(null);
    setPairingStatus(null);
    setQrMarkup(null);
    setPairingPayload(null);
    previousPairingStateRef.current = null;
    try {
      // The backend also falls back to the public relay for an empty URL; pass the displayed URL explicitly.
      const payload = await invoke<RemotePairingPayload>(
        "remote_pairing_begin",
        { relayUrl: savedRelayUrl !== "" ? savedRelayUrl : defaultRelayUrl },
      );
      const qrUrl = buildPairingQrUrl(payload);
      if (!qrUrl) {
        setPairingError(t("settings.remoteControl.qrEncodeFailed"));
        // Pairing has already registered a token and left the slot Waiting. Cancel it
        // so it does not remain for five minutes with no visible cancel button.
        // Keep the QR error if cancellation fails.
        try {
          await invoke<void>("remote_pairing_cancel");
        } catch {
          // Keep the QR error displayed above.
        }
        return;
      }
      // Specify ECC and margin in an object passed to QRCode.toString. Inlining
      // them at the call site fails the narrow ambient SvgOptions excess-property check.
      const qrOptions = {
        type: "svg" as const,
        errorCorrectionLevel: QR_ERROR_CORRECTION_LEVEL,
        margin: QR_MARGIN,
        width: QR_RENDER_SIZE_PX,
      };
      const markup = await QRCode.toString(qrUrl, qrOptions);
      if (pollingGeneration !== pollingGenerationRef.current) return;
      setQrMarkup(markup);
      setPairingPayload(payload);
      setPairingStatus({
        state: "WaitingForHello",
        expires_at: Math.floor(Date.now() / 1000) + 300,
      });
      pairingPollingRef.current = true;
      startPolling();
      void refreshRemoteStatus();
    } catch (cause) {
      setPairingError(renderBackendError(String(cause), t));
    } finally {
      setPairingAction("idle");
    }
  }

  async function cancelPairing() {
    if (pairingAction !== "idle") return;
    setPairingAction("cancelling");
    setPairingError(null);
    try {
      await invoke<void>("remote_pairing_cancel");
      pairingPollingRef.current = false;
      previousPairingStateRef.current = "Idle";
      setPairingStatus(null);
      setQrMarkup(null);
      setPairingPayload(null);
    } catch (cause) {
      setPairingError(renderBackendError(String(cause), t));
    } finally {
      setPairingAction("idle");
    }
  }

  return { copyPairingString, beginPairing, cancelPairing };
}

export function useRemoteControlPairing(
  loadDevices: () => Promise<void>,
  setDevicesError: (error: string | null) => void,
  t: Translate,
) {
  const state = usePairingState();
  const polling = usePairingPolling(state, loadDevices, setDevicesError, t);
  const actions = usePairingActions(state, polling, t);
  return { ...state, ...polling, ...actions };
}

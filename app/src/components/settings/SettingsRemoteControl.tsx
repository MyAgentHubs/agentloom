import { useCallback, useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import QRCode from "qrcode";
import type { CSSProperties } from "react";
import { useI18n } from "../../i18n";
import { renderBackendError } from "../../lib/backendMsg";
import { ConfirmDialog } from "../ConfirmDialog";
import type { RepoMeta } from "../../types/agent";

type RemoteControlSettings = {
  enabled: boolean;
  /** 存储的原始值——可为空（空 = 未自定义，网关/配对侧自行兜底到官方公共中继）。 */
  relay_url: string;
  /** relay 留空时实际生效的官方公共中继地址（恒为后端 `DEFAULT_PUBLIC_RELAY_URL` 常量值）。 */
  default_relay_url: string;
  /** M2-4d：`None`（后端 `Option<String>` 序列化为 `null`）= 未设置活跃项目。 */
  active_repo_id: string | null;
};

export type RemotePairingPayload = {
  v: number;
  relay_url: string;
  room: string;
  pairing_token: string;
  desktop_pub: string;
};

type RemotePairingStatus =
  | { state: "Idle" }
  | { state: "WaitingForHello"; expires_at: number }
  | { state: "Done"; device_id: string };

type RemoteGatewayStatus = {
  running: boolean;
  stopped_reason: string | null;
  /** Optional while a newer frontend is briefly paired with an older desktop IPC. */
  last_error?: string | null;
  counters?: Partial<GatewayCounters>;
};

const GATEWAY_COUNTER_NAMES = [
  "frames_seen",
  "frames_sent",
  "keepalive_pings_sent",
  "bad_frames",
  "upstream_dropped",
  "upstream_stale_generation_dropped",
  "upstream_budget_dropped",
  "milestone_dropped",
  "session_index_snapshot_unavailable",
  "snapshot_worker_spawn_count",
  "tool_correlation_dropped",
  "classify_skipped",
  "connection_failures",
  "panics",
  "disconnect_config_stale",
  "disconnect_closed_by_peer",
  "disconnect_error",
  "upstream_repo_filtered",
  "partial_snapshot_capacity_dropped",
  "snapshot_oversized_dropped",
] as const;

type GatewayCounters = Record<
  (typeof GATEWAY_COUNTER_NAMES)[number],
  number
> & {
  last_disconnect_reason: string;
};

type RemoteDeviceView = {
  device_id: string;
  name: string;
  created_at: number;
  access_expires_at: number;
  revoked_at: number | null;
};

const styles = {
  root: {
    display: "flex",
    flexDirection: "column",
    gap: 14,
    maxWidth: 760,
  },
  header: {
    alignItems: "flex-start",
    gap: 16,
    justifyContent: "space-between",
    marginBottom: 0,
  },
  headerCopy: {
    display: "flex",
    flexDirection: "column",
    gap: 3,
    minWidth: 0,
  },
  description: {
    color: "var(--ink-3)",
    fontSize: 11.5,
    lineHeight: 1.5,
    maxWidth: 620,
  },
  switchGroup: {
    alignItems: "center",
    display: "flex",
    flexShrink: 0,
    gap: 8,
  },
  switchStatus: {
    color: "var(--ink-2)",
    fontSize: 11,
    whiteSpace: "nowrap",
  },
  switch: {
    border: 0,
    borderRadius: 999,
    cursor: "pointer",
    height: 24,
    padding: 2,
    transition: "background 120ms ease",
    width: 42,
  },
  switchKnob: {
    background: "white",
    borderRadius: "50%",
    boxShadow: "0 1px 3px rgba(0, 0, 0, 0.3)",
    display: "block",
    height: 20,
    transition: "transform 120ms ease",
    width: 20,
  },
  field: {
    display: "flex",
    flexDirection: "column",
    gap: 5,
  },
  label: {
    color: "var(--ink-2)",
    fontSize: 11,
    fontWeight: 600,
  },
  input: {
    background: "var(--panel)",
    border: "1px solid var(--line)",
    borderRadius: 6,
    color: "var(--ink)",
    fontFamily: '"SF Mono", monospace',
    fontSize: 11.5,
    padding: "7px 10px",
    width: "100%",
  },
  // M24DF 项 7：活跃项目 select 不该等宽——项目名是人类可读文本，不是 relay URL / token 这类
  // 需要等宽对齐的值。除 fontFamily 外尺寸/边框与 `input` 保持一致的视觉观感。
  selectInput: {
    background: "var(--panel)",
    border: "1px solid var(--line)",
    borderRadius: 6,
    color: "var(--ink)",
    fontSize: 11.5,
    padding: "7px 10px",
    width: "100%",
  },
  hint: {
    color: "var(--ink-3)",
    fontSize: 10.5,
    lineHeight: 1.45,
  },
  error: {
    color: "var(--red)",
    fontSize: 11,
    lineHeight: 1.45,
  },
  section: {
    border: "1px solid var(--line)",
    borderRadius: 8,
    display: "flex",
    flexDirection: "column",
    gap: 10,
    padding: 14,
    transition: "opacity 120ms ease",
  },
  sectionHeader: {
    display: "flex",
    flexDirection: "column",
    gap: 3,
  },
  sectionTitle: {
    color: "var(--ink)",
    fontSize: 12.5,
    fontWeight: 650,
  },
  pairingActions: {
    alignItems: "center",
    display: "flex",
    flexWrap: "wrap",
    gap: 8,
  },
  qrWrap: {
    alignItems: "center",
    alignSelf: "flex-start",
    background: "white",
    border: "1px solid var(--line)",
    borderRadius: 8,
    display: "flex",
    justifyContent: "center",
    // P0-a1：QR 渲染容器最小 240×240 CSS px（钉死数值，别自创）。
    minWidth: 240,
    minHeight: 240,
    padding: 8,
  },
  pairingStatus: {
    color: "var(--ink-2)",
    fontSize: 11,
  },
  deviceList: {
    display: "flex",
    flexDirection: "column",
    gap: 7,
    listStyle: "none",
    margin: 0,
    padding: 0,
  },
  deviceRow: {
    alignItems: "center",
    border: "1px solid var(--line)",
    borderRadius: 7,
    display: "flex",
    gap: 12,
    padding: "10px 12px",
  },
  deviceBody: {
    display: "flex",
    flex: 1,
    flexDirection: "column",
    gap: 3,
    minWidth: 0,
  },
  deviceName: {
    color: "var(--ink)",
    fontSize: 12.5,
    fontWeight: 600,
    overflow: "hidden",
    textOverflow: "ellipsis",
    whiteSpace: "nowrap",
  },
  deviceMeta: {
    color: "var(--ink-3)",
    fontSize: 10.5,
    lineHeight: 1.4,
  },
  empty: {
    color: "var(--ink-3)",
    fontSize: 12,
    padding: "12px 0 4px",
  },
  diagnostics: {
    borderTop: "1px solid var(--line)",
    paddingTop: 10,
  },
  diagnosticsSummary: {
    color: "var(--ink-2)",
    cursor: "pointer",
    fontSize: 11.5,
    fontWeight: 600,
  },
  diagnosticsBody: {
    display: "flex",
    flexDirection: "column",
    gap: 8,
    paddingTop: 8,
  },
  diagnosticsGrid: {
    display: "grid",
    gap: "4px 14px",
    gridTemplateColumns: "minmax(0, 1fr) auto",
  },
  diagnosticsName: {
    color: "var(--ink-3)",
    fontFamily: '"SF Mono", monospace',
    fontSize: 10,
    overflowWrap: "anywhere",
  },
  diagnosticsValue: {
    color: "var(--ink-2)",
    fontFamily: '"SF Mono", monospace',
    fontSize: 10,
    overflowWrap: "anywhere",
    textAlign: "right",
  },
} satisfies Record<string, CSSProperties>;

// P0-a1：数值全部钉死（spec `2026-08-12-remote-control-m2-c1-web-client.md` §0 决策 1 +
// worker 任务书 M24DF §B），实现不许自创。
const QR_MAX_URL_BYTES = 1024;
const QR_ERROR_CORRECTION_LEVEL = "M";
const QR_MARGIN = 4;
// 真机踩坑：qrcode 库不传 width 时生成的 svg 只带 viewBox，没有显式 width/height 属性——
// 这种 svg 在 WKWebView（Tauri 桌面 webview）的 flex 容器（qrWrap）里量不出内在尺寸，直接
// 塌成 0×0；jsdom 测试没有真实布局，测不出这个问题。显式钉死渲染尺寸让库把 width/height
// 属性写进 svg 标签解决。qrWrap 容器 min 240px 减去两侧 8px padding = 224，容器数值 240
// 是钉死的，这里跟着它推。
const QR_RENDER_SIZE_PX = 224;

/** P0-a1：relay_url 校验收紧——不只查 `wss://` 前缀，还要求可 parse、无 userinfo、
 *  host 非空、pathname 为根路径或空、无 query/fragment。返回 parsed `URL` 供后续派生 origin 用，
 *  校验失败统一返回 `null`。 */
function parseValidRelayUrl(value: string): URL | null {
  let url: URL;
  try {
    url = new URL(value);
  } catch {
    return null;
  }
  if (url.protocol !== "wss:") return null;
  if (url.username !== "" || url.password !== "") return null;
  if (!url.host) return null;
  if (url.pathname !== "/" && url.pathname !== "") return null;
  if (url.search !== "") return null;
  if (url.hash !== "") return null;
  // M24DF 项 2：canonical 等值收口——上面的逐属性检查各自查的是"解析后的属性是不是
  // 空"，堵不住"输入串本身带着噪音、被 WHATWG 归一化悄悄吃掉"的情况：空 userinfo
  // （`wss://@host` → href 里 `@` 消失）、空 query（`wss://host?` → 去掉 `?`）、空 hash
  // （`wss://host#` → 去掉 `#`），以及多斜杠/反斜杠这类宽容形态。这里直接比对 canonical
  // href 与原始输入是否等值（唯一允许的差异是 WHATWG 给根路径补的尾随 `/`），字符层面
  // 必须原样还原。有意收紧：大写 scheme/host（`WSS://Host`）也会被这条拒绝——因为
  // WHATWG 会把它们小写化，href 与原始输入不再逐字符相等；relay URL 只在设置里填一次，
  // 安全边界从严不算成本。
  if (url.href !== value && url.href !== `${value}/`) return null;
  // M24DF 微返工第 3 轮：canonical 等值检查仍有一条漏网——`wss://host/?`、`wss://host/#`
  // 这两种"斜杠已经在、标记本身留空"的写法，WHATWG 归一化后 href 与原始输入逐字符相等
  // （不会补斜杠、也不会吃掉这个空标记），上面的 `url.search`/`url.hash` 属性检查读到的
  // 都是空字符串（空 query/fragment 的 getter 契约就是返回 ""），canonical 比对也测不出
  // 差异，两条检查一起放行。合法的 relay URL（根路径、无 query、无 hash）原始字符串里
  // 永远不该出现 `?` 或 `#` 字符，直接在原始输入里查这两个字符最直接、不依赖解析结果。
  if (value.includes("?") || value.includes("#")) return null;
  return url;
}

function isValidRelayUrl(value: string): boolean {
  return parseValidRelayUrl(value) !== null;
}

/** 内置公共中继单：relay 地址允许留空（空 = 用官方公共中继，后端 `effective_relay_url` 兜底），
 *  非空则仍要求通过 `isValidRelayUrl` 的严格 `wss://` 校验。 */
function isValidOrEmptyRelayUrl(value: string): boolean {
  return value === "" || isValidRelayUrl(value);
}

/** P0-a1：wss → https 派生的唯一规则——host（含端口）原样接到 `https://` 后面，不做任何
 *  宽容映射（比如把默认端口特殊处理）。 */
function relayHttpsOrigin(url: URL): string {
  return `https://${url.host}`;
}

/** base64url（RFC 4648 §5，无 padding）。先转 UTF-8 字节再编码——别假设 payload 全 ASCII。 */
export function base64UrlEncode(text: string): string {
  const bytes = new TextEncoder().encode(text);
  let binary = "";
  for (const byte of bytes) binary += String.fromCharCode(byte);
  return btoa(binary)
    .replace(/\+/g, "-")
    .replace(/\//g, "_")
    .replace(/=+$/, "");
}

/** P0-a1：QR 内容 = `https://<relay host>/#p=<base64url(payload)>`。relay_url 未过严格校验，
 *  或总 URL 长度超 1024 字节，一律返回 `null`——调用方据此显示配对错误文案，不出降级码
 *  （不落回裸 JSON 编码）。
 *
 *  M24DF 项 1：origin 只从编码进 QR 本体的 `payload.relay_url` 派生——不接受调用方另传一个
 *  `relayUrl` 参数。桌面本地保存的 relay URL 和后端 `remote_pairing_begin` 回抄进 payload 的
 *  `relay_url` 今天恰好同源，但那只是构造巧合、没有断言钉死；QR 里编码的内容才是手机端真正
 *  会解析、连接的那份，origin 派生必须跟它一一对应，不能悄悄依赖另一个"恰好相同"的变量。 */
export function buildPairingQrUrl(
  payload: RemotePairingPayload,
): string | null {
  const parsedRelay = parseValidRelayUrl(payload.relay_url);
  if (!parsedRelay) return null;
  const origin = relayHttpsOrigin(parsedRelay);
  const encoded = base64UrlEncode(JSON.stringify(payload));
  const url = `${origin}/#p=${encoded}`;
  const byteLength = new TextEncoder().encode(url).length;
  if (byteLength > QR_MAX_URL_BYTES) return null;
  return url;
}

export function SettingsRemoteControl() {
  const { locale, t } = useI18n();
  const [enabled, setEnabled] = useState(false);
  const [relayUrl, setRelayUrl] = useState("");
  const savedRelayUrlRef = useRef("");
  // relay 留空时实际生效的官方公共中继地址——从设置读接口取得，只用于展示（input placeholder
  // + 配对调用兜底），不是另一份可写状态。加载完成前留空，settings 到达后立刻回填。
  const [defaultRelayUrl, setDefaultRelayUrl] = useState("");
  const [settingsLoading, setSettingsLoading] = useState(true);
  const [settingsSaving, setSettingsSaving] = useState(false);
  const [relaySaving, setRelaySaving] = useState(false);
  const [settingsError, setSettingsError] = useState<string | null>(null);
  const [relayError, setRelayError] = useState<string | null>(null);
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

  // M2-4d：活跃项目——按当前项目的配对/设备 UI。
  const [repos, setRepos] = useState<RepoMeta[]>([]);
  const [activeRepoId, setActiveRepoId] = useState<string | null>(null);
  const [activeProjectSaving, setActiveProjectSaving] = useState(false);
  const [activeProjectError, setActiveProjectError] = useState<string | null>(
    null,
  );

  const [pairingStatus, setPairingStatus] =
    useState<RemotePairingStatus | null>(null);
  const [qrMarkup, setQrMarkup] = useState<string | null>(null);
  // P0-a1：兜底「复制配对串」按钮用的裸 payload（QR 本体只放 URL，这个是留给未来手机页粘贴解析）。
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

  const [devices, setDevices] = useState<RemoteDeviceView[]>([]);
  const [devicesLoading, setDevicesLoading] = useState(true);
  const [devicesError, setDevicesError] = useState<string | null>(null);
  const [revokeTarget, setRevokeTarget] = useState<RemoteDeviceView | null>(
    null,
  );
  const [revokingDeviceId, setRevokingDeviceId] = useState<string | null>(null);

  const stopPolling = useCallback(() => {
    pollingGenerationRef.current += 1;
    if (pollingRef.current !== null) {
      clearInterval(pollingRef.current);
      pollingRef.current = null;
    }
  }, []);

  const loadDevices = useCallback(async () => {
    const result = await invoke<RemoteDeviceView[]>("remote_devices_list");
    setDevices(result.filter((device) => device.revoked_at === null));
    setDevicesError(null);
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
      await refreshRemoteStatus();
      // DEVLIST 返工·项 2：切活跃项目成功后必须重拉设备列表——后端 remote_devices_list 按
      // active 房过滤（见 lib.rs remote_devices_list_in_conn），不重拉的话旧房的设备行会一直
      // 挂在 UI 上、看着能点撤销，实际 revoke 解析不到新房时不排 relay token.delete（只在本
      // 地生效），点了也撤不干净 relay 那侧——这里复用现有 loadDevices，不新造一条拉取逻辑。
      // 单独 try/catch：设备列表拉取失败不该回滚已经成功的项目切换。
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

  async function copyPairingString() {
    if (!pairingPayload) return;
    try {
      await navigator.clipboard?.writeText(JSON.stringify(pairingPayload));
      setPairingStringCopied(true);
      setTimeout(() => setPairingStringCopied(false), 2000);
    } catch {
      // 剪贴板写入失败——静默忽略，跟 AboutDialog 的复制惯例一致，不新增错误态噪音。
    }
  }

  async function beginPairing() {
    const savedRelayUrl = savedRelayUrlRef.current;
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
      // 后端 `remote_pairing_begin` 已对空 relay_url 兜底到官方公共中继（`effective_relay_url`）；
      // 这里让前端传参跟展示一致，不依赖后端兜底作为唯一真相。
      const payload = await invoke<RemotePairingPayload>(
        "remote_pairing_begin",
        { relayUrl: savedRelayUrl !== "" ? savedRelayUrl : defaultRelayUrl },
      );
      const qrUrl = buildPairingQrUrl(payload);
      if (!qrUrl) {
        setPairingError(t("settings.remoteControl.qrEncodeFailed"));
        // M24DF 项 5：出码失败前，后端配对槽已经 `remote_pairing_begin` 成功、token 已注册、
        // 停在 `Waiting`——这里不 best-effort 取消的话，槽位会悬空到 5 分钟自然过期，且 UI
        // 此刻没有取消入口。失败静默吞掉：不能让取消槽位失败反而遮住上面已经在展示的
        // `qrEncodeFailed` 错误文案。
        try {
          await invoke<void>("remote_pairing_cancel");
        } catch {
          // 静默——见上方注释。
        }
        return;
      }
      // P0-a1：ECC/margin 显式钉死传给 QRCode 库——不走变量直接放对象字面量到函数调用位，
      // 避免 TS excess-property check 卡在 ambient `qrcode.d.ts`（本单 forbidden 之外的文件）
      // 声明的窄 `SvgOptions` 上。
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

  const normalizedRelayUrl = relayUrl.trim();
  const relayReady =
    normalizedRelayUrl === savedRelayUrlRef.current &&
    isValidOrEmptyRelayUrl(normalizedRelayUrl);
  const gatewayStoppedMessage =
    gatewayStoppedReason === "room_claim_conflict"
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
  const dateLocale = locale === "zh" ? "zh-CN" : "en-US";
  const formatTime = (milliseconds: number) =>
    new Date(milliseconds).toLocaleString(dateLocale);

  return (
    <div style={styles.root}>
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
          // B5（backlog 跟进）：切换项目会断连"已配对的手机"这件事，只在真的存在已配对
          // 设备时才成立——零设备时提示切换风险是"文案强于事实"（review 定性），复用
          // activeProjectHint 这条一直为真的背景说明（配对/设备归属当前活跃项目的房间），
          // 不展示只在有设备时才有意义的断连警告。
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
          onChange={(event) => {
            setRelayUrl(event.target.value);
            setRelayError(null);
          }}
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
        ) : devices.length === 0 ? (
          <div style={styles.empty}>
            {t("settings.remoteControl.devicesEmpty")}
          </div>
        ) : (
          <ul style={styles.deviceList}>
            {devices.map((device) => (
              <li
                key={device.device_id}
                data-testid={`remote-device-row-${device.device_id}`}
                style={styles.deviceRow}
              >
                <div style={styles.deviceBody}>
                  <span style={styles.deviceName}>{device.name}</span>
                  <span style={styles.deviceMeta}>
                    {t("settings.remoteControl.createdAt", {
                      date: formatTime(device.created_at * 1000),
                    })}
                  </span>
                  <span style={styles.deviceMeta}>
                    {t("settings.remoteControl.expiresAt", {
                      date: formatTime(device.access_expires_at),
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
                      {gatewayDiagnostics.counters.last_disconnect_reason ||
                        "—"}
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
    </div>
  );
}

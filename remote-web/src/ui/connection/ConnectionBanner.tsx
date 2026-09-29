import { useEffect, useReducer, type CSSProperties } from "react";
import type { ConnectionSessionPhase } from "../../connection/types.ts";
import { useI18n, type Locale } from "../i18n.ts";

/**
 * C1-PS（dogfood 修障第二批·手机发消息桌面离线无反馈）——`app/AppRuntime.tsx` 消费 relay 定向
 * 回的 presence 快照/广播（`{t:"presence", role:"desktop", event}`，见
 * `remote-relay/src/room-do.js::sendDesktopPresenceSnapshot`/`broadcastPresence`）后维护的桌面
 * 在线态。`"unknown"` 是初始值——快照到达前、以及每次连接断开重连期间都会回到这个态（等新快照）；
 * presence 是 per-socket、无 device 标识的弱担保信号（M2 C1 spec §…G10），措辞与 UI 呈现都必须
 * 是"可能不在线"而不是断言。
 */
export type DesktopPresence = "online" | "offline" | "unknown";

export interface ConnectionBannerProps {
  phase: ConnectionSessionPhase;
  phaseChangedAtMs: number;
  /** 省略/`"unknown"` 时行为与旧版完全一致（`phase==="open"` 就零渲染）——只有确认 `"offline"`
   *  时才在已连接状态下额外提示"电脑可能不在线"。 */
  desktopPresence?: DesktopPresence;
  now?: () => number;
  locale?: Locale;
}

const LONG_DISCONNECTION_THRESHOLD_SECONDS = 60;

const bannerStyle: CSSProperties = {
  width: "100%",
  padding: "6px var(--content-padding)",
  borderBottom: "1px solid var(--line)",
  background: "var(--accent-soft)",
  color: "var(--amber-ink)",
  fontSize: "12px",
  lineHeight: 1.45,
  textAlign: "center",
};

export function ConnectionBanner({
  phase,
  phaseChangedAtMs,
  desktopPresence = "unknown",
  now = Date.now,
  locale,
}: ConnectionBannerProps) {
  const { t } = useI18n(locale);
  const [, tick] = useReducer((value: number) => value + 1, 0);
  const tracksDuration = phase === "connecting" || phase === "reconnect_scheduled";

  useEffect(() => {
    if (!tracksDuration) return;
    const timer = setInterval(tick, 1_000);
    return () => clearInterval(timer);
  }, [tracksDuration]);

  if (phase === "open") {
    // C1-PS：连接本身是好的（能收发帧），但 relay 认为桌面此刻不在线——弱担保提示，不是错误横幅
    // （没有 data-status="error" 这类强断言，措辞见 i18n.ts `connection.desktopMaybeOffline`）。
    // online/unknown 两态都零渲染，与旧版行为一致。
    if (desktopPresence !== "offline") return null;
    return (
      <div
        role="status"
        aria-live="polite"
        data-testid="connection-banner"
        data-phase={phase}
        data-desktop-presence={desktopPresence}
        style={bannerStyle}
      >
        {t("connection.desktopMaybeOffline")}
      </div>
    );
  }

  const elapsedSeconds = Math.max(0, Math.floor((now() - phaseChangedAtMs) / 1_000));
  let label: string;
  switch (phase) {
    case "connecting":
      label = t("connection.connecting", { seconds: String(elapsedSeconds) });
      break;
    case "reconnect_scheduled":
      label = t("connection.reconnecting", { seconds: String(elapsedSeconds) });
      break;
    case "needs_repair":
      label = t("connection.needsRepair");
      break;
    case "idle":
    case "closed":
      label = t("connection.disconnected");
      break;
  }

  const showLongDisconnectionHint = tracksDuration && elapsedSeconds >= LONG_DISCONNECTION_THRESHOLD_SECONDS;
  return (
    <div role="status" aria-live="polite" data-testid="connection-banner" data-phase={phase} style={bannerStyle}>
      {label}
      {showLongDisconnectionHint && <> · {t("connection.longDisconnectionHint")}</>}
    </div>
  );
}

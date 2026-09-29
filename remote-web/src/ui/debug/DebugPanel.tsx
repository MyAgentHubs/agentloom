import type { CSSProperties } from "react";
import type { FrameDiagnosticsSnapshot } from "../../app/appRuntimeCore.ts";
import type { LogLevel, UpgradeFailureClassification } from "../../connection/types.ts";
import { redact } from "../../connection/redact.ts";

const MAX_CONNECTION_EVENTS = 20;

export interface ConnectionDiagnosticEvent {
  atMs: number;
  name: string;
}

export interface ConnectionDiagnosticsSnapshot {
  events: ConnectionDiagnosticEvent[];
  lastClose: { code: number; reason: string } | null;
  lastResult: string | null;
}

export function createConnectionDiagnostics(): ConnectionDiagnosticsSnapshot {
  return { events: [], lastClose: null, lastResult: null };
}

function isClassification(value: unknown): value is UpgradeFailureClassification {
  return value === "retry_backoff" || value === "needs_refresh" || value === "needs_repair";
}

/**
 * ConnectionSession 的日志 context 可能包含内部错误对象；诊断面板只白名单提取关闭信息和分类枚举。
 * message/reason 已在 ConnectionSession 出口脱敏，这里再次处理，避免测试或未来调用方绕过该出口。
 */
export function recordConnectionLog(
  current: ConnectionDiagnosticsSnapshot,
  level: LogLevel,
  message: string,
  context?: Record<string, unknown>,
  atMs = Date.now(),
): ConnectionDiagnosticsSnapshot {
  const name = redact(message);
  const events = [...current.events, { atMs, name }].slice(-MAX_CONNECTION_EVENTS);
  let lastClose = current.lastClose;
  let lastResult = current.lastResult;

  if (name === "connection closed" && typeof context?.code === "number" && typeof context.reason === "string") {
    lastClose = { code: context.code, reason: redact(context.reason) };
  }
  if (isClassification(context?.classification)) {
    lastResult = context.classification;
  } else if (level === "error") {
    lastResult = name;
  }

  return { events, lastClose, lastResult };
}

export interface DebugPanelProps {
  diagnostics: FrameDiagnosticsSnapshot;
  connectionDiagnostics: ConnectionDiagnosticsSnapshot;
}

export function isDebugPanelEnabled(search = typeof window === "undefined" ? "" : window.location.search): boolean {
  return new URLSearchParams(search).getAll("debug").includes("1");
}

const panelStyle: CSSProperties = {
  position: "fixed",
  right: "8px",
  bottom: "calc(6rem + env(safe-area-inset-bottom, 0px))",
  zIndex: 1000,
  maxWidth: "min(18rem, calc(100vw - 16px))",
  maxHeight: "42vh",
  overflow: "auto",
  padding: "6px 8px",
  border: "1px solid var(--line-soft)",
  borderRadius: "6px",
  background: "var(--panel)",
  opacity: 0.9,
  color: "var(--ink)",
  fontFamily: "ui-monospace, SFMono-Regular, Menlo, Consolas, monospace",
  fontSize: "10px",
  lineHeight: 1.35,
  pointerEvents: "none",
};

const rowStyle: CSSProperties = {
  display: "grid",
  gridTemplateColumns: "minmax(0, 1fr) auto",
  gap: "10px",
};

function formatEventTime(atMs: number): string {
  const date = new Date(atMs);
  return [date.getHours(), date.getMinutes(), date.getSeconds()]
    .map((part) => String(part).padStart(2, "0"))
    .join(":");
}

export function DebugPanel({ diagnostics, connectionDiagnostics }: DebugPanelProps) {
  if (!isDebugPanelEnabled()) return null;

  const rows: Array<[string, string | number]> = [
    ["framesSeen", diagnostics.framesSeen],
    ["plaintextControl", diagnostics.plaintextControl],
    ["kindSkipped", diagnostics.kindSkipped],
    ["decryptFailed", diagnostics.decryptFailed],
    ["parseFailed", diagnostics.parseFailed],
    ["routingRejected", diagnostics.routingRejected],
    ["missingIds", diagnostics.missingIds],
    ["storeError", diagnostics.storeError],
    ["storeDuplicate", diagnostics.storeDuplicate],
    ["applied", diagnostics.applied],
    ["milestoneSessionNull", diagnostics.milestoneSessionNull],
    ["snapshotRejected", diagnostics.snapshotRejected],
    ["defensiveDefault", diagnostics.defensiveDefault],
    ["liveSessionNull", diagnostics.liveSessionNull],
    ["liveUnknownDelta", diagnostics.liveUnknownDelta],
    ["liveDroppedNoRun", diagnostics.liveDroppedNoRun],
    ["liveWatermarkRejected", diagnostics.liveWatermarkRejected],
    ["parseReason", diagnostics.lastParseFailedReason ?? "—"],
    [
      "lastDrop",
      `${diagnostics.lastDroppedFrameT ?? "—"} / ${diagnostics.lastDroppedKind ?? "—"}${
        diagnostics.lastDroppedErrorMessage === null ? "" : ` / ${diagnostics.lastDroppedErrorMessage}`
      }`,
    ],
  ];

  return (
    <aside aria-label="Process frame diagnostics" data-testid="debug-panel" style={panelStyle}>
      {rows.map(([name, value]) => (
        <div key={name} style={rowStyle}>
          <span>{name}</span>
          <output data-testid={`debug-${name}`}>{value}</output>
        </div>
      ))}
      <section aria-label="Connection diagnostics">
        <div style={rowStyle}>
          <span>lastWsClose</span>
          <output data-testid="debug-lastWsClose">
            {connectionDiagnostics.lastClose
              ? `${connectionDiagnostics.lastClose.code} / ${connectionDiagnostics.lastClose.reason || "—"}`
              : "—"}
          </output>
        </div>
        <div style={rowStyle}>
          <span>lastResult</span>
          <output data-testid="debug-lastConnectionResult">{connectionDiagnostics.lastResult ?? "—"}</output>
        </div>
        <div>connectionEvents</div>
        <ol aria-label="Recent connection events">
          {connectionDiagnostics.events.map((event, index) => (
            <li key={`${event.atMs}:${index}`} data-testid="debug-connectionEvent">
              <time dateTime={new Date(event.atMs).toISOString()}>{formatEventTime(event.atMs)}</time>{" "}
              {event.name}
            </li>
          ))}
        </ol>
      </section>
    </aside>
  );
}

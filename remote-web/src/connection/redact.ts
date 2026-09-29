// redact.ts — T6c-refresh · 日志脱敏（M0 §9.8 日志纪律 + M2 C1 spec §3 v0.5 块「日志脱敏」段）。
//
// 权威参照（只读对照，未改动）：`app/src-tauri/src/remote_gateway.rs::redact()`/`scrub_after_marker`
// ——桌面侧用「marker（`Bearer `/`token.`/`token=`）后跟着的十六进制串」判定要不要打码，附
// `MIN_SCRUBBED_HEX_LEN = 32` 门槛（避免误伤 `token.ack`/`token.refresh_failed` 这类短诊断串）。
//
// **本实现的偏离（有意，理由见下）**：不照抄「只在特定 marker 后面找」的写法，改成「扫描整条
// 字符串里任何长度 ≥32 的连续十六进制字符游程，一律打码」——直接对齐 spec 原文字面："内部 log
// 回调出口一律过 `redact()`（hex≥32 打码…）"，比 marker 版本更宽（能覆盖 marker 版本设计时没预料到
// 的新日志形状，例如把 `access`/`refresh`/K_room 相关十六进制字符串直接拼进 error message 而不带
// `Bearer `/`token.` 前缀的情况——这正是 C1 侧最可能出现的新日志形状，因为浏览端没有桌面那套
// `Authorization`/`Sec-WebSocket-Protocol` header 拼接惯例）。32 字符门槛与桌面一致，理由相同：
// 真实凭据（access/refresh/desktop credential/K_room 原始 hex 表示）全部是 hex64，短哈希前缀/诊断
// 字面量（如 `token.ack` 的 `ack`）不会撞到 32 字符门槛。

const MIN_REDACTED_HEX_LEN = 32;
const HEX_RUN_RE = /[0-9a-fA-F]+/g;

/**
 * 打码任意长度 ≥32 的连续十六进制字符游程为 `***`；更短的十六进制串原样保留（避免误伤诊断串里的
 * 短片段，如 `t=token.ack`/`reason=in_flight` 这类不含真凭据的短标识符）。不区分大小写，因为
 * hex64 令牌一律小写产出（M0 §9.1），但攻击面/误粘贴场景不该假设输入永远规范。
 */
export function redact(input: string): string {
  return input.replace(HEX_RUN_RE, (run) => (run.length >= MIN_REDACTED_HEX_LEN ? "***" : run));
}

/** 便捷包装：先 `redact()` 消息，再交给注入的 `LogSink`（`connectionSession.ts` 的唯一日志出口）。 */
export function redactedLog(
  sink: (level: "debug" | "info" | "warn" | "error", message: string, context?: Record<string, unknown>) => void,
  level: "debug" | "info" | "warn" | "error",
  message: string,
  context?: Record<string, unknown>,
): void {
  const redactedMessage = redact(message);
  const redactedContext = context ? redactRecord(context) : undefined;
  sink(level, redactedMessage, redactedContext);
}

function redactRecord(context: Record<string, unknown>): Record<string, unknown> {
  const out: Record<string, unknown> = {};
  for (const [key, value] of Object.entries(context)) {
    out[key] = typeof value === "string" ? redact(value) : value;
  }
  return out;
}

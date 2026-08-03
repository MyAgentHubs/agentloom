import type { Block, ChatMessage } from "../types/agent";
import type { I18nKey } from "../i18n";

type Translate = (
  key: I18nKey,
  values?: Record<string, string | number>,
) => string;

const PREVIEW_MAX_CHARS = 60;
const BLOCK_MAX_LINES = 3;
const BLOCK_MAX_CHARS = 180;
const TOOLTIP_MAX_CHARS = 280;

function toolText(block: Extract<Block, { type: "tool" }>): string {
  const status =
    block.exit_code !== null
      ? `[${block.status} exit ${block.exit_code}]`
      : `[${block.status}]`;
  const lines = [`$ ${block.summary}`, status];
  if (block.output) lines.push(block.output);
  return lines.join("\n");
}

function nonEmpty(value?: string | null): string | undefined {
  return value && value.trim() !== "" ? value : undefined;
}

/** 决定「引什么」：text 优先 → 纯工具卡兜底 → 空。message 级。 */
export function quotableSource(message: ChatMessage): string {
  const text = message.content
    .filter((b): b is Extract<Block, { type: "text" }> => b.type === "text")
    .map((b) => b.text)
    .join("\n");
  if (text.trim() !== "") return text;

  const tool = message.content.find(
    (b): b is Extract<Block, { type: "tool" }> => b.type === "tool",
  );
  if (tool) return toolText(tool);

  return "";
}

export function canQuote(message: ChatMessage): boolean {
  return quotableSource(message).trim() !== "";
}

/** chip 一行预览（按码点截断，不切坏代理对；ZWJ/组合字符不严格保证）。 */
export function quotePreview(message: ChatMessage): string {
  const src = quotableSource(message);
  if (src === "") return "";
  const lines = src.split("\n");
  const firstIdx = lines.findIndex((l) => l.trim() !== "");
  const firstLine = firstIdx >= 0 ? lines[firstIdx] : "";
  const cps = Array.from(firstLine);
  const truncated = cps.length > PREVIEW_MAX_CHARS;
  // 首行后还有非空内容 → chip 是片段，也要 … 提示（否则多行消息看着像完整）。
  const hasMore = lines.slice(firstIdx + 1).some((l) => l.trim() !== "");
  const base = truncated ? cps.slice(0, PREVIEW_MAX_CHARS).join("") : firstLine;
  return base + (truncated || hasMore ? "…" : "");
}

export function quoteBlock(message: ChatMessage): string {
  const src = quotableSource(message);
  if (src.trim() === "") return "";

  const allLines = src.split("\n");
  let truncated = allLines.length > BLOCK_MAX_LINES;
  let body = allLines.slice(0, BLOCK_MAX_LINES).join("\n");

  const cps = Array.from(body);
  if (cps.length > BLOCK_MAX_CHARS) {
    body = cps.slice(0, BLOCK_MAX_CHARS).join("");
    truncated = true;
  }
  if (truncated) body += "…";

  const prefixed = body
    .split("\n")
    .map((l) => `> ${l}`)
    .join("\n");
  return prefixed + "\n\n";
}

export function quoteLabel(message: ChatMessage, t: Translate): string {
  if (message.role === "user") return t("stream.role.user");
  return (
    nonEmpty(message.agent_name_snapshot) ??
    nonEmpty(message.engine) ??
    nonEmpty(message.agent_id) ??
    t("quote.role.assistant")
  );
}

/** hover tooltip：更全的引文（保留换行；截到 280 码点，避免巨型 tooltip）。 */
export function quoteTooltip(message: ChatMessage): string {
  const src = quotableSource(message);
  const cps = Array.from(src);
  return cps.length > TOOLTIP_MAX_CHARS
    ? cps.slice(0, TOOLTIP_MAX_CHARS).join("") + "…"
    : src;
}

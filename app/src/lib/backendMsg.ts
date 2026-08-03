import type { TranslationKey } from "../i18n";

export type BackendErrorEnvelope = {
  code: string;
  params: Record<string, string>;
};

type Translate = (
  key: TranslationKey,
  values?: Record<string, string | number>,
) => string;

const PREFIX = "AL_ERR:";
const LOCAL_SESSION_UNSUPPORTED_PREFIX = "LOCAL_SESSION_UNSUPPORTED";
const CODE_PATTERN = /^[a-zA-Z0-9.]+$/;
const TRANSIENT_LEAD_ERROR_CODES = new Set([
  "lead.spawnDriverFailed",
  "lead.spawnLeadFailed",
  "lead.noFinalText",
  "lead.noFinalTextStderr",
  "lead.parseSpawnFailed",
  "lead.parseNoOutput",
  "lead.draftNoFinalText",
  "lead.draftNoFinalTextStderr",
]);

export type LeadErrorClassification = "claudeOnly" | "transient" | "generic";

export function parseBackendError(raw: unknown): BackendErrorEnvelope | null {
  if (typeof raw !== "string" || !raw.startsWith(PREFIX)) return null;

  const payload = raw.slice(PREFIX.length);
  const separator = payload.indexOf(":");
  const code = separator === -1 ? payload : payload.slice(0, separator);
  if (!code || !CODE_PATTERN.test(code)) return null;
  if (separator === -1) return { code, params: {} };

  try {
    const parsed: unknown = JSON.parse(payload.slice(separator + 1));
    if (
      parsed === null ||
      Array.isArray(parsed) ||
      typeof parsed !== "object"
    ) {
      return null;
    }
    if (Object.values(parsed).some((value) => typeof value !== "string")) {
      return null;
    }
    return { code, params: parsed as Record<string, string> };
  } catch {
    return null;
  }
}

export function renderBackendError(raw: unknown, t: Translate): string {
  const message = String(raw);
  if (message.startsWith(LOCAL_SESSION_UNSUPPORTED_PREFIX)) {
    return t("backend.continuation.localSessionUnsupported");
  }

  const envelope = parseBackendError(raw);
  if (!envelope) return message;

  const key = `backend.${envelope.code}` as TranslationKey;
  const rendered = t(key, envelope.params);
  return rendered === key ? message : rendered;
}

export function classifyLeadError(msg: string): LeadErrorClassification {
  const envelope = parseBackendError(msg);
  const claudeOnly =
    envelope?.code.startsWith("lead.claudeOnly") ||
    /native claude|claude.only|仅.*claude|provider.*claude/i.test(msg);
  if (claudeOnly) return "claudeOnly";

  if (
    TRANSIENT_LEAD_ERROR_CODES.has(envelope?.code ?? "") ||
    /spawn 失败|无终态 final_text|无输出/.test(msg)
  ) {
    return "transient";
  }

  return "generic";
}

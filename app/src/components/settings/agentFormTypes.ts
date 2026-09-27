import type { TranslationKey } from "../../i18n";
import { useI18n } from "../../i18n";
import type { ReasoningTier } from "../../types/agent";
import type { AgentProfile } from "../../types/agent";
import {
  AUTO_REASONING_DEFAULT,
  asReasoningTier,
  defaultReasoningCapabilityForProvider,
} from "../../lib/agentReasoning";
import {
  AUTH_MODE,
  PROVIDER_PRESETS,
  engineView,
  inferProviderAccessPoint,
  type AccessPoint,
  type EngineId,
  type ProviderPreset,
  type ProviderId,
} from "./agentFormHelpers";

export type { ProviderId } from "./agentFormHelpers";

export type AuthMode = "bearer" | "x_api_key" | "";
export type ReasoningDefault = ReasoningTier;
export type TestState = "idle" | "testing" | "ok" | "err";
export type DetectResult = {
  available: boolean;
  creds_hint: boolean | null;
  overridden: boolean;
  path: string | null;
};
export type DetectState = { claude?: DetectResult; codex?: DetectResult };

export type FormValues = {
  preset: ProviderId;
  name: string;
  provider: string;
  primaryModel: string;
  apiKey: string;
  reasoningDefault: ReasoningDefault;
  endpoint: string;
  authMode: AuthMode;
  modelOpus: string;
  modelSonnet: string;
  modelHaiku: string;
  modelSubagent: string;
  maxOutputTokens: string;
  apiTimeoutMs: string;
  compatDisableBetas: boolean;
  compatDisableNonessential: boolean;
  compatDisableThinking: boolean;
  compatProxy: string;
};

export type ModelFieldKey =
  | "primaryModel"
  | "modelOpus"
  | "modelSonnet"
  | "modelHaiku"
  | "modelSubagent";

export type ModelFieldFlags = Record<ModelFieldKey, boolean>;

export type AgentFormProps = {
  agent?: AgentProfile | null;
  nextSortOrder?: number;
  onCancel: () => void;
  onSaved: () => void | Promise<void>;
};

export const CATEGORY_LABEL_KEYS: Record<string, TranslationKey> = {
  auth: "settings.agentForm.category.auth",
  rate_limit: "settings.agentForm.category.rateLimit",
  network: "settings.agentForm.category.network",
  not_found: "settings.agentForm.category.notFound",
  missing_key: "settings.agentForm.category.missingKey",
  endpoint_required: "settings.agentForm.category.endpointRequired",
  other: "settings.agentForm.category.other",
};

export function emptyModelFieldFlags(): ModelFieldFlags {
  return {
    primaryModel: false,
    modelOpus: false,
    modelSonnet: false,
    modelHaiku: false,
    modelSubagent: false,
  };
}

export function allModelFieldFlags(): ModelFieldFlags {
  return {
    primaryModel: true,
    modelOpus: true,
    modelSonnet: true,
    modelHaiku: true,
    modelSubagent: true,
  };
}

export function emptyValues(): FormValues {
  return {
    preset: "custom",
    name: "",
    provider: "",
    primaryModel: "",
    apiKey: "",
    reasoningDefault: "auto",
    endpoint: "",
    authMode: "",
    modelOpus: "",
    modelSonnet: "",
    modelHaiku: "",
    modelSubagent: "",
    maxOutputTokens: "",
    apiTimeoutMs: "",
    compatDisableBetas: false,
    compatDisableNonessential: false,
    compatDisableThinking: false,
    compatProxy: "",
  };
}

export function providerById(id: ProviderId) {
  return PROVIDER_PRESETS.find((provider) => provider.id === id)!;
}

export function engineOfPreset(presetId: ProviderId): EngineId {
  for (const entry of engineView()) {
    for (const group of entry.groups) {
      if (group.presets.some((preset) => preset.id === presetId)) {
        return entry.engine;
      }
    }
  }
  return "claude-code";
}

export function defaultPresetForEngine(
  entry: ReturnType<typeof engineView>[number],
): ProviderId {
  for (const group of entry.groups) {
    const preset = group.presets[0];
    if (preset) return preset.id;
  }
  return "custom";
}

export function engineDetectKey(engine: EngineId): "claude" | "codex" | null {
  if (engine === "claude-code") return "claude";
  if (engine === "codex") return "codex";
  return null;
}

export function nativeDetectKey(preset: ProviderId): "claude" | "codex" {
  return preset === "codex" ? "codex" : "claude";
}

export function nativeAccountName(preset: ProviderId) {
  return preset === "codex" ? "OpenAI" : "Anthropic";
}

export type Translator = ReturnType<typeof useI18n>["t"];

export function groupKindLabel(kind: "account" | "api_key", t: Translator) {
  return kind === "account" ? t("settings.agentForm.group.account") : "API Key";
}

export function accessPointLabel(id: string, t: Translator) {
  if (id === "cn") return t("settings.agentForm.accessPoint.cn");
  if (id === "intl") return t("settings.agentForm.accessPoint.intl");
  if (id === "cn-coding") return t("settings.agentForm.accessPoint.cn-coding");
  if (id === "intl-coding")
    return t("settings.agentForm.accessPoint.intl-coding");
  return t("settings.agentForm.accessPoint.default");
}

export function engineDescKey(engine: EngineId): TranslationKey {
  if (engine === "claude-code") {
    return "settings.agentForm.engineDesc.claudeCode";
  }
  if (engine === "codex") return "settings.agentForm.engineDesc.codex";
  return "settings.agentForm.engineDesc.myagent";
}

export function inferFromAgent(agent: AgentProfile) {
  return inferProviderAccessPoint({
    endpoint: agent.endpoint,
    provider: agent.provider,
    access: agent.access,
  });
}

export function apLevelValues(
  ap: AccessPoint,
): Pick<
  FormValues,
  | "endpoint"
  | "primaryModel"
  | "modelOpus"
  | "modelSonnet"
  | "modelHaiku"
  | "modelSubagent"
  | "apiTimeoutMs"
> {
  return {
    endpoint: ap.endpoint,
    primaryModel: ap.primaryModel,
    modelOpus: ap.mapping.opus,
    modelSonnet: ap.mapping.sonnet,
    modelHaiku: ap.mapping.haiku,
    modelSubagent: ap.mapping.subagent,
    apiTimeoutMs: ap.apiTimeoutMs ? String(ap.apiTimeoutMs) : "",
  };
}

export function nativeLevelValues(
  provider: ProviderPreset,
): Pick<
  FormValues,
  | "endpoint"
  | "primaryModel"
  | "modelOpus"
  | "modelSonnet"
  | "modelHaiku"
  | "modelSubagent"
  | "apiTimeoutMs"
> {
  const mapping = provider.nativeMapping ?? {
    opus: provider.nativePrimaryModel ?? "",
    sonnet: provider.nativePrimaryModel ?? "",
    haiku: provider.nativePrimaryModel ?? "",
    subagent: provider.nativePrimaryModel ?? "",
  };
  return {
    endpoint: "",
    primaryModel: provider.nativePrimaryModel ?? "",
    modelOpus: mapping.opus,
    modelSonnet: mapping.sonnet,
    modelHaiku: mapping.haiku,
    modelSubagent: mapping.subagent,
    apiTimeoutMs: "",
  };
}

export function valueFromAgent(
  agent: AgentProfile,
  inferred: ReturnType<typeof inferProviderAccessPoint>,
): FormValues {
  return {
    ...emptyValues(),
    preset: inferred.providerId,
    name: agent.name,
    provider: agent.provider,
    primaryModel: agent.primary_model ?? "",
    reasoningDefault: asReasoningDefault(agent.reasoning_default),
    endpoint: agent.endpoint ?? "",
    authMode: asAuthMode(agent.auth_mode),
    modelOpus: agent.model_opus ?? "",
    modelSonnet: agent.model_sonnet ?? "",
    modelHaiku: agent.model_haiku ?? "",
    modelSubagent: agent.model_subagent ?? "",
    maxOutputTokens: numberToInput(agent.max_output_tokens),
    apiTimeoutMs: numberToInput(agent.api_timeout_ms),
    compatDisableBetas: agent.compat_disable_betas,
    compatDisableNonessential: agent.compat_disable_nonessential,
    compatDisableThinking: agent.compat_disable_thinking,
    compatProxy: agent.compat_proxy ?? "",
  };
}

export function usesCustomModelInput(
  agent: AgentProfile | null | undefined,
  inferred: ReturnType<typeof inferProviderAccessPoint> | null,
  extraKnownModels: string[] = [],
): boolean {
  if (!agent?.primary_model) return false;
  if (!inferred || inferred.providerId === "custom") return true;
  const provider = providerById(inferred.providerId);
  const accessPoint = provider.accessPoints.find(
    (ap) => ap.id === inferred.accessPointId,
  );
  const knownModels = [
    ...(accessPoint?.knownModels ?? provider.nativeModels ?? []),
    ...extraKnownModels,
  ];
  return !knownModels.includes(agent.primary_model);
}

export function asReasoningDefault(value: string): ReasoningDefault {
  return asReasoningTier(value) ?? "auto";
}

export function asAuthMode(value: string | null): AuthMode {
  return value === AUTH_MODE.bearer || value === AUTH_MODE.xApiKey ? value : "";
}

export function numberToInput(value: number | null): string {
  return value == null ? "" : String(value);
}

export function nullableText(value: string): string | null {
  const trimmed = value.trim();
  return trimmed ? trimmed : null;
}

export function nullableNumber(value: string): number | null {
  const trimmed = value.trim();
  if (!trimmed) return null;
  const parsed = Number(trimmed);
  if (!Number.isFinite(parsed)) return null;
  return Math.trunc(parsed);
}

export function reasoningCapability(
  values: FormValues,
  current: string | null | undefined,
): string | null {
  if (values.compatDisableThinking) return null;
  const existing = current?.trim();
  return existing || defaultReasoningCapabilityForProvider(values.provider);
}

export function reasoningDefaultForOptions(
  value: ReasoningDefault,
  options: ReasoningTier[],
): ReasoningDefault {
  const requested = value === "auto" ? AUTO_REASONING_DEFAULT : value;
  if (options.includes(requested)) return requested;
  if (options.includes(AUTO_REASONING_DEFAULT)) return AUTO_REASONING_DEFAULT;
  return options[0] ?? "auto";
}

export function slugAgentId(name: string, fallbackTime: number): string {
  const slug = name
    .trim()
    .toLowerCase()
    .replace(/[^a-z0-9_-]+/g, "-")
    .replace(/^-+|-+$/g, "")
    .slice(0, 48);
  return slug || `agent-${fallbackTime}`;
}

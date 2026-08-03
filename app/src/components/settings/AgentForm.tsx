import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import type { CSSProperties, FormEvent, ReactNode } from "react";
import { useI18n, type TranslationKey } from "../../i18n";
import type { AgentProfile, ConnectionTestResult } from "../../types/agent";
import type { ReasoningTier } from "../../types/agent";
import {
  AUTO_REASONING_DEFAULT,
  asReasoningTier,
  defaultReasoningCapabilityForProvider,
  reasoningOptionsForCapability,
} from "../../lib/agentReasoning";
import { openInstallGuide } from "../../lib/installGuide";
import {
  AUTH_MODE,
  PROVIDER_PRESETS,
  autoAgentName,
  classifyModelsFetchError,
  deriveModelMapping,
  deriveAccess,
  engineView,
  inferProviderAccessPoint,
  mergeModelOptions,
  normalizeEndpoint,
  readModelCache,
  resolveModelsEndpoint,
  writeModelCache,
  type AccessPoint,
  type EngineId,
  type ProviderPreset,
  type ProviderId,
} from "./agentFormHelpers";
import { ModelDropdown } from "./ModelDropdown";

type AuthMode = "bearer" | "x_api_key" | "";
type ReasoningDefault = ReasoningTier;
type TestState = "idle" | "testing" | "ok" | "err";
type DetectResult = { available: boolean; creds_hint: boolean | null };
type DetectState = { claude?: DetectResult; codex?: DetectResult };

type FormValues = {
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

type ModelFieldKey =
  | "primaryModel"
  | "modelOpus"
  | "modelSonnet"
  | "modelHaiku"
  | "modelSubagent";

type ModelFieldFlags = Record<ModelFieldKey, boolean>;

type AgentFormProps = {
  agent?: AgentProfile | null;
  nextSortOrder?: number;
  onCancel: () => void;
  onSaved: () => void | Promise<void>;
};

const CATEGORY_LABEL_KEYS: Record<string, TranslationKey> = {
  auth: "settings.agentForm.category.auth",
  rate_limit: "settings.agentForm.category.rateLimit",
  network: "settings.agentForm.category.network",
  not_found: "settings.agentForm.category.notFound",
  missing_key: "settings.agentForm.category.missingKey",
  endpoint_required: "settings.agentForm.category.endpointRequired",
  other: "settings.agentForm.category.other",
};

function emptyModelFieldFlags(): ModelFieldFlags {
  return {
    primaryModel: false,
    modelOpus: false,
    modelSonnet: false,
    modelHaiku: false,
    modelSubagent: false,
  };
}

function allModelFieldFlags(): ModelFieldFlags {
  return {
    primaryModel: true,
    modelOpus: true,
    modelSonnet: true,
    modelHaiku: true,
    modelSubagent: true,
  };
}

const styles = {
  title: {
    color: "var(--ink)",
    fontSize: 12.5,
    fontWeight: 600,
    marginBottom: 10,
  },
  sectionTitle: {
    color: "var(--ink-2)",
    fontSize: 10.5,
    fontWeight: 700,
    margin: "14px 0 8px",
  },
  field: {
    display: "flex",
    flexDirection: "column",
    gap: 3,
    marginBottom: 10,
  },
  grid: {
    display: "grid",
    gap: "8px 10px",
    gridTemplateColumns: "repeat(2, minmax(0, 1fr))",
  },
  label: {
    color: "var(--ink-3)",
    fontSize: 10.5,
    fontWeight: 600,
  },
  input: {
    background: "var(--panel)",
    border: "1px solid var(--line)",
    borderRadius: 6,
    color: "var(--ink)",
    font: "inherit",
    fontSize: 12,
    padding: "6px 10px",
    width: "100%",
  },
  monoInput: {
    fontFamily: '"SF Mono", monospace',
    fontSize: 11,
  },
  hint: {
    color: "var(--ink-3)",
    fontSize: 10.5,
    marginTop: 1,
  },
  segment: {
    display: "flex",
    flexWrap: "wrap",
    gap: 5,
  },
  segmentButton: {
    background: "var(--panel)",
    border: "1px solid var(--line)",
    borderRadius: 6,
    color: "var(--ink-2)",
    cursor: "pointer",
    fontFamily: "inherit",
    fontSize: 11,
    fontWeight: 400,
    padding: "5px 10px",
  },
  segmentButtonActive: {
    background: "var(--accent-soft)",
    border: "1px solid var(--accent)",
    color: "var(--accent)",
    fontWeight: 600,
  },
  toggle: {
    alignItems: "center",
    background: "transparent",
    border: 0,
    color: "var(--ink-2)",
    cursor: "pointer",
    display: "flex",
    fontFamily: "inherit",
    fontSize: 10.5,
    fontWeight: 700,
    margin: "14px 0 8px",
    padding: 0,
    textAlign: "left",
  },
  checks: {
    display: "flex",
    flexWrap: "wrap",
    gap: 7,
    margin: "2px 0 10px",
  },
  checkLabel: {
    alignItems: "center",
    background: "var(--panel)",
    border: "1px solid var(--line)",
    borderRadius: 6,
    color: "var(--ink-2)",
    display: "inline-flex",
    fontSize: 11,
    gap: 6,
    padding: "5px 9px",
  },
  actions: {
    display: "flex",
    alignItems: "center",
    gap: 8,
    justifyContent: "flex-end",
    marginTop: 4,
  },
  engineRow: {
    display: "grid",
    gap: 8,
    gridTemplateColumns: "repeat(3, minmax(0, 1fr))",
  },
  engineButton: {
    background: "var(--panel)",
    border: "1px solid var(--line)",
    borderRadius: 8,
    color: "var(--ink-2)",
    cursor: "pointer",
    display: "flex",
    flexDirection: "column",
    fontFamily: "inherit",
    gap: 4,
    minHeight: 82,
    padding: "9px 10px",
    textAlign: "left",
  },
  engineButtonActive: {
    background: "var(--accent-soft)",
    border: "1px solid var(--accent)",
    boxShadow: "inset 0 0 0 1px var(--accent)",
    color: "var(--ink)",
  },
  engineSelectButton: {
    background: "transparent",
    border: 0,
    color: "inherit",
    cursor: "pointer",
    display: "flex",
    flex: 1,
    flexDirection: "column",
    font: "inherit",
    gap: 4,
    padding: 0,
    textAlign: "left",
    width: "100%",
  },
  engineName: {
    color: "var(--ink)",
    fontSize: 12,
    fontWeight: 700,
  },
  engineDesc: {
    color: "var(--ink-3)",
    fontSize: 10.5,
    lineHeight: 1.45,
  },
  engineStatus: {
    fontSize: 10,
    fontWeight: 700,
    marginTop: "auto",
  },
  providerGroup: {
    display: "flex",
    flexDirection: "column",
    gap: 5,
  },
  providerGroupLabel: {
    color: "var(--ink-3)",
    fontSize: 10,
    fontWeight: 700,
  },
  providerPlaceholder: {
    background: "var(--bg)",
    border: "1px solid var(--line)",
    borderRadius: 6,
    color: "var(--ink-3)",
    display: "inline-flex",
    fontSize: 11,
    padding: "5px 10px",
  },
  chipBadge: {
    color: "var(--green)",
    fontSize: 9.5,
    fontWeight: 700,
    marginLeft: 4,
  },
  nativeStatus: {
    alignItems: "center",
    display: "flex",
    flexWrap: "wrap",
    fontSize: 11,
    fontWeight: 600,
    gap: 6,
    margin: "2px 0 10px",
  },
  statusLink: {
    background: "transparent",
    border: 0,
    color: "var(--accent)",
    cursor: "pointer",
    font: "inherit",
    fontWeight: 700,
    padding: 0,
    textDecoration: "underline",
  },
  saveWhy: {
    color: "var(--ink-3)",
    fontSize: 10.5,
    marginRight: "auto",
  },
  error: {
    color: "var(--red)",
    fontSize: 11,
    marginTop: 8,
    textAlign: "right",
  },
} satisfies Record<string, CSSProperties>;

function emptyValues(): FormValues {
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

function providerById(id: ProviderId) {
  return PROVIDER_PRESETS.find((provider) => provider.id === id)!;
}

function engineOfPreset(presetId: ProviderId): EngineId {
  for (const entry of engineView()) {
    for (const group of entry.groups) {
      if (group.presets.some((preset) => preset.id === presetId)) {
        return entry.engine;
      }
    }
  }
  return "claude-code";
}

function defaultPresetForEngine(
  entry: ReturnType<typeof engineView>[number],
): ProviderId {
  for (const group of entry.groups) {
    const preset = group.presets[0];
    if (preset) return preset.id;
  }
  return "custom";
}

function engineDetectKey(engine: EngineId): "claude" | "codex" | null {
  if (engine === "claude-code") return "claude";
  if (engine === "codex") return "codex";
  return null;
}

function nativeDetectKey(preset: ProviderId): "claude" | "codex" {
  return preset === "codex" ? "codex" : "claude";
}

function nativeAccountName(preset: ProviderId) {
  return preset === "codex" ? "OpenAI" : "Anthropic";
}

type Translator = ReturnType<typeof useI18n>["t"];

function groupKindLabel(kind: "account" | "api_key", t: Translator) {
  return kind === "account" ? t("settings.agentForm.group.account") : "API Key";
}

function accessPointLabel(id: string, t: Translator) {
  if (id === "cn") return t("settings.agentForm.accessPoint.cn");
  if (id === "intl") return t("settings.agentForm.accessPoint.intl");
  if (id === "cn-coding") return t("settings.agentForm.accessPoint.cn-coding");
  if (id === "intl-coding")
    return t("settings.agentForm.accessPoint.intl-coding");
  return t("settings.agentForm.accessPoint.default");
}

function engineDescKey(engine: EngineId): TranslationKey {
  if (engine === "claude-code") {
    return "settings.agentForm.engineDesc.claudeCode";
  }
  if (engine === "codex") return "settings.agentForm.engineDesc.codex";
  return "settings.agentForm.engineDesc.myagent";
}

function inferFromAgent(agent: AgentProfile) {
  return inferProviderAccessPoint({
    endpoint: agent.endpoint,
    provider: agent.provider,
    access: agent.access,
  });
}

function apLevelValues(
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

function nativeLevelValues(
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

function valueFromAgent(
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

function usesCustomModelInput(
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

function asReasoningDefault(value: string): ReasoningDefault {
  return asReasoningTier(value) ?? "auto";
}

function asAuthMode(value: string | null): AuthMode {
  return value === AUTH_MODE.bearer || value === AUTH_MODE.xApiKey ? value : "";
}

function numberToInput(value: number | null): string {
  return value == null ? "" : String(value);
}

function nullableText(value: string): string | null {
  const trimmed = value.trim();
  return trimmed ? trimmed : null;
}

function nullableNumber(value: string): number | null {
  const trimmed = value.trim();
  if (!trimmed) return null;
  const parsed = Number(trimmed);
  if (!Number.isFinite(parsed)) return null;
  return Math.trunc(parsed);
}

function reasoningCapability(
  values: FormValues,
  current: string | null | undefined,
): string | null {
  if (values.compatDisableThinking) return null;
  const existing = current?.trim();
  return existing || defaultReasoningCapabilityForProvider(values.provider);
}

function reasoningDefaultForOptions(
  value: ReasoningDefault,
  options: ReasoningTier[],
): ReasoningDefault {
  const requested = value === "auto" ? AUTO_REASONING_DEFAULT : value;
  if (options.includes(requested)) return requested;
  if (options.includes(AUTO_REASONING_DEFAULT)) return AUTO_REASONING_DEFAULT;
  return options[0] ?? "auto";
}

function slugAgentId(name: string, fallbackTime: number): string {
  const slug = name
    .trim()
    .toLowerCase()
    .replace(/[^a-z0-9_-]+/g, "-")
    .replace(/^-+|-+$/g, "")
    .slice(0, 48);
  return slug || `agent-${fallbackTime}`;
}

function SegmentButton({
  active,
  ariaLabel,
  children,
  onClick,
}: {
  active: boolean;
  ariaLabel?: string;
  children: ReactNode;
  onClick: () => void;
}) {
  return (
    <button
      type="button"
      aria-label={ariaLabel}
      aria-pressed={active}
      onClick={onClick}
      style={{
        ...styles.segmentButton,
        ...(active ? styles.segmentButtonActive : {}),
      }}
    >
      {children}
    </button>
  );
}

function Field({
  children,
  hint,
  label,
}: {
  children: ReactNode;
  hint?: string;
  label: string;
}) {
  return (
    <div style={styles.field}>
      <span style={styles.label}>{label}</span>
      {children}
      {hint ? <span style={styles.hint}>{hint}</span> : null}
    </div>
  );
}

export function AgentForm({
  agent,
  nextSortOrder = 0,
  onCancel,
  onSaved,
}: AgentFormProps) {
  const { t } = useI18n();
  const initialInference = agent ? inferFromAgent(agent) : null;
  const [values, setValues] = useState<FormValues>(() =>
    agent && initialInference
      ? valueFromAgent(agent, initialInference)
      : emptyValues(),
  );
  const [accessPointId, setAccessPointId] = useState<string | null>(
    () => initialInference?.accessPointId ?? null,
  );
  const [liveModels, setLiveModels] = useState<string[]>([]);
  const [testState, setTestState] = useState<TestState>("idle");
  const [testResult, setTestResult] = useState<ConnectionTestResult | null>(
    null,
  );
  const [connDirty, setConnDirty] = useState<boolean>(() => !agent);
  const [rawOpen, setRawOpen] = useState(false);
  const [customModel, setCustomModel] = useState(() =>
    usesCustomModelInput(
      agent,
      initialInference,
      // 缓存列表只参与 harness 的输入形态判定；borrow 编辑态保持旧行为（opus 审 Minor）
      agent?.access === "harness"
        ? (readModelCache(values.preset, values.endpoint) ?? [])
        : [],
    ),
  );
  const [showKey, setShowKey] = useState(false);
  const [advancedOpen, setAdvancedOpen] = useState(
    () => agent?.access === "native",
  );
  const [detect, setDetect] = useState<DetectState>({});
  const [nameDirty, setNameDirty] = useState(false);
  const [modelFieldDirty, setModelFieldDirty] = useState<ModelFieldFlags>(() =>
    agent ? allModelFieldFlags() : emptyModelFieldFlags(),
  );
  const [modelFieldAutoFilled, setModelFieldAutoFilled] =
    useState<ModelFieldFlags>(() => emptyModelFieldFlags());
  const [submitting, setSubmitting] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const testGen = useRef(0);
  const modelFieldDirtyRef = useRef<ModelFieldFlags>(
    agent ? allModelFieldFlags() : emptyModelFieldFlags(),
  );
  const currentProvider = () => providerById(values.preset);
  const currentAccessPoint = () =>
    currentProvider().accessPoints.find((ap) => ap.id === accessPointId);
  const staticModels =
    currentAccessPoint()?.knownModels ?? currentProvider().nativeModels ?? [];
  const modelOptions = mergeModelOptions(
    staticModels,
    liveModels,
    values.primaryModel,
  );
  const provider = currentProvider();
  const multiAP = provider.accessPoints.length >= 2;
  const hasStoredKey = agent?.has_key ?? false;
  const keyInput = values.apiKey.trim();
  const hasKey = (agent?.has_key ?? false) || values.apiKey.trim() !== "";
  const effectiveAccess = agent
    ? deriveAccess(values.preset, agent.access)
    : deriveAccess(values.preset);
  const isNative = effectiveAccess === "native";
  const isEditingNative = Boolean(agent && isNative);
  const isBorrow = effectiveAccess === "borrow";
  const isHarness = effectiveAccess === "harness";
  const harnessHasModelChoices = isHarness && liveModels.length > 0;
  const showCustomModelInput = isHarness
    ? !harnessHasModelChoices || customModel
    : customModel;
  const isUnknownModel =
    values.primaryModel.trim() !== "" &&
    !staticModels.includes(values.primaryModel) &&
    !liveModels.includes(values.primaryModel);
  // 编辑态锁 access 家族：只显示与该 agent 同族的预设组（codex 审出的 Medium——
  // 跨族点击会存出「access 与 preset 脱钩」的坏配置，如 harness agent 配上借壳
  // /anthropic 端点）。要换族请新建 agent；新增态三组全显。
  const groupVisible = (access: string) => !agent || agent.access === access;
  const showBorrowKeyWarning = isBorrow && !hasStoredKey && !keyInput;
  const presetReasoningCapability =
    provider.access === "native" ? provider.nativeCapReasoning : null;
  const effectiveReasoningCapability = reasoningCapability(
    values,
    agent?.cap_reasoning ?? presetReasoningCapability,
  );
  const reasoningLevels = reasoningOptionsForCapability(
    effectiveReasoningCapability,
    values.provider,
  );
  const selectedReasoningDefault = reasoningDefaultForOptions(
    values.reasoningDefault,
    reasoningLevels,
  );
  const engines = engineView();
  const currentEngineId = engineOfPreset(values.preset);
  const currentEngine =
    engines.find((entry) => entry.engine === currentEngineId) ?? engines[0];
  const visibleEngines = agent && !isEditingNative ? [currentEngine] : engines;
  const providerGroups = currentEngine.groups.filter((group) => {
    if (!agent) return true;
    const access = group.presets[0]?.access;
    return access ? groupVisible(access) : true;
  });
  const showProviderGroupLabels = providerGroups.length > 1;
  const nativeRuntimeKey = nativeDetectKey(values.preset);
  const nativeRuntime = isNative ? detect[nativeRuntimeKey] : undefined;
  const nativeRuntimeBlocked = isNative && nativeRuntime?.available === false;
  const requiresFreshTest = (isBorrow || isHarness) && connDirty;
  const connectionTestBlocked = requiresFreshTest && testState !== "ok";
  const saveBlocked = nativeRuntimeBlocked || connectionTestBlocked;
  const saveBlockedReason = nativeRuntimeBlocked
    ? t("settings.agentForm.saveBlocked.nativeMissing", {
        cli: nativeRuntimeKey,
      })
    : connectionTestBlocked
      ? t("settings.agentForm.saveBlocked.testFailed")
      : null;

  function setValue<K extends keyof FormValues>(key: K, value: FormValues[K]) {
    setValues((current) => ({ ...current, [key]: value }));
  }

  function resetModelFieldTracking() {
    const empty = emptyModelFieldFlags();
    modelFieldDirtyRef.current = empty;
    setModelFieldDirty(empty);
    setModelFieldAutoFilled(emptyModelFieldFlags());
  }

  function markModelFieldDirty(field: ModelFieldKey) {
    modelFieldDirtyRef.current = {
      ...modelFieldDirtyRef.current,
      [field]: true,
    };
    setModelFieldDirty((current) => ({ ...current, [field]: true }));
    setModelFieldAutoFilled((current) => ({ ...current, [field]: false }));
  }

  function applyDerivedModels(modelIds: string[]) {
    const derived = deriveModelMapping(modelIds);
    if (!derived) return;

    const dirty = modelFieldDirtyRef.current;
    setValues((current) => ({
      ...current,
      ...(!dirty.primaryModel && derived.primary
        ? { primaryModel: derived.primary }
        : {}),
      ...(!dirty.modelOpus && derived.opus ? { modelOpus: derived.opus } : {}),
      ...(!dirty.modelSonnet && derived.sonnet
        ? { modelSonnet: derived.sonnet }
        : {}),
      ...(!dirty.modelHaiku && derived.haiku
        ? { modelHaiku: derived.haiku }
        : {}),
      ...(!dirty.modelSubagent && derived.subagent
        ? { modelSubagent: derived.subagent }
        : {}),
    }));
    setModelFieldAutoFilled((current) => ({
      ...current,
      ...(!dirty.primaryModel && derived.primary ? { primaryModel: true } : {}),
      ...(!dirty.modelOpus && derived.opus ? { modelOpus: true } : {}),
      ...(!dirty.modelSonnet && derived.sonnet ? { modelSonnet: true } : {}),
      ...(!dirty.modelHaiku && derived.haiku ? { modelHaiku: true } : {}),
      ...(!dirty.modelSubagent && derived.subagent
        ? { modelSubagent: true }
        : {}),
    }));
  }

  async function refreshDetect() {
    try {
      const result = await invoke<{
        claude?: DetectResult;
        codex?: DetectResult;
      }>("detect_runtime");
      if (!result?.claude && !result?.codex) return;
      setDetect({
        claude: result?.claude,
        codex: result?.codex,
      });
    } catch {
      setDetect({});
    }
  }

  function resetTest(options: { keepLiveModels?: boolean } = {}) {
    setConnDirty(true);
    testGen.current++;
    setTestState("idle");
    setTestResult(null);
    if (!options.keepLiveModels) setLiveModels([]);
    setRawOpen(false);
  }

  useEffect(() => {
    const norm = normalizeEndpoint(values.endpoint);
    const ap = currentAccessPoint();
    if (norm && norm === normalizeEndpoint(ap?.endpoint ?? "")) {
      setLiveModels(readModelCache(values.preset, values.endpoint) ?? []);
    } else {
      setLiveModels([]);
    }
  }, [values.preset, values.endpoint, accessPointId]);

  useEffect(() => {
    void refreshDetect();
  }, []);

  function applyProvider(id: ProviderId) {
    const provider = providerById(id);
    const defaultAccessPoint = provider.accessPoints[0];
    resetTest();
    setLiveModels(
      defaultAccessPoint
        ? (readModelCache(id, defaultAccessPoint.endpoint) ?? [])
        : [],
    );
    setNameDirty(false);
    resetModelFieldTracking();
    setCustomModel(id === "custom");
    setAccessPointId(defaultAccessPoint?.id ?? null);
    setValues((current) => ({
      ...emptyValues(),
      preset: id,
      name:
        id === "custom"
          ? current.name
          : autoAgentName(id, defaultAccessPoint?.id),
      provider: id === "custom" ? current.provider : provider.providerValue,
      apiKey: current.apiKey,
      authMode: provider.authMode,
      compatDisableBetas: provider.compatDisableBetas,
      compatDisableNonessential: provider.compatDisableNonessential,
      compatDisableThinking: provider.compatDisableThinking,
      compatProxy: provider.compatProxy ?? "",
      reasoningDefault: "auto",
      ...(defaultAccessPoint
        ? apLevelValues(defaultAccessPoint)
        : nativeLevelValues(provider)),
    }));
  }

  function applyAccessPoint(apId: string) {
    const accessPoint = currentProvider().accessPoints.find(
      (ap) => ap.id === apId,
    )!;
    resetTest();
    setLiveModels(readModelCache(values.preset, accessPoint.endpoint) ?? []);
    resetModelFieldTracking();
    setCustomModel(false);
    setAccessPointId(apId);
    setValues((current) => ({
      ...current,
      ...apLevelValues(accessPoint),
      ...(!nameDirty ? { name: autoAgentName(current.preset, apId) } : {}),
    }));
  }

  async function runTest() {
    if (
      (effectiveAccess !== "borrow" && effectiveAccess !== "harness") ||
      testState === "testing"
    ) {
      return;
    }
    const gen = ++testGen.current;
    setTestState("testing");
    setTestResult(null);
    setRawOpen(false);
    try {
      if (effectiveAccess === "harness") {
        const ep = values.endpoint.trim();
        if (!ep) {
          setTestState("err");
          setTestResult({
            ok: false,
            category: "endpoint_required",
            raw_error: null,
          });
          return;
        }
        const modelsEndpoint = resolveModelsEndpoint(ep, currentAccessPoint());
        const models = await invoke<string[]>("fetch_agent_models", {
          agentId: agent?.id ?? null,
          modelsEndpoint,
          authMode: values.authMode || null,
          apiKey: values.apiKey.trim() || null,
        });
        if (gen !== testGen.current) return;
        setTestResult({ ok: true, category: null, raw_error: null });
        setTestState("ok");
        setLiveModels(models);
        writeModelCache(values.preset, values.endpoint, models);
        // 测试成功这一刻：若模型字段从未被用户动过且当前为空，自动选列表末位
        // （z.ai 等按发布时间升序排列·末位最新），防止落到引擎写死的老默认
        // 模型（如 glm-4-plus 不在 Coding 套餐模型列表里·真跑 429/1113）。
        // 用户已经交互过（哪怕是显式选回「myagent 默认」）就不再覆盖。
        if (
          !modelFieldDirtyRef.current.primaryModel &&
          !values.primaryModel.trim() &&
          models.length > 0
        ) {
          setValue("primaryModel", models[models.length - 1]);
        }
        return;
      }

      const r = await invoke<ConnectionTestResult>("test_agent_connection", {
        agentId: agent?.id ?? null,
        endpoint: values.endpoint,
        protocol: undefined,
        authMode: values.authMode || null,
        model: values.primaryModel,
        apiKey: values.apiKey.trim() || null,
      });
      if (gen !== testGen.current) return;
      setTestResult(r);
      if (r.ok) {
        setTestState("ok");
        const ap = currentAccessPoint();
        const norm = normalizeEndpoint(values.endpoint);
        const sameEp =
          norm !== null && norm === normalizeEndpoint(ap?.endpoint ?? "");
        const modelsEndpoint = sameEp ? ap?.modelsEndpoint : undefined;
        if (modelsEndpoint) {
          try {
            const models = await invoke<string[]>("fetch_agent_models", {
              agentId: agent?.id ?? null,
              modelsEndpoint,
              authMode: values.authMode || null,
              apiKey: values.apiKey.trim() || null,
            });
            if (gen !== testGen.current) return;
            setLiveModels(models);
            if (effectiveAccess === "borrow") {
              applyDerivedModels(models);
            }
            writeModelCache(values.preset, values.endpoint, models);
          } catch {
            /* /models 拉取失败静默降级静态模型 */
          }
        }
      } else {
        setTestState("err");
      }
    } catch (e) {
      if (gen !== testGen.current) return;
      const raw = String(e);
      setTestState("err");
      setTestResult({
        ok: false,
        category:
          effectiveAccess === "harness"
            ? classifyModelsFetchError(raw)
            : "other",
        raw_error: raw,
      });
    }
  }

  async function submit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    if (submitting) return;
    // 门禁兜底：按钮 disabled 挡 GUI 点击，这里挡 form submit 事件层（回车等路径）
    if (saveBlocked) {
      if (saveBlockedReason) setError(saveBlockedReason);
      return;
    }

    const formName = values.name.trim();
    setError(null);
    if (!formName) {
      setError(t("settings.agentForm.error.nameRequired"));
      return;
    }
    if (isBorrow && !values.primaryModel.trim()) {
      setError(t("settings.agentForm.error.primaryModelRequired"));
      return;
    }
    if (isBorrow && !values.endpoint.trim()) {
      setError(t("settings.agentForm.error.endpointRequired"));
      return;
    }

    const now = Date.now();
    const id =
      agent?.id ?? slugAgentId(formName || values.provider || "agent", now);
    const inferred = inferProviderAccessPoint({
      endpoint: values.endpoint,
      provider: values.provider,
    });
    const canonicalProvider = isHarness
      ? values.provider.trim()
      : inferred.providerId !== "custom"
        ? inferred.providerId
        : values.provider.trim();
    const profile: AgentProfile = {
      id,
      name: formName,
      access: effectiveAccess,
      provider: canonicalProvider,
      primary_model: nullableText(values.primaryModel),
      endpoint: nullableText(values.endpoint),
      auth_mode: nullableText(values.authMode),
      model_opus: nullableText(values.modelOpus),
      model_sonnet: nullableText(values.modelSonnet),
      model_haiku: nullableText(values.modelHaiku),
      model_subagent: nullableText(values.modelSubagent),
      reasoning_default: selectedReasoningDefault,
      max_output_tokens: nullableNumber(values.maxOutputTokens),
      api_timeout_ms: nullableNumber(values.apiTimeoutMs),
      compat_disable_betas: values.compatDisableBetas,
      compat_disable_nonessential: values.compatDisableNonessential,
      compat_disable_thinking: values.compatDisableThinking,
      compat_proxy: nullableText(values.compatProxy),
      custom_headers: agent?.custom_headers ?? null,
      extra_body: agent?.extra_body ?? null,
      cap_reasoning: effectiveReasoningCapability,
      cap_computer_use: agent?.cap_computer_use ?? null,
      cap_lead: agent?.cap_lead ?? provider.nativeCapLead ?? null,
      has_key: agent?.has_key ?? false,
      is_builtin: agent?.is_builtin ?? false,
      enabled: agent?.enabled ?? true,
      sort_order: agent?.sort_order ?? nextSortOrder,
      created_at: agent?.created_at ?? now,
      updated_at: now,
    };

    setSubmitting(true);
    try {
      await invoke("upsert_agent", { profile });
      if (keyInput) {
        await invoke("set_agent_key", { id: profile.id, key: keyInput });
      }
      await onSaved();
    } catch {
      setError(t("settings.agentForm.error.saveFailed"));
    } finally {
      setSubmitting(false);
    }
  }

  function renderEngineStatus(entry: (typeof engines)[number]) {
    if (entry.engine === "myagent") {
      return (
        <span style={{ ...styles.engineStatus, color: "var(--green)" }}>
          {t("settings.agentForm.engineStatus.builtIn")}
        </span>
      );
    }

    const key = engineDetectKey(entry.engine);
    if (!key) return null;
    const result = detect[key];
    if (result === undefined) return null;
    if (result.available) {
      return (
        <span style={{ ...styles.engineStatus, color: "var(--green)" }}>
          {result.creds_hint === true
            ? t("settings.agentForm.engineStatus.installedLoggedIn")
            : t("settings.agentForm.engineStatus.installed")}
        </span>
      );
    }

    return (
      <span style={{ ...styles.engineStatus, color: "var(--amber)" }}>
        {t("settings.agentForm.engineStatus.notDetected")}{" "}
        <button
          type="button"
          aria-label={t("settings.agentForm.engineStatus.installGuideAria", {
            engine: entry.label,
          })}
          onClick={(event) => {
            event.stopPropagation();
            void openInstallGuide(key);
          }}
          style={styles.statusLink}
        >
          {t("settings.agentForm.engineStatus.installGuide")}
        </button>
      </span>
    );
  }

  function renderNativeRuntimeStatus() {
    if (!isNative || nativeRuntime === undefined) return null;

    const cli = nativeRuntimeKey;
    const account = nativeAccountName(values.preset);
    if (nativeRuntime.available && nativeRuntime.creds_hint === true) {
      return (
        <div style={{ ...styles.nativeStatus, color: "var(--green)" }}>
          {t("settings.agentForm.nativeStatus.loggedIn", { cli, account })}
        </div>
      );
    }

    if (nativeRuntime.available) {
      return (
        <div style={{ ...styles.nativeStatus, color: "var(--amber)" }}>
          {t("settings.agentForm.nativeStatus.installedNoCredsPrefix", {
            cli,
          })}{" "}
          <code>{cli} login</code>{" "}
          {t("settings.agentForm.nativeStatus.installedNoCredsSuffix")}{" "}
          <button
            type="button"
            onClick={() => void refreshDetect()}
            style={styles.statusLink}
          >
            {t("settings.agentForm.nativeStatus.recheck")}
          </button>
        </div>
      );
    }

    return (
      <div style={{ ...styles.nativeStatus, color: "var(--red)" }}>
        {t("settings.agentForm.nativeStatus.notDetected", { cli })}
        <button
          type="button"
          onClick={() => void openInstallGuide(cli)}
          style={styles.statusLink}
        >
          {t("settings.agentForm.nativeStatus.viewInstallGuide")}
        </button>
        <button
          type="button"
          onClick={() => void refreshDetect()}
          style={styles.statusLink}
        >
          {t("settings.agentForm.nativeStatus.recheck")}
        </button>
      </div>
    );
  }

  function moreOptionsSummary() {
    if (isBorrow) {
      return t("settings.agentForm.moreSummary.borrow");
    }
    if (isHarness) {
      return t("settings.agentForm.moreSummary.harness");
    }
    return t("settings.agentForm.moreSummary.native");
  }

  function renderModelAndReasoningFields() {
    const modelLabel =
      isNative || isHarness
        ? t("settings.agentForm.modelLabel")
        : t("settings.agentForm.primaryModelLabel");
    const reasoningLabel = t("settings.agentForm.reasoningLabel");

    return (
      <div style={styles.grid}>
        <Field label={modelLabel}>
          {showCustomModelInput ? (
            <>
              <input
                id="agent-primary-model"
                aria-label={modelLabel}
                placeholder={
                  isHarness
                    ? t("settings.agentForm.harnessModelPlaceholder")
                    : undefined
                }
                required={isBorrow}
                style={{ ...styles.input, ...styles.monoInput }}
                value={values.primaryModel}
                onChange={(event) => {
                  markModelFieldDirty("primaryModel");
                  // harness 只改模型不废测试态（测试内容是 GET /models，与选
                  // 哪个模型无关）；borrow/native 维持旧行为，改模型仍需重测。
                  if (!isHarness) {
                    resetTest({ keepLiveModels: harnessHasModelChoices });
                  }
                  setValue("primaryModel", event.currentTarget.value);
                }}
              />
              {isUnknownModel ? (
                <span style={{ ...styles.hint, color: "var(--amber)" }}>
                  {t("settings.agentForm.unknownModelWarning")}
                </span>
              ) : null}
              {!isHarness || harnessHasModelChoices ? (
                <button
                  type="button"
                  onClick={() => setCustomModel(false)}
                  style={{ ...styles.segmentButton, alignSelf: "flex-start" }}
                >
                  {t("settings.agentForm.fromList")}
                </button>
              ) : null}
            </>
          ) : (
            <ModelDropdown
              value={values.primaryModel}
              options={modelOptions}
              liveModels={liveModels}
              placeholder={
                isNative
                  ? t("settings.agentForm.modelPlaceholder.cliDefault")
                  : t("settings.agentForm.modelPlaceholder.select")
              }
              defaultOption={
                isHarness
                  ? {
                      value: "",
                      label: t("settings.agentForm.harnessDefaultModelOption"),
                    }
                  : undefined
              }
              onChange={(model) => {
                markModelFieldDirty("primaryModel");
                // harness 只改模型不废测试态（同上：测试内容与选哪个模型无
                // 关），否则用户改模型后保存按钮会被误挡要求重测。
                if (!isHarness) {
                  resetTest({ keepLiveModels: harnessHasModelChoices });
                }
                setValue("primaryModel", model);
              }}
              onSelectCustom={() => setCustomModel(true)}
            />
          )}
        </Field>

        <Field label={reasoningLabel}>
          <div aria-label={reasoningLabel} style={styles.segment}>
            {reasoningLevels.map((level) => (
              <SegmentButton
                key={level}
                active={selectedReasoningDefault === level}
                onClick={() => setValue("reasoningDefault", level)}
              >
                {level}
              </SegmentButton>
            ))}
            {reasoningLevels.length === 0 ? (
              <span style={styles.hint}>
                {t("settings.agentForm.reasoningDisabledHint")}
              </span>
            ) : null}
          </div>
        </Field>
      </div>
    );
  }

  function renderEndpointAndAuthFields() {
    if (!isBorrow && !isHarness) return null;

    return (
      <div style={styles.grid}>
        <Field label="Endpoint">
          <input
            id="agent-endpoint"
            aria-label="Endpoint"
            style={{ ...styles.input, ...styles.monoInput }}
            value={values.endpoint}
            onChange={(event) => {
              resetTest();
              setValue("endpoint", event.currentTarget.value);
            }}
          />
        </Field>

        {isBorrow ? (
          <Field label={t("settings.agentForm.authLabel")}>
            <div
              aria-label={t("settings.agentForm.authLabel")}
              style={styles.segment}
            >
              <SegmentButton
                active={values.authMode === AUTH_MODE.bearer}
                onClick={() => {
                  setValue("authMode", AUTH_MODE.bearer);
                  resetTest();
                }}
              >
                Bearer (ANTHROPIC_AUTH_TOKEN)
              </SegmentButton>
              <SegmentButton
                active={values.authMode === AUTH_MODE.xApiKey}
                onClick={() => {
                  setValue("authMode", AUTH_MODE.xApiKey);
                  resetTest();
                }}
              >
                x-api-key (ANTHROPIC_API_KEY)
              </SegmentButton>
            </div>
          </Field>
        ) : null}
      </div>
    );
  }

  function renderAutoMark(field: ModelFieldKey) {
    if (!modelFieldAutoFilled[field] || modelFieldDirty[field]) return null;
    return (
      <span
        style={{
          ...styles.hint,
          marginTop: 0,
          whiteSpace: "nowrap",
        }}
      >
        {t("settings.agentForm.autoMark")}
      </span>
    );
  }

  function renderMappingInput(
    field: Exclude<ModelFieldKey, "primaryModel">,
    label: string,
    value: string,
  ) {
    return (
      <Field label={label}>
        <div style={{ alignItems: "center", display: "flex", gap: 4 }}>
          <input
            aria-label={label}
            style={{ ...styles.input, ...styles.monoInput, minWidth: 0 }}
            value={value}
            onChange={(event) => {
              markModelFieldDirty(field);
              setValue(field, event.currentTarget.value);
            }}
          />
          {renderAutoMark(field)}
        </div>
      </Field>
    );
  }

  function renderModelMappingFields() {
    if (!isBorrow) return null;

    return (
      <>
        <Field label={t("settings.agentForm.modelMappingLabel")}>
          <div style={styles.grid}>
            {renderMappingInput("modelOpus", "opus", values.modelOpus)}
            {renderMappingInput("modelSonnet", "sonnet", values.modelSonnet)}
            {renderMappingInput("modelHaiku", "haiku", values.modelHaiku)}
            {renderMappingInput(
              "modelSubagent",
              "subagent",
              values.modelSubagent,
            )}
          </div>
        </Field>

        <div className="st-form-note">
          {t("settings.agentForm.modelMappingHint")}
        </div>
      </>
    );
  }

  function renderTimeoutFields() {
    if (!isBorrow && !isHarness) return null;

    return (
      <div style={styles.grid}>
        <Field label="api timeout" hint="ms">
          <input
            aria-label="api timeout"
            inputMode="numeric"
            style={{ ...styles.input, ...styles.monoInput }}
            value={values.apiTimeoutMs}
            onChange={(event) =>
              setValue("apiTimeoutMs", event.currentTarget.value)
            }
          />
        </Field>
        {isBorrow ? (
          <Field label="max output tokens">
            <input
              aria-label="max output tokens"
              inputMode="numeric"
              placeholder={t("settings.agentForm.maxOutputTokensPlaceholder")}
              style={styles.input}
              value={values.maxOutputTokens}
              onChange={(event) =>
                setValue("maxOutputTokens", event.currentTarget.value)
              }
            />
          </Field>
        ) : null}
      </div>
    );
  }

  function renderCompatibilityFields() {
    if (!isBorrow) return null;

    return (
      <Field label={t("settings.agentForm.compatLabel")}>
        <div style={styles.checks}>
          <label style={styles.checkLabel}>
            <input
              checked={values.compatDisableThinking}
              type="checkbox"
              onChange={(event) =>
                setValue("compatDisableThinking", event.currentTarget.checked)
              }
            />
            {t("settings.agentForm.compatDisableThinking")}
          </label>
          <label style={styles.checkLabel}>
            <input
              checked={values.compatDisableBetas}
              type="checkbox"
              onChange={(event) =>
                setValue("compatDisableBetas", event.currentTarget.checked)
              }
            />
            {t("settings.agentForm.compatDisableBetas")}
          </label>
          <label style={styles.checkLabel}>
            <input
              checked={values.compatDisableNonessential}
              type="checkbox"
              onChange={(event) =>
                setValue(
                  "compatDisableNonessential",
                  event.currentTarget.checked,
                )
              }
            />
            {t("settings.agentForm.compatDisableNonessential")}
          </label>
        </div>

        <Field label="compat proxy">
          <input
            aria-label="compat proxy"
            placeholder={t("settings.agentForm.compatProxyPlaceholder")}
            style={{ ...styles.input, ...styles.monoInput }}
            value={values.compatProxy}
            onChange={(event) =>
              setValue("compatProxy", event.currentTarget.value)
            }
          />
        </Field>
      </Field>
    );
  }

  return (
    <form
      aria-label={t("settings.agentForm.formAria")}
      className="st-form"
      onSubmit={(event) => void submit(event)}
    >
      <div style={styles.title}>
        {agent
          ? t("settings.agentForm.title.edit")
          : t("settings.agentForm.title.add")}
      </div>
      {isBorrow ? (
        <div className="st-form-note">
          {t("settings.agentForm.borrowIntro")}
        </div>
      ) : null}

      <div style={styles.sectionTitle}>{t("settings.agentForm.basic")}</div>
      {!isEditingNative ? (
        <>
          <Field label={t("settings.agentForm.engineLabel")}>
            <div
              aria-label={t("settings.agentForm.engineLabel")}
              style={{
                ...styles.engineRow,
                gridTemplateColumns: `repeat(${visibleEngines.length}, minmax(0, 1fr))`,
              }}
            >
              {visibleEngines.map((entry) => (
                <div
                  key={entry.engine}
                  onClick={() => applyProvider(defaultPresetForEngine(entry))}
                  style={{
                    ...styles.engineButton,
                    ...(currentEngineId === entry.engine
                      ? styles.engineButtonActive
                      : {}),
                  }}
                >
                  <button
                    type="button"
                    aria-label={entry.label}
                    aria-pressed={currentEngineId === entry.engine}
                    style={styles.engineSelectButton}
                  >
                    <span style={styles.engineName}>{entry.label}</span>
                    <span style={styles.engineDesc}>
                      {t(engineDescKey(entry.engine))}
                    </span>
                  </button>
                  {renderEngineStatus(entry)}
                </div>
              ))}
            </div>
          </Field>

          <Field label="LLM Provider">
            <div
              aria-label="LLM Provider"
              style={{ ...styles.providerGroup, gap: 8 }}
            >
              {providerGroups.map((group) => (
                <div key={group.kind} style={styles.providerGroup}>
                  {showProviderGroupLabels ? (
                    <span style={styles.providerGroupLabel}>
                      {groupKindLabel(group.kind, t)}
                    </span>
                  ) : null}
                  <div style={styles.segment}>
                    {group.presets.length === 0 &&
                    currentEngine.engine === "codex" &&
                    group.kind === "api_key" ? (
                      <span
                        aria-disabled="true"
                        style={styles.providerPlaceholder}
                      >
                        {t("settings.agentForm.providerUpcoming")}
                      </span>
                    ) : null}
                    {group.presets.map((preset) => {
                      const detectKey = engineDetectKey(currentEngine.engine);
                      const cliLoggedIn =
                        group.kind === "account" &&
                        detectKey &&
                        detect[detectKey]?.creds_hint === true;
                      const chipLabel =
                        group.kind === "account"
                          ? t("settings.agentForm.accountChip", {
                              account: nativeAccountName(preset.id),
                            })
                          : preset.id === "custom"
                            ? t("settings.agentForm.presetLabel.custom")
                            : preset.label;
                      return (
                        <SegmentButton
                          key={preset.id}
                          active={values.preset === preset.id}
                          onClick={() => applyProvider(preset.id)}
                        >
                          {chipLabel}
                          {cliLoggedIn ? (
                            <span style={styles.chipBadge}>
                              {t("settings.agentForm.cliLoggedIn")}
                            </span>
                          ) : null}
                        </SegmentButton>
                      );
                    })}
                  </div>
                </div>
              ))}
            </div>
          </Field>
        </>
      ) : null}
      {currentProvider().accessPoints.length >= 2 ? (
        <Field label={t("settings.agentForm.accessPointLabel")}>
          <div
            aria-label={t("settings.agentForm.accessPointLabel")}
            style={styles.segment}
          >
            {currentProvider().accessPoints.map((accessPoint) => (
              <SegmentButton
                key={accessPoint.id}
                active={accessPointId === accessPoint.id}
                onClick={() => applyAccessPoint(accessPoint.id)}
              >
                {`${accessPointLabel(accessPoint.id, t)} · ${accessPoint.domain}`}
              </SegmentButton>
            ))}
          </div>
        </Field>
      ) : null}
      {renderNativeRuntimeStatus()}
      {isBorrow ? (
        <div className="st-form-note">
          {t("settings.agentForm.borrowPresetHint")}
        </div>
      ) : null}
      {isHarness ? (
        <div className="st-form-note">
          {t("settings.agentForm.harnessHint")}
        </div>
      ) : null}

      <Field label={t("settings.agentForm.nameLabel")}>
        <input
          id="agent-name"
          aria-label={t("settings.agentForm.nameLabel")}
          placeholder={autoAgentName(values.preset, accessPointId ?? undefined)}
          required
          style={styles.input}
          value={values.name}
          onChange={(event) => {
            setNameDirty(true);
            setValue("name", event.currentTarget.value);
          }}
        />
      </Field>

      {isBorrow || isHarness ? (
        <Field label="API Key" hint={t("settings.agentForm.apiKeyHint")}>
          <div className="st-test-row">
            <input
              id="agent-api-key"
              aria-label="API Key"
              autoComplete="off"
              placeholder={
                hasStoredKey
                  ? t("settings.agentForm.existingKeyPlaceholder")
                  : ""
              }
              style={{ ...styles.input, ...styles.monoInput }}
              type={showKey ? "text" : "password"}
              value={values.apiKey}
              onChange={(event) => {
                resetTest();
                setValue("apiKey", event.currentTarget.value);
              }}
            />
            <button
              type="button"
              aria-label={
                showKey
                  ? t("settings.agentForm.hideApiKey")
                  : t("settings.agentForm.showApiKey")
              }
              className="st-test-btn"
              onClick={() => setShowKey((value) => !value)}
              style={{
                alignItems: "center",
                display: "inline-flex",
                justifyContent: "center",
                padding: "6px 8px",
              }}
            >
              {showKey ? (
                <svg
                  aria-hidden="true"
                  viewBox="0 0 24 24"
                  fill="none"
                  stroke="currentColor"
                  strokeWidth="2"
                  strokeLinecap="round"
                  strokeLinejoin="round"
                  width="18"
                  height="18"
                >
                  <path d="M2 12s3.5-7 10-7 10 7 10 7-3.5 7-10 7-10-7-10-7Z" />
                  <circle cx="12" cy="12" r="3" />
                </svg>
              ) : (
                <svg
                  aria-hidden="true"
                  viewBox="0 0 24 24"
                  fill="none"
                  stroke="currentColor"
                  strokeWidth="2"
                  strokeLinecap="round"
                  strokeLinejoin="round"
                  width="18"
                  height="18"
                >
                  <path d="M9.88 9.88A3 3 0 0 0 12 15a3 3 0 0 0 2.12-.88" />
                  <path d="M10.73 5.08A10.5 10.5 0 0 1 12 5c6.5 0 10 7 10 7a18 18 0 0 1-3.23 4.31" />
                  <path d="M6.61 6.61C3.75 8.55 2 12 2 12s3.5 7 10 7a9.8 9.8 0 0 0 5.39-1.61" />
                  <line x1="2" y1="2" x2="22" y2="22" />
                </svg>
              )}
            </button>
            {isBorrow || isHarness ? (
              <button
                type="button"
                className="st-test-btn"
                data-testid="test-conn-btn"
                disabled={testState === "testing"}
                onClick={() => void runTest()}
              >
                {testState === "testing"
                  ? t("settings.agentForm.testing")
                  : t("settings.agentForm.testConnection")}
              </button>
            ) : null}
          </div>
          <span style={styles.hint}>
            {t("settings.agentForm.keyStatusPrefix")}
            <strong>
              {hasStoredKey
                ? t("settings.agentKeyState.configured")
                : t("settings.agentKeyState.missing")}
            </strong>
            {t("settings.agentForm.keepStoredKeyHint")}
          </span>
          {showBorrowKeyWarning ? (
            <span style={styles.hint}>
              {t("settings.agentForm.borrowKeyMissing")}
            </span>
          ) : null}
          {multiAP && hasKey ? (
            <span style={styles.hint}>
              {currentAccessPoint()?.keyHint
                ? t("settings.agentForm.multiAp.keyHint", {
                    accessPoints: provider.accessPoints
                      .map((ap) => accessPointLabel(ap.id, t))
                      .join(" / "),
                    keyHint: currentAccessPoint()!.keyHint!,
                  })
                : t("settings.agentForm.multiAp.noKeyHint", {
                    accessPoints: provider.accessPoints
                      .map((ap) => accessPointLabel(ap.id, t))
                      .join(" / "),
                  })}
            </span>
          ) : null}
        </Field>
      ) : null}

      {(isBorrow || isHarness) && testState !== "idle" ? (
        <div
          data-testid="test-state"
          aria-live="polite"
          className={`st-test-state ${
            testState === "ok" ? "ok" : testState === "err" ? "err" : ""
          }`}
        >
          {testState === "testing" ? t("settings.agentForm.testing") : null}
          {testState === "ok" ? (
            <>
              {t("settings.agentForm.testSuccess")}
              {isHarness
                ? t("settings.agentForm.testSuccessFetchedHarness", {
                    n: liveModels.length,
                  })
                : liveModels.length
                  ? t("settings.agentForm.testSuccessFetchedBorrow", {
                      n: liveModels.length,
                    })
                  : ""}
            </>
          ) : null}
          {testState === "err" ? (
            <>
              {t(
                CATEGORY_LABEL_KEYS[testResult?.category ?? "other"] ??
                  CATEGORY_LABEL_KEYS.other,
              )}
              <button
                type="button"
                aria-expanded={rawOpen}
                onClick={() => setRawOpen((open) => !open)}
              >
                {t("settings.agentForm.rawErrorToggle")} {rawOpen ? "▾" : "▸"}
              </button>
              {rawOpen ? (
                <div className="st-test-raw">{testResult?.raw_error}</div>
              ) : null}
            </>
          ) : null}
        </div>
      ) : null}

      <button
        type="button"
        aria-controls="agent-form-more-options"
        aria-expanded={advancedOpen}
        onClick={() => setAdvancedOpen((open) => !open)}
        style={styles.toggle}
      >
        {advancedOpen ? "▾" : "▸"} {t("settings.agentForm.moreOptions")}{" "}
        <span style={{ ...styles.hint, marginLeft: 6, marginTop: 0 }}>
          {moreOptionsSummary()}
        </span>
      </button>

      {advancedOpen ? (
        <div id="agent-form-more-options">
          {isBorrow ? (
            <div className="st-runline">
              <span style={styles.label}>
                {t("settings.agentForm.runModeLabel")}
              </span>
              <span className="st-agent-chip cc">
                {t("settings.agentAccess.borrow")}
              </span>
              <span style={styles.hint}>
                {t("settings.agentForm.runModeHint")}
              </span>
            </div>
          ) : null}

          {renderModelAndReasoningFields()}
          {renderEndpointAndAuthFields()}
          {renderModelMappingFields()}
          {renderTimeoutFields()}
          {renderCompatibilityFields()}
        </div>
      ) : null}

      <div style={styles.actions}>
        {saveBlockedReason ? (
          <span style={styles.saveWhy}>{saveBlockedReason}</span>
        ) : null}
        <button type="button" className="ob-btn" onClick={onCancel}>
          {t("settings.agentForm.cancel")}
        </button>
        <button
          type="submit"
          className="ob-btn primary"
          disabled={submitting || saveBlocked}
        >
          {agent ? t("settings.agentForm.save") : t("settings.agentForm.add")}
        </button>
      </div>
      {error ? <div style={styles.error}>{error}</div> : null}
    </form>
  );
}

export type PresetId = "deepseek" | "zai" | "bigmodel" | "kimi" | "custom";
export type AccessMode = "native" | "borrow" | "harness";

export const CUSTOM_MODEL_SENTINEL = "__custom__";

// auth_mode 统一下划线（与后端 DB CHECK 'x_api_key' 一致）
export const AUTH_MODE = { bearer: "bearer", xApiKey: "x_api_key" } as const;

export type ProviderId =
  | "claude"
  | "codex"
  | "deepseek"
  | "harness-deepseek"
  | "harness-glm"
  | "harness-kimi"
  | "kimi"
  | "zhipu"
  | "custom";

export type AccessPoint = {
  id: string;
  label: string;
  domain: string;
  endpoint: string;
  modelsEndpoint?: string;
  knownModels: string[];
  primaryModel: string;
  mapping: { opus: string; sonnet: string; haiku: string; subagent: string };
  apiTimeoutMs?: number;
  keyHint?: string;
};
export type ProviderPreset = {
  id: ProviderId;
  label: string;
  access: AccessMode;
  providerValue: string;
  accessPoints: AccessPoint[];
  authMode: "bearer" | "";
  compatDisableBetas: boolean;
  compatDisableNonessential: boolean;
  compatDisableThinking: boolean;
  compatProxy?: string;
  nativeModels?: string[];
  nativePrimaryModel?: string;
  nativeMapping?: {
    opus: string;
    sonnet: string;
    haiku: string;
    subagent: string;
  };
  nativeCapReasoning?: string | null;
  nativeCapLead?: string | null;
};

const KIMI_MODELS = ["kimi-k2.5", "kimi-k2.6"];
const KIMI_MAP = {
  opus: "kimi-k2.5",
  sonnet: "kimi-k2.5",
  haiku: "kimi-k2.5",
  subagent: "kimi-k2.5",
};
const GLM_MODELS = ["glm-4.7", "glm-4.5-air"];
const GLM_MAP = {
  opus: "glm-4.7",
  sonnet: "glm-4.7",
  haiku: "glm-4.5-air",
  subagent: "glm-4.5-air",
};

export const PROVIDER_PRESETS: ProviderPreset[] = [
  {
    id: "claude",
    label: "Claude CLI",
    access: "native",
    providerValue: "claude",
    authMode: "",
    compatDisableBetas: false,
    compatDisableNonessential: false,
    compatDisableThinking: false,
    accessPoints: [],
    nativeModels: ["fable", "sonnet", "opus", "haiku"],
    nativePrimaryModel: "sonnet",
    nativeMapping: {
      opus: "opus",
      sonnet: "sonnet",
      haiku: "haiku",
      subagent: "sonnet",
    },
    nativeCapReasoning: "low,medium,high,xhigh,max",
    nativeCapLead: "native_cli",
  },
  {
    id: "codex",
    label: "Codex CLI",
    access: "native",
    providerValue: "codex",
    authMode: "",
    compatDisableBetas: false,
    compatDisableNonessential: false,
    compatDisableThinking: false,
    accessPoints: [],
    nativeModels: ["gpt-5.6-sol", "gpt-5.5", "gpt-5.4"],
    nativePrimaryModel: "gpt-5",
    nativeMapping: {
      opus: "gpt-5",
      sonnet: "gpt-5",
      haiku: "gpt-5",
      subagent: "gpt-5",
    },
    nativeCapReasoning: "minimal,low,medium,high,xhigh",
    nativeCapLead: null,
  },
  {
    id: "deepseek",
    label: "DeepSeek",
    access: "borrow",
    providerValue: "deepseek",
    authMode: "bearer",
    compatDisableBetas: false,
    compatDisableNonessential: true,
    compatDisableThinking: false,
    compatProxy: "thinking_passback",
    accessPoints: [
      {
        id: "default",
        label: "默认",
        domain: "api.deepseek.com",
        endpoint: "https://api.deepseek.com/anthropic",
        modelsEndpoint: "https://api.deepseek.com/models",
        knownModels: ["deepseek-v4-pro", "deepseek-v4-flash"],
        primaryModel: "deepseek-v4-pro",
        mapping: {
          opus: "deepseek-v4-pro",
          sonnet: "deepseek-v4-pro",
          haiku: "deepseek-v4-flash",
          subagent: "deepseek-v4-flash",
        },
        apiTimeoutMs: 600000,
      },
    ],
  },
  {
    id: "kimi",
    label: "Kimi",
    access: "borrow",
    providerValue: "kimi",
    authMode: "bearer",
    compatDisableBetas: false,
    compatDisableNonessential: true,
    compatDisableThinking: false,
    accessPoints: [
      {
        id: "cn",
        label: "中国区",
        domain: "api.moonshot.cn",
        endpoint: "https://api.moonshot.cn/anthropic",
        modelsEndpoint: "https://api.moonshot.cn/v1/models",
        knownModels: KIMI_MODELS,
        primaryModel: "kimi-k2.5",
        mapping: KIMI_MAP,
        apiTimeoutMs: 600000,
        keyHint: "platform.moonshot.cn",
      },
      {
        id: "intl",
        label: "国际区",
        domain: "api.moonshot.ai",
        endpoint: "https://api.moonshot.ai/anthropic",
        modelsEndpoint: "https://api.moonshot.ai/v1/models",
        knownModels: KIMI_MODELS,
        primaryModel: "kimi-k2.5",
        mapping: KIMI_MAP,
        apiTimeoutMs: 600000,
        keyHint: "platform.moonshot.ai / kimi.ai",
      },
    ],
  },
  {
    id: "zhipu",
    label: "智谱 GLM",
    access: "borrow",
    providerValue: "zhipu",
    authMode: "bearer",
    compatDisableBetas: false,
    compatDisableNonessential: true,
    compatDisableThinking: false,
    accessPoints: [
      {
        id: "cn",
        label: "中国",
        domain: "open.bigmodel.cn",
        endpoint: "https://open.bigmodel.cn/api/anthropic",
        modelsEndpoint: "https://open.bigmodel.cn/api/paas/v4/models",
        knownModels: GLM_MODELS,
        primaryModel: "glm-4.7",
        mapping: GLM_MAP,
        apiTimeoutMs: 600000,
      },
      {
        id: "intl",
        label: "国际",
        domain: "z.ai",
        endpoint: "https://api.z.ai/api/anthropic",
        modelsEndpoint: "https://api.z.ai/api/paas/v4/models",
        knownModels: GLM_MODELS,
        primaryModel: "glm-4.7",
        mapping: GLM_MAP,
        apiTimeoutMs: 3000000,
      },
    ],
  },
  {
    id: "custom",
    label: "自定义",
    access: "borrow",
    providerValue: "",
    authMode: "bearer",
    compatDisableBetas: false,
    compatDisableNonessential: false,
    compatDisableThinking: false,
    accessPoints: [],
  },
  {
    id: "harness-deepseek",
    label: "DeepSeek",
    access: "harness",
    providerValue: "deepseek",
    authMode: "",
    compatDisableBetas: false,
    compatDisableNonessential: false,
    compatDisableThinking: false,
    accessPoints: [
      {
        id: "default",
        label: "默认",
        domain: "api.deepseek.com",
        endpoint: "https://api.deepseek.com/v1",
        modelsEndpoint: "https://api.deepseek.com/v1/models",
        knownModels: [],
        primaryModel: "",
        mapping: { opus: "", sonnet: "", haiku: "", subagent: "" },
        apiTimeoutMs: 600000,
      },
    ],
  },
  {
    id: "harness-glm",
    label: "GLM · 智谱",
    access: "harness",
    providerValue: "glm",
    authMode: "",
    compatDisableBetas: false,
    compatDisableNonessential: false,
    compatDisableThinking: false,
    accessPoints: [
      {
        id: "cn",
        label: "中国",
        domain: "open.bigmodel.cn",
        endpoint: "https://open.bigmodel.cn/api/paas/v4",
        modelsEndpoint: "https://open.bigmodel.cn/api/paas/v4/models",
        knownModels: [],
        primaryModel: "",
        mapping: { opus: "", sonnet: "", haiku: "", subagent: "" },
        apiTimeoutMs: 600000,
      },
      {
        id: "intl",
        label: "国际",
        domain: "z.ai",
        endpoint: "https://api.z.ai/api/paas/v4",
        modelsEndpoint: "https://api.z.ai/api/paas/v4/models",
        knownModels: [],
        primaryModel: "",
        mapping: { opus: "", sonnet: "", haiku: "", subagent: "" },
        apiTimeoutMs: 600000,
      },
      {
        id: "cn-coding",
        label: "中国 · Coding 套餐",
        domain: "open.bigmodel.cn",
        endpoint: "https://open.bigmodel.cn/api/coding/paas/v4",
        modelsEndpoint: "https://open.bigmodel.cn/api/coding/paas/v4/models",
        knownModels: [],
        primaryModel: "",
        mapping: { opus: "", sonnet: "", haiku: "", subagent: "" },
        apiTimeoutMs: 600000,
      },
      {
        id: "intl-coding",
        label: "国际 · Coding 套餐",
        domain: "z.ai",
        endpoint: "https://api.z.ai/api/coding/paas/v4",
        modelsEndpoint: "https://api.z.ai/api/coding/paas/v4/models",
        knownModels: [],
        primaryModel: "",
        mapping: { opus: "", sonnet: "", haiku: "", subagent: "" },
        apiTimeoutMs: 600000,
      },
    ],
  },
  {
    id: "harness-kimi",
    label: "Kimi",
    access: "harness",
    providerValue: "kimi",
    authMode: "",
    compatDisableBetas: false,
    compatDisableNonessential: false,
    compatDisableThinking: false,
    accessPoints: [
      {
        id: "cn",
        label: "中国",
        domain: "api.moonshot.cn",
        endpoint: "https://api.moonshot.cn/v1",
        modelsEndpoint: "https://api.moonshot.cn/v1/models",
        knownModels: [],
        primaryModel: "",
        mapping: { opus: "", sonnet: "", haiku: "", subagent: "" },
        apiTimeoutMs: 600000,
      },
      {
        id: "intl",
        label: "国际",
        domain: "api.moonshot.ai",
        endpoint: "https://api.moonshot.ai/v1",
        modelsEndpoint: "https://api.moonshot.ai/v1/models",
        knownModels: [],
        primaryModel: "",
        mapping: { opus: "", sonnet: "", haiku: "", subagent: "" },
        apiTimeoutMs: 600000,
      },
    ],
  },
];

export function classifyModelsFetchError(raw: string): string {
  if (raw === "missing_key") return "missing_key";
  const match = raw.match(/HTTP (\d{3})/);
  if (match) {
    const code = Number(match[1]);
    if (code === 401 || code === 403) return "auth";
    if (code === 429) return "rate_limit";
    if (code === 404) return "not_found";
    return "other";
  }
  return "network";
}

/** parse 失败→null（坏 endpoint 不崩）；成功→origin+pathname（host 小写·去尾斜杠）。精确匹配·禁 substring。 */
export function normalizeEndpoint(url: string): string | null {
  const t = url.trim();
  if (!t) return null;
  try {
    const u = new URL(t);
    return `${u.protocol}//${u.host.toLowerCase()}${u.pathname.replace(/\/+$/, "")}`;
  } catch {
    return null;
  }
}

/**
 * 拉取某接入点模型列表用的 URL——唯一真相源。
 * endpoint 与接入点匹配时优先用其显式配置的 modelsEndpoint（接入点的 models 路径
 * 未必等于 endpoint + "/models"——borrow 的 anthropic 路径就完全不同）；
 * 否则回退按 OpenAI 惯例拼接（custom harness 自填 endpoint 场景）。
 */
export function resolveModelsEndpoint(
  endpoint: string,
  accessPoint?: Pick<AccessPoint, "endpoint" | "modelsEndpoint">,
): string {
  const norm = normalizeEndpoint(endpoint);
  if (
    norm !== null &&
    norm === normalizeEndpoint(accessPoint?.endpoint ?? "") &&
    accessPoint?.modelsEndpoint
  ) {
    return accessPoint.modelsEndpoint;
  }
  return `${endpoint.trim().replace(/\/+$/, "")}/models`;
}

export type InferResult = {
  providerId: ProviderId;
  accessPointId: string | null;
};

export function inferProviderAccessPoint(agent: {
  endpoint?: string | null;
  provider?: string;
  access?: string;
}): InferResult {
  const ep = (agent.endpoint ?? "").trim();
  if (ep) {
    const norm = normalizeEndpoint(ep);
    if (norm) {
      for (const p of PROVIDER_PRESETS) {
        if (p.id === "custom") continue;
        for (const ap of p.accessPoints) {
          if (normalizeEndpoint(ap.endpoint) === norm) {
            return { providerId: p.id, accessPointId: ap.id };
          }
        }
      }
    }
    if (agent.access === "harness") {
      const prov = (agent.provider ?? "").toLowerCase();
      if (prov === "deepseek") {
        return { providerId: "harness-deepseek", accessPointId: null };
      }
      if (prov === "glm") {
        return { providerId: "harness-glm", accessPointId: null };
      }
      if (prov === "kimi") {
        return { providerId: "harness-kimi", accessPointId: null };
      }
    }
    return { providerId: "custom", accessPointId: null }; // 非空不匹配/坏 URL → custom
  }
  const prov = (agent.provider ?? "").toLowerCase();
  if (prov === "claude" || prov === "anthropic") {
    return { providerId: "claude", accessPointId: null };
  }
  if (prov === "codex" || prov === "openai") {
    return { providerId: "codex", accessPointId: null };
  }
  if (prov === "z.ai" || prov === "bigmodel" || prov === "zhipu") {
    return { providerId: "zhipu", accessPointId: "cn" };
  }
  if (prov === "kimi") return { providerId: "kimi", accessPointId: "cn" };
  if (prov === "deepseek") {
    return { providerId: "deepseek", accessPointId: "default" };
  }
  return { providerId: "custom", accessPointId: null };
}

export function deriveAccess(
  preset: ProviderId,
  existingAccess?: string,
): AccessMode {
  if (existingAccess === "native") return "native";
  if (existingAccess === "borrow") return "borrow";
  if (existingAccess === "harness") return "harness";
  return (
    PROVIDER_PRESETS.find((provider) => provider.id === preset)?.access ??
    "borrow"
  );
}

export function mergeModelOptions(
  staticKnown: string[],
  liveCached: string[],
  currentValue: string,
): string[] {
  const out: string[] = [];
  const seen = new Set<string>();
  for (const m of [...staticKnown, ...liveCached]) {
    const t = m.trim();
    if (t && t !== CUSTOM_MODEL_SENTINEL && !seen.has(t)) {
      seen.add(t);
      out.push(t);
    }
  }
  const cur = currentValue.trim();
  if (cur && cur !== CUSTOM_MODEL_SENTINEL && !seen.has(cur)) out.push(cur);
  out.push(CUSTOM_MODEL_SENTINEL);
  return out;
}

const CACHE_TTL_MS = 24 * 60 * 60 * 1000;
function cacheKey(preset: string, endpoint: string): string {
  return `agentloom:models:${preset}:${normalizeEndpoint(endpoint) ?? endpoint}`;
}

export function writeModelCache(
  preset: string,
  endpoint: string,
  models: string[],
): void {
  const normalizedEndpoint = normalizeEndpoint(endpoint) ?? endpoint;
  try {
    localStorage.setItem(
      cacheKey(preset, endpoint),
      JSON.stringify({
        models,
        endpoint: normalizedEndpoint,
        fetchedAt: Date.now(),
      }),
    );
  } catch {
    /* localStorage 不可用·静默 */
  }
}

export function readModelCache(
  preset: string,
  endpoint: string,
): string[] | null {
  try {
    const raw = localStorage.getItem(cacheKey(preset, endpoint));
    if (!raw) return null;
    const normalizedEndpoint = normalizeEndpoint(endpoint) ?? endpoint;
    const p = JSON.parse(raw) as {
      models?: unknown;
      endpoint?: unknown;
      fetchedAt?: unknown;
    };
    if (
      !Array.isArray(p.models) ||
      p.endpoint !== normalizedEndpoint ||
      typeof p.fetchedAt !== "number" ||
      Date.now() - p.fetchedAt > CACHE_TTL_MS
    )
      return null;
    return p.models.filter((m): m is string => typeof m === "string");
  } catch {
    return null;
  }
}

export type EngineId = "claude-code" | "codex" | "myagent";
export type PresetMeta = { id: ProviderId; label: string; access: AccessMode };
export type EngineViewEntry = {
  engine: EngineId;
  label: string;
  desc: string;
  groups: Array<{ kind: "account" | "api_key"; presets: PresetMeta[] }>;
};

const providerMeta = (id: ProviderId): PresetMeta => {
  const preset = PROVIDER_PRESETS.find((provider) => provider.id === id);
  if (!preset) throw new Error(`Missing provider preset: ${id}`);
  return preset;
};

export function engineView(): EngineViewEntry[] {
  return [
    {
      engine: "claude-code",
      label: "Claude Code CLI",
      desc: "本机 claude 命令。可跑 Anthropic 自家，也可借壳跑别家",
      groups: [
        { kind: "account", presets: [providerMeta("claude")] },
        {
          kind: "api_key",
          presets: [
            providerMeta("deepseek"),
            providerMeta("kimi"),
            providerMeta("zhipu"),
            providerMeta("custom"),
          ],
        },
      ],
    },
    {
      engine: "codex",
      label: "Codex CLI",
      desc: "本机 codex 命令。跑 OpenAI 自家模型",
      groups: [
        { kind: "account", presets: [providerMeta("codex")] },
        { kind: "api_key", presets: [] },
      ],
    },
    {
      engine: "myagent",
      label: "myagent",
      desc: "自研 harness，直连各家 API",
      groups: [
        {
          kind: "api_key",
          presets: [
            providerMeta("harness-deepseek"),
            providerMeta("harness-glm"),
            providerMeta("harness-kimi"),
          ],
        },
      ],
    },
  ];
}

export function autoAgentName(
  presetId: ProviderId,
  accessPointId?: string,
): string {
  const preset = PROVIDER_PRESETS.find((provider) => provider.id === presetId);
  if (!preset) return presetId;

  if (preset.access === "native") return preset.label;
  if (preset.access === "harness") return `${preset.label}（myagent）`;

  if (preset.accessPoints.length > 1 && accessPointId) {
    const accessPoint = preset.accessPoints.find(
      (candidate) => candidate.id === accessPointId,
    );
    if (accessPoint) {
      return `${preset.label} ${accessPoint.label}（Claude Code 借壳）`;
    }
  }
  return `${preset.label}（Claude Code 借壳）`;
}

export type ModelMapping = {
  primary?: string;
  opus?: string;
  sonnet?: string;
  haiku?: string;
  subagent?: string;
};

type ParsedModelId = {
  id: string;
  version: number | null;
  hasLightSuffix: boolean;
  hasExcludedSuffix: boolean;
};

const LIGHT_SUFFIXES = ["air", "flash", "lite", "mini"];
const EXCLUDED_SUFFIXES = ["preview", "beta"];

function parseModelId(id: string): ParsedModelId {
  const normalized = id.toLowerCase();
  const versionMatches = Array.from(
    // Supports ids like glm-5, glm-4.7, kimi-k2, and kimi-k2.5.
    normalized.matchAll(/(?:^|[-_a-z])(\d+(?:\.\d+)*)(?=$|[-_a-z])/g),
  );
  const versionToken = versionMatches[versionMatches.length - 1]?.[1];
  const version = versionToken === undefined ? null : Number(versionToken);

  return {
    id,
    version: Number.isFinite(version) ? version : null,
    hasLightSuffix: LIGHT_SUFFIXES.some((suffix) =>
      normalized.includes(suffix),
    ),
    hasExcludedSuffix: EXCLUDED_SUFFIXES.some((suffix) =>
      normalized.includes(suffix),
    ),
  };
}

function pickHighestVersion(models: ParsedModelId[]): ParsedModelId {
  return models.reduce((best, candidate) => {
    if (candidate.version! > best.version!) return candidate;
    if (
      candidate.version === best.version &&
      candidate.id.length < best.id.length
    ) {
      return candidate;
    }
    return best;
  });
}

export function deriveModelMapping(modelIds: string[]): ModelMapping | null {
  if (modelIds.length === 0) return null;

  const versioned = modelIds
    .map(parseModelId)
    .filter((model): model is ParsedModelId & { version: number } => {
      return model.version !== null;
    });
  if (versioned.length === 0) return null;

  const mainlineCandidates = versioned.filter(
    (model) => !model.hasLightSuffix && !model.hasExcludedSuffix,
  );
  const flagshipCandidates =
    mainlineCandidates.length > 0
      ? mainlineCandidates
      : versioned.filter((model) => !model.hasExcludedSuffix);
  const flagship = pickHighestVersion(
    flagshipCandidates.length > 0 ? flagshipCandidates : versioned,
  );

  const sameVersionLight = versioned.filter(
    (model) => model.hasLightSuffix && model.version === flagship.version,
  );
  const lightCandidates =
    sameVersionLight.length > 0
      ? sameVersionLight
      : versioned.filter((model) => model.hasLightSuffix);
  const haiku =
    lightCandidates.length > 0
      ? pickHighestVersion(lightCandidates).id
      : flagship.id;

  return {
    primary: flagship.id,
    opus: flagship.id,
    sonnet: flagship.id,
    haiku,
    subagent: flagship.id,
  };
}

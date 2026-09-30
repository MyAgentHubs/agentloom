export type PresetId = "deepseek" | "zai" | "bigmodel" | "kimi" | "custom";
export type AccessMode = "native" | "borrow" | "harness";

export const CUSTOM_MODEL_SENTINEL = "__custom__";

// auth_mode uniformly uses underscores (consistent with the backend DB CHECK 'x_api_key')
export const AUTH_MODE = { bearer: "bearer", xApiKey: "x_api_key" } as const;

export type ProviderId =
  | "claude"
  | "codex"
  | "deepseek"
  | "harness-deepseek"
  | "harness-gemini"
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
    nativeModels: [
      "gpt-6-astra",
      "gpt-6-sol",
      "gpt-6-luna",
      "gpt-5.6-sol",
      "gpt-5.6-terra",
      "gpt-5.6-luna",
      "gpt-5.5",
    ],
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
    id: "harness-gemini",
    label: "Gemini",
    access: "harness",
    providerValue: "gemini",
    authMode: "",
    compatDisableBetas: false,
    compatDisableNonessential: false,
    compatDisableThinking: false,
    accessPoints: [
      {
        id: "default",
        label: "默认",
        domain: "generativelanguage.googleapis.com",
        endpoint: "https://generativelanguage.googleapis.com/v1beta/openai/",
        modelsEndpoint:
          "https://generativelanguage.googleapis.com/v1beta/openai/models",
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

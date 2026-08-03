import { describe, it, expect, beforeEach, vi } from "vitest";
import {
  AUTH_MODE,
  CUSTOM_MODEL_SENTINEL,
  autoAgentName,
  classifyModelsFetchError,
  deriveAccess,
  deriveModelMapping,
  engineView,
  inferProviderAccessPoint,
  mergeModelOptions,
  normalizeEndpoint,
  PROVIDER_PRESETS,
  readModelCache,
  resolveModelsEndpoint,
  writeModelCache,
} from "./agentFormHelpers";

const byId = (id: string) => PROVIDER_PRESETS.find((p) => p.id === id)!;
const staticModelsFor = (providerId: string, accessPointId?: string) => {
  const provider = byId(providerId);
  return (
    provider.accessPoints.find((ap) => ap.id === accessPointId)?.knownModels ??
    provider.nativeModels ??
    []
  );
};
const usesCustomStaticModelInput = (
  providerId: string,
  accessPointId: string | undefined,
  model: string,
) => !staticModelsFor(providerId, accessPointId).includes(model);
const ag = (
  p: Partial<{ endpoint: string | null; provider: string; access: string }>,
) => ({
  endpoint: null,
  provider: "",
  ...p,
});

describe("PROVIDER_PRESETS 配置表", () => {
  it("Claude native 模型以 fable 为首项并保留现有别名", () => {
    expect(staticModelsFor("claude")).toEqual([
      "fable",
      "sonnet",
      "opus",
      "haiku",
    ]);
  });

  it("Codex native 模型按新到旧暴露当前版本", () => {
    expect(staticModelsFor("codex")).toEqual([
      "gpt-5.6-sol",
      "gpt-5.5",
      "gpt-5.4",
    ]);
    expect(usesCustomStaticModelInput("codex", undefined, "gpt-5")).toBe(true);
  });

  it("从 provider 真相源区分已知与未知静态模型", () => {
    expect(staticModelsFor("kimi", "cn")).toEqual(["kimi-k2.5", "kimi-k2.6"]);
    expect(staticModelsFor("zhipu", "cn")).toEqual(["glm-4.7", "glm-4.5-air"]);
    expect(usesCustomStaticModelInput("kimi", "cn", "kimi-k2.6")).toBe(false);
    expect(usesCustomStaticModelInput("kimi", "cn", "kimi-k2-turbo")).toBe(
      true,
    );
    expect(usesCustomStaticModelInput("zhipu", "cn", "glm-4.7")).toBe(false);
    expect(usesCustomStaticModelInput("zhipu", "cn", "glm-4.6")).toBe(true);
  });

  it("含 native claude/codex 和 borrow provider·按 access 暴露默认入口", () => {
    expect(PROVIDER_PRESETS.map((p) => p.id).sort()).toEqual([
      "claude",
      "codex",
      "custom",
      "deepseek",
      "harness-deepseek",
      "harness-glm",
      "harness-kimi",
      "kimi",
      "zhipu",
    ]);
    expect(byId("claude")).toMatchObject({
      access: "native",
      providerValue: "claude",
      nativeCapReasoning: "low,medium,high,xhigh,max",
      nativeCapLead: "native_cli",
    });
    expect(byId("codex")).toMatchObject({
      access: "native",
      providerValue: "codex",
      nativeCapReasoning: "minimal,low,medium,high,xhigh",
      nativeCapLead: null,
    });
    for (const id of ["deepseek", "kimi", "zhipu", "custom"]) {
      expect(byId(id)).toMatchObject({ access: "borrow" });
    }
  });

  it("含 myagent harness 直连 provider 预设", () => {
    const harnessPresets = [
      {
        id: "harness-deepseek",
        label: "DeepSeek",
        providerValue: "deepseek",
        accessPoints: [
          {
            id: "default",
            label: "默认",
            endpoint: "https://api.deepseek.com/v1",
            domain: "api.deepseek.com",
            modelsEndpoint: "https://api.deepseek.com/v1/models",
          },
        ],
      },
      {
        id: "harness-glm",
        label: "GLM · 智谱",
        providerValue: "glm",
        accessPoints: [
          {
            id: "cn",
            label: "中国",
            endpoint: "https://open.bigmodel.cn/api/paas/v4",
            domain: "open.bigmodel.cn",
            modelsEndpoint: "https://open.bigmodel.cn/api/paas/v4/models",
          },
          {
            id: "intl",
            label: "国际",
            endpoint: "https://api.z.ai/api/paas/v4",
            domain: "z.ai",
            modelsEndpoint: "https://api.z.ai/api/paas/v4/models",
          },
          {
            id: "cn-coding",
            label: "中国 · Coding 套餐",
            endpoint: "https://open.bigmodel.cn/api/coding/paas/v4",
            domain: "open.bigmodel.cn",
            modelsEndpoint:
              "https://open.bigmodel.cn/api/coding/paas/v4/models",
          },
          {
            id: "intl-coding",
            label: "国际 · Coding 套餐",
            endpoint: "https://api.z.ai/api/coding/paas/v4",
            domain: "z.ai",
            modelsEndpoint: "https://api.z.ai/api/coding/paas/v4/models",
          },
        ],
      },
      {
        id: "harness-kimi",
        label: "Kimi",
        providerValue: "kimi",
        accessPoints: [
          {
            id: "cn",
            label: "中国",
            endpoint: "https://api.moonshot.cn/v1",
            domain: "api.moonshot.cn",
            modelsEndpoint: "https://api.moonshot.cn/v1/models",
          },
          {
            id: "intl",
            label: "国际",
            endpoint: "https://api.moonshot.ai/v1",
            domain: "api.moonshot.ai",
            modelsEndpoint: "https://api.moonshot.ai/v1/models",
          },
        ],
      },
    ];

    for (const expected of harnessPresets) {
      const preset = byId(expected.id);
      expect(preset).toMatchObject({
        id: expected.id,
        label: expected.label,
        access: "harness",
        providerValue: expected.providerValue,
        authMode: "",
        compatDisableBetas: false,
        compatDisableNonessential: false,
        compatDisableThinking: false,
      });
      expect(preset.accessPoints).toEqual(
        expected.accessPoints.map((ap) => ({
          ...ap,
          knownModels: [],
          primaryModel: "",
          mapping: { opus: "", sonnet: "", haiku: "", subagent: "" },
          apiTimeoutMs: 600000,
        })),
      );
    }
  });

  const HARNESS_EXPECT = {
    "harness-glm": [
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
    "harness-kimi": [
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
  };

  for (const [pid, aps] of Object.entries(HARNESS_EXPECT)) {
    it(`${pid} harness accessPoints 全字段精确`, () => {
      const got = byId(pid).accessPoints;
      expect(got).toHaveLength(aps.length);
      aps.forEach((expected, index) => {
        expect(got[index]).toEqual(expected);
      });
    });
  }

  // table-driven 精确断言每个 accessPoint 全字段（防 kimi-k2/glm-4.6 等旧值回归）
  const EXPECT: Record<string, any[]> = {
    deepseek: [
      {
        id: "default",
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
        keyHint: undefined,
      },
    ],
    kimi: [
      {
        id: "cn",
        endpoint: "https://api.moonshot.cn/anthropic",
        modelsEndpoint: "https://api.moonshot.cn/v1/models",
        knownModels: ["kimi-k2.5", "kimi-k2.6"],
        primaryModel: "kimi-k2.5",
        mapping: {
          opus: "kimi-k2.5",
          sonnet: "kimi-k2.5",
          haiku: "kimi-k2.5",
          subagent: "kimi-k2.5",
        },
        apiTimeoutMs: 600000,
        keyHint: "platform.moonshot.cn",
      },
      {
        id: "intl",
        endpoint: "https://api.moonshot.ai/anthropic",
        modelsEndpoint: "https://api.moonshot.ai/v1/models",
        knownModels: ["kimi-k2.5", "kimi-k2.6"],
        primaryModel: "kimi-k2.5",
        mapping: {
          opus: "kimi-k2.5",
          sonnet: "kimi-k2.5",
          haiku: "kimi-k2.5",
          subagent: "kimi-k2.5",
        },
        apiTimeoutMs: 600000,
        keyHint: "platform.moonshot.ai / kimi.ai",
      },
    ],
    zhipu: [
      {
        id: "cn",
        endpoint: "https://open.bigmodel.cn/api/anthropic",
        modelsEndpoint: "https://open.bigmodel.cn/api/paas/v4/models",
        knownModels: ["glm-4.7", "glm-4.5-air"],
        primaryModel: "glm-4.7",
        mapping: {
          opus: "glm-4.7",
          sonnet: "glm-4.7",
          haiku: "glm-4.5-air",
          subagent: "glm-4.5-air",
        },
        apiTimeoutMs: 600000,
        keyHint: undefined,
      },
      {
        id: "intl",
        endpoint: "https://api.z.ai/api/anthropic",
        modelsEndpoint: "https://api.z.ai/api/paas/v4/models",
        knownModels: ["glm-4.7", "glm-4.5-air"],
        primaryModel: "glm-4.7",
        mapping: {
          opus: "glm-4.7",
          sonnet: "glm-4.7",
          haiku: "glm-4.5-air",
          subagent: "glm-4.5-air",
        },
        apiTimeoutMs: 3000000,
        keyHint: undefined,
      },
    ],
  };
  for (const [pid, aps] of Object.entries(EXPECT)) {
    it(`${pid} accessPoints 全字段精确`, () => {
      const got = byId(pid).accessPoints;
      expect(got).toHaveLength(aps.length);
      aps.forEach((e, i) => {
        expect(got[i].id).toBe(e.id);
        expect(got[i].endpoint).toBe(e.endpoint);
        expect(got[i].modelsEndpoint).toBe(e.modelsEndpoint);
        expect(got[i].knownModels).toEqual(e.knownModels);
        expect(got[i].primaryModel).toBe(e.primaryModel);
        expect(got[i].primaryModel).toBe(got[i].knownModels[0]); // primaryModel===knownModels[0]
        expect(got[i].mapping).toEqual(e.mapping);
        expect(got[i].apiTimeoutMs).toBe(e.apiTimeoutMs);
        expect(got[i].keyHint).toBe(e.keyHint);
      });
    });
  }

  it("provider-level compat defaults·deepseek 有 thinking_passback", () => {
    expect(byId("deepseek").compatProxy).toBe("thinking_passback");
    expect(byId("deepseek").compatDisableNonessential).toBe(true);
    for (const id of ["kimi", "zhipu"]) {
      expect(byId(id).compatProxy).toBeUndefined();
      expect(byId(id).compatDisableNonessential).toBe(true);
      expect(byId(id).compatDisableBetas).toBe(false);
      expect(byId(id).authMode).toBe("bearer");
    }
  });
});

describe("normalizeEndpoint", () => {
  it("trim/host 小写/去尾斜杠·返回 origin+pathname", () => {
    expect(normalizeEndpoint("  https://API.Moonshot.CN/anthropic/  ")).toBe(
      "https://api.moonshot.cn/anthropic",
    );
  });
  it("非法 URL / 空 → null", () => {
    expect(normalizeEndpoint("not a url")).toBeNull();
    expect(normalizeEndpoint("")).toBeNull();
  });
  it("两区精确可区分·不被 substring 误判", () => {
    expect(normalizeEndpoint("https://api.moonshot.cn/anthropic")).not.toBe(
      normalizeEndpoint("https://api.moonshot.ai/anthropic"),
    );
    expect(
      normalizeEndpoint("https://proxy.example/api.moonshot.cn/anthropic"),
    ).not.toBe(normalizeEndpoint("https://api.moonshot.cn/anthropic"));
  });
});

describe("inferProviderAccessPoint", () => {
  it("endpoint 精确匹配 → (provider, ap)", () => {
    expect(
      inferProviderAccessPoint(
        ag({ endpoint: "https://api.moonshot.cn/anthropic" }),
      ),
    ).toEqual({ providerId: "kimi", accessPointId: "cn" });
    expect(
      inferProviderAccessPoint(
        ag({ endpoint: "https://api.moonshot.ai/anthropic/" }),
      ),
    ).toEqual({ providerId: "kimi", accessPointId: "intl" });
    expect(
      inferProviderAccessPoint(
        ag({ endpoint: "https://open.bigmodel.cn/api/anthropic" }),
      ),
    ).toEqual({ providerId: "zhipu", accessPointId: "cn" });
    expect(
      inferProviderAccessPoint(
        ag({ endpoint: "https://api.z.ai/api/anthropic" }),
      ),
    ).toEqual({ providerId: "zhipu", accessPointId: "intl" });
    expect(
      inferProviderAccessPoint(
        ag({ endpoint: "https://api.deepseek.com/anthropic" }),
      ),
    ).toEqual({ providerId: "deepseek", accessPointId: "default" });
    expect(
      inferProviderAccessPoint(ag({ endpoint: "https://api.deepseek.com/v1" })),
    ).toEqual({ providerId: "harness-deepseek", accessPointId: "default" });
    expect(
      inferProviderAccessPoint(
        ag({ endpoint: "https://open.bigmodel.cn/api/paas/v4" }),
      ),
    ).toEqual({ providerId: "harness-glm", accessPointId: "cn" });
    expect(
      inferProviderAccessPoint(ag({ endpoint: "https://api.moonshot.cn/v1" })),
    ).toEqual({ providerId: "harness-kimi", accessPointId: "cn" });
  });
  it("endpoint 非空但非标准 → custom（即使 provider=kimi）", () => {
    expect(
      inferProviderAccessPoint(
        ag({ endpoint: "https://my.proxy/anthropic", provider: "kimi" }),
      ),
    ).toEqual({ providerId: "custom", accessPointId: null });
  });
  it("harness agent 手改 endpoint 后按 provider 反推 myagent 预设", () => {
    expect(
      inferProviderAccessPoint(
        ag({
          endpoint: "https://my-harness-proxy.example/v1",
          provider: "glm",
          access: "harness",
        }),
      ),
    ).toEqual({ providerId: "harness-glm", accessPointId: null });
    expect(
      inferProviderAccessPoint(
        ag({
          endpoint: "https://my-deepseek-harness.example/v1",
          provider: "deepseek",
          access: "harness",
        }),
      ),
    ).toEqual({ providerId: "harness-deepseek", accessPointId: null });
    expect(
      inferProviderAccessPoint(
        ag({
          endpoint: "https://my-kimi-harness.example/v1",
          provider: "kimi",
          access: "harness",
        }),
      ),
    ).toEqual({ providerId: "harness-kimi", accessPointId: null });
  });
  it("endpoint 非空但非标准且非 harness access → custom", () => {
    expect(
      inferProviderAccessPoint(
        ag({
          endpoint: "https://my-harness-proxy.example/v1",
          provider: "glm",
        }),
      ),
    ).toEqual({ providerId: "custom", accessPointId: null });
    expect(
      inferProviderAccessPoint(
        ag({
          endpoint: "https://my-harness-proxy.example/v1",
          provider: "glm",
          access: "borrow",
        }),
      ),
    ).toEqual({ providerId: "custom", accessPointId: null });
  });
  it("坏 URL → custom", () => {
    expect(
      inferProviderAccessPoint(ag({ endpoint: "garbage", provider: "kimi" })),
    ).toEqual({ providerId: "custom", accessPointId: null });
  });
  it("endpoint 空 → provider 兜底（旧 z.ai/bigmodel→zhipu/cn）", () => {
    expect(
      inferProviderAccessPoint(ag({ endpoint: null, provider: "z.ai" })),
    ).toEqual({ providerId: "zhipu", accessPointId: "cn" });
    expect(
      inferProviderAccessPoint(ag({ endpoint: "", provider: "bigmodel" })),
    ).toEqual({ providerId: "zhipu", accessPointId: "cn" });
    expect(
      inferProviderAccessPoint(ag({ endpoint: null, provider: "kimi" })),
    ).toEqual({ providerId: "kimi", accessPointId: "cn" });
    expect(
      inferProviderAccessPoint(ag({ endpoint: null, provider: "deepseek" })),
    ).toEqual({ providerId: "deepseek", accessPointId: "default" });
    expect(
      inferProviderAccessPoint(ag({ endpoint: null, provider: "claude" })),
    ).toEqual({ providerId: "claude", accessPointId: null });
    expect(
      inferProviderAccessPoint(ag({ endpoint: null, provider: "anthropic" })),
    ).toEqual({ providerId: "claude", accessPointId: null });
    expect(
      inferProviderAccessPoint(ag({ endpoint: null, provider: "codex" })),
    ).toEqual({ providerId: "codex", accessPointId: null });
  });
});

describe("classifyModelsFetchError", () => {
  it("把 /models 原始错误归类到连接测试 category", () => {
    expect(classifyModelsFetchError("HTTP 401")).toBe("auth");
    expect(classifyModelsFetchError("HTTP 404")).toBe("not_found");
    expect(classifyModelsFetchError("HTTP 429")).toBe("rate_limit");
    expect(classifyModelsFetchError("missing_key")).toBe("missing_key");
    expect(classifyModelsFetchError("reqwest error")).toBe("network");
  });
});

describe("deriveAccess", () => {
  it("borrow 系预设 → borrow", () => {
    expect(deriveAccess("deepseek")).toBe("borrow");
    expect(deriveAccess("custom")).toBe("borrow");
  });
  it("原生 CLI 预设 → native", () => {
    expect(deriveAccess("claude")).toBe("native");
    expect(deriveAccess("codex")).toBe("native");
  });
  it("编辑已存 native → 保留 native", () => {
    expect(deriveAccess("custom", "native")).toBe("native");
  });
  it("编辑已存 borrow → 保留 borrow", () => {
    expect(deriveAccess("deepseek", "borrow")).toBe("borrow");
  });
  it("harness 预设和已存 harness → harness", () => {
    expect(deriveAccess("harness-deepseek")).toBe("harness");
    expect(deriveAccess("deepseek", "harness")).toBe("harness");
  });
});

describe("mergeModelOptions", () => {
  it("去重 + currentValue 保可见 + 自定义哨兵末尾", () => {
    expect(mergeModelOptions(["a", "b"], ["b", "c"], "z")).toEqual([
      "a",
      "b",
      "c",
      "z",
      CUSTOM_MODEL_SENTINEL,
    ]);
  });
  it("静态模型先于缓存模型·哨兵末尾", () => {
    expect(mergeModelOptions(["s"], ["L"], "")).toEqual([
      "s",
      "L",
      CUSTOM_MODEL_SENTINEL,
    ]);
  });
  it("输入含自定义哨兵时只保留末尾一个", () => {
    expect(
      mergeModelOptions(
        ["a", CUSTOM_MODEL_SENTINEL],
        [CUSTOM_MODEL_SENTINEL],
        CUSTOM_MODEL_SENTINEL,
      ),
    ).toEqual(["a", CUSTOM_MODEL_SENTINEL]);
  });
  it("currentValue 已在列表不重复·空值不加", () => {
    expect(mergeModelOptions(["a"], [], "a")).toEqual([
      "a",
      CUSTOM_MODEL_SENTINEL,
    ]);
    expect(mergeModelOptions(["a"], [], "")).toEqual([
      "a",
      CUSTOM_MODEL_SENTINEL,
    ]);
  });
});

describe("model cache", () => {
  beforeEach(() => localStorage.clear());
  it("空 store → null", () => {
    expect(readModelCache("deepseek", "https://x")).toBeNull();
  });
  it("write/read round-trip（endpoint 匹配 + 未过期）", () => {
    writeModelCache("deepseek", "https://x/anthropic", ["m1", "m2"]);
    expect(readModelCache("deepseek", "https://x/anthropic")).toEqual([
      "m1",
      "m2",
    ]);
  });
  it("endpoint 不匹配 → null", () => {
    writeModelCache("deepseek", "https://x/anthropic", ["m1"]);
    expect(readModelCache("deepseek", "https://y/anthropic")).toBeNull();
  });
  it("尾斜杠/大小写归一后命中同一 cache 且 payload endpoint 归一", () => {
    writeModelCache("kimi", "https://API.moonshot.CN/anthropic/", [
      "kimi-k2.5",
      "cached-x",
    ]);
    expect(readModelCache("kimi", "https://api.moonshot.cn/anthropic")).toEqual(
      ["kimi-k2.5", "cached-x"],
    );
    const raw = localStorage.getItem(
      "agentloom:models:kimi:https://api.moonshot.cn/anthropic",
    );
    expect(JSON.parse(raw!).endpoint).toBe("https://api.moonshot.cn/anthropic");
  });
  it("TTL 过期 → null", () => {
    writeModelCache("deepseek", "https://x/anthropic", ["m1"]);
    const key = "agentloom:models:deepseek:https://x/anthropic";
    const v = JSON.parse(localStorage.getItem(key)!);
    v.fetchedAt = Date.now() - 25 * 60 * 60 * 1000; // 25h 前
    localStorage.setItem(key, JSON.stringify(v));
    expect(readModelCache("deepseek", "https://x/anthropic")).toBeNull();
  });
  it("payload endpoint 被篡改 → null", () => {
    writeModelCache("deepseek", "https://x", ["m1"]);
    localStorage.setItem(
      "agentloom:models:deepseek:https://x",
      JSON.stringify({
        models: ["m1"],
        endpoint: "https://y",
        fetchedAt: Date.now(),
      }),
    );
    expect(readModelCache("deepseek", "https://x")).toBeNull();
  });
  it("models 含非 string → 过滤", () => {
    localStorage.setItem(
      "agentloom:models:deepseek:https://x",
      JSON.stringify({
        models: [1, "ok", null],
        endpoint: "https://x",
        fetchedAt: Date.now(),
      }),
    );
    expect(readModelCache("deepseek", "https://x")).toEqual(["ok"]);
  });
  it("fetchedAt 非 number → null", () => {
    localStorage.setItem(
      "agentloom:models:deepseek:https://x",
      JSON.stringify({
        models: ["m"],
        endpoint: "https://x",
        fetchedAt: "x",
      }),
    );
    expect(readModelCache("deepseek", "https://x")).toBeNull();
  });
  it("坏 JSON / shape 错 → null（不抛）", () => {
    localStorage.setItem(
      "agentloom:models:deepseek:https://x/anthropic",
      "{not json",
    );
    expect(readModelCache("deepseek", "https://x/anthropic")).toBeNull();
    localStorage.setItem(
      "agentloom:models:deepseek:https://x/anthropic",
      JSON.stringify({ models: "nope" }),
    );
    expect(readModelCache("deepseek", "https://x/anthropic")).toBeNull();
  });
  it("writeModelCache setItem 抛异常时静默", () => {
    const setItem = vi
      .spyOn(globalThis.localStorage, "setItem")
      .mockImplementation(() => {
        throw new Error("quota");
      });
    try {
      expect(() => writeModelCache("d", "e", ["m"])).not.toThrow();
    } finally {
      setItem.mockRestore();
    }
  });
  it("常量保持精确值", () => {
    expect(AUTH_MODE).toEqual({ bearer: "bearer", xApiKey: "x_api_key" });
  });
});

describe("resolveModelsEndpoint", () => {
  it("核心锁：AP 显式 modelsEndpoint 压过拼接惯例（endpoint 与 AP 匹配时）", () => {
    const ap = {
      endpoint: "https://x.test/v1",
      modelsEndpoint: "https://x.test/other/models",
    };
    expect(resolveModelsEndpoint("https://x.test/v1", ap)).toBe(
      "https://x.test/other/models",
    );
  });

  it("endpoint 与 AP 不匹配 → 走拼接", () => {
    const ap = {
      endpoint: "https://x.test/v1",
      modelsEndpoint: "https://x.test/other/models",
    };
    expect(resolveModelsEndpoint("https://other.test/v1", ap)).toBe(
      "https://other.test/v1/models",
    );
  });

  it("无 AP（custom harness）→ 走拼接", () => {
    expect(resolveModelsEndpoint("https://custom.test/v1")).toBe(
      "https://custom.test/v1/models",
    );
  });

  it("AP 有 endpoint 但没配 modelsEndpoint → 走拼接", () => {
    const ap = { endpoint: "https://x.test/v1" };
    expect(resolveModelsEndpoint("https://x.test/v1", ap)).toBe(
      "https://x.test/v1/models",
    );
  });

  it("endpoint 带尾斜杠 → 拼接时不产生双斜杠", () => {
    expect(resolveModelsEndpoint("https://x.test/v1/")).toBe(
      "https://x.test/v1/models",
    );
  });

  it("尾斜杠/大小写 host 差异不影响与 AP 的判等", () => {
    const ap = {
      endpoint: "https://x.test/v1",
      modelsEndpoint: "https://x.test/other/models",
    };
    expect(resolveModelsEndpoint("https://X.TEST/v1/", ap)).toBe(
      "https://x.test/other/models",
    );
  });

  it("空 endpoint → normalizeEndpoint 返回 null，判等必不成立，走拼接", () => {
    expect(resolveModelsEndpoint("")).toBe("/models");
  });

  it("数据不变量：每个 harness 接入点都显式配置了 modelsEndpoint", () => {
    // 这条锁的是【数据】不是【实现】：resolveModelsEndpoint 的实现差异由上面的
    // 「核心锁」那条负责（它用刻意不等价的假接入点，退回朴素拼接实现时会转红）。
    // 本条防的是另一类回归：某个 harness 接入点漏配 modelsEndpoint 时，
    // resolveModelsEndpoint 会静默回退到 endpoint + "/models" 拼接惯例——
    // 当前 7 个接入点恰好都满足该惯例，所以漏配不会立刻出错，但一旦某个 provider
    // 的 models 路径不规则，就会静默拉错 URL 且无人察觉。
    const harnessAccessPoints = PROVIDER_PRESETS.filter(
      (preset) => preset.access === "harness",
    ).flatMap((preset) =>
      preset.accessPoints.map((ap) => ({ presetId: preset.id, ap })),
    );

    // 防 vacuous truth：harness preset 被改名/删光时，上面的 flatMap 会得到空数组，
    // 下面的循环一次都不跑、测试静默通过。
    expect(harnessAccessPoints.length).toBeGreaterThan(0);

    for (const { presetId, ap } of harnessAccessPoints) {
      expect(
        ap.modelsEndpoint,
        `harness 接入点 ${presetId}/${ap.id} 漏配 modelsEndpoint`,
      ).toBeTruthy();
    }
  });
});

describe("engineView", () => {
  it("按引擎暴露 account/api_key 分组且顺序稳定", () => {
    const view = engineView();

    expect(view.map((entry) => entry.engine)).toEqual([
      "claude-code",
      "codex",
      "myagent",
    ]);
    expect(view[0]).toMatchObject({
      label: "Claude Code CLI",
      desc: "本机 claude 命令。可跑 Anthropic 自家，也可借壳跑别家",
    });
    expect(view[0].groups.map((group) => group.kind)).toEqual([
      "account",
      "api_key",
    ]);
    expect(view[0].groups[0].presets).toEqual([byId("claude")]);
    expect(view[0].groups[1].presets.map((preset) => preset.id)).toEqual([
      "deepseek",
      "kimi",
      "zhipu",
      "custom",
    ]);

    expect(view[1]).toMatchObject({
      label: "Codex CLI",
      desc: "本机 codex 命令。跑 OpenAI 自家模型",
    });
    expect(view[1].groups).toEqual([
      { kind: "account", presets: [byId("codex")] },
      { kind: "api_key", presets: [] },
    ]);

    expect(view[2]).toMatchObject({
      label: "myagent",
      desc: "自研 harness，直连各家 API",
    });
    expect(view[2].groups).toEqual([
      {
        kind: "api_key",
        presets: [
          byId("harness-deepseek"),
          byId("harness-glm"),
          byId("harness-kimi"),
        ],
      },
    ]);
  });

  it("分组内 preset 复用 PROVIDER_PRESETS 中的对象引用", () => {
    const presetRefs = engineView().flatMap((entry) =>
      entry.groups.flatMap((group) => group.presets),
    );

    for (const preset of presetRefs) {
      expect(preset).toBe(byId(preset.id));
    }
  });
});

describe("autoAgentName", () => {
  it("native 直接返回 preset label", () => {
    expect(autoAgentName("claude")).toBe("Claude CLI");
    expect(autoAgentName("codex")).toBe("Codex CLI");
  });

  it("harness 追加 myagent 后缀", () => {
    expect(autoAgentName("harness-deepseek")).toBe("DeepSeek（myagent）");
  });

  it("borrow 多接入点按 accessPoint label 生成名称", () => {
    expect(autoAgentName("zhipu", "intl")).toBe(
      "智谱 GLM 国际（Claude Code 借壳）",
    );
    expect(autoAgentName("zhipu", "cn")).toBe(
      "智谱 GLM 中国（Claude Code 借壳）",
    );
  });

  it("borrow 单接入点或未知接入点不带接入点名", () => {
    expect(autoAgentName("deepseek")).toBe("DeepSeek（Claude Code 借壳）");
    expect(autoAgentName("zhipu", "missing")).toBe(
      "智谱 GLM（Claude Code 借壳）",
    );
  });
});

describe("deriveModelMapping", () => {
  it("主线最大版本作为旗舰，同版本轻量模型作为 haiku", () => {
    expect(
      deriveModelMapping(["glm-4.7", "glm-5", "glm-5-air", "glm-4.5-air"]),
    ).toEqual({
      primary: "glm-5",
      opus: "glm-5",
      sonnet: "glm-5",
      haiku: "glm-5-air",
      subagent: "glm-5",
    });
  });

  it("按数值版本比较 kimi k 系列，缺少 light 时 haiku 等于旗舰", () => {
    expect(deriveModelMapping(["kimi-k2.5", "kimi-k2"])).toEqual({
      primary: "kimi-k2.5",
      opus: "kimi-k2.5",
      sonnet: "kimi-k2.5",
      haiku: "kimi-k2.5",
      subagent: "kimi-k2.5",
    });
  });

  it("preview 不参与主线旗舰竞选", () => {
    expect(deriveModelMapping(["glm-5", "glm-5-preview"])).toEqual({
      primary: "glm-5",
      opus: "glm-5",
      sonnet: "glm-5",
      haiku: "glm-5",
      subagent: "glm-5",
    });
  });

  it("无法解析版本或空数组返回 null", () => {
    expect(
      deriveModelMapping(["deepseek-chat", "deepseek-reasoner"]),
    ).toBeNull();
    expect(deriveModelMapping([])).toBeNull();
  });

  it("版本比较使用数值而不是字符串", () => {
    expect(deriveModelMapping(["foo-4.7", "foo-5"])).toMatchObject({
      primary: "foo-5",
      opus: "foo-5",
      sonnet: "foo-5",
      haiku: "foo-5",
      subagent: "foo-5",
    });
  });
});

// @ts-expect-error - Vitest runs in Node, but this frontend tsconfig has no Node type declarations.
import { readFileSync } from "fs";
import { describe, expect, it } from "vitest";
import type { Locale, TranslationKey } from "../i18n";
import {
  classifyLeadError,
  parseBackendError,
  renderBackendError,
} from "./backendMsg";

const templates: Partial<Record<TranslationKey, string>> = {
  "backend.landing.noEvidence":
    "落地前检查未通过：找不到 worker changed_files 证据",
  "backend.landing.protectedPath": "落地前检查未通过：受保护路径 {paths}",
  "backend.team.oneshotFailed": "run_oneshot_llm 失败：{detail}",
};

const t = (
  key: TranslationKey,
  values?: Record<string, string | number>,
): string => {
  let template = templates[key] ?? key;
  for (const [name, value] of Object.entries(values ?? {})) {
    template = template.split(`{${name}}`).join(String(value));
  }
  return template;
};

function loadI18nMessages(): Record<Locale, Record<string, string>> {
  const source = readFileSync("src/i18n.tsx", "utf-8");
  const match = source.match(/const messages = (\{[\s\S]*?\n\} as const)/);
  if (!match) throw new Error("Could not locate the i18n message tables");
  const literalText = match[1].replace(/\s+as const$/, "");
  return new Function(`"use strict"; return (${literalText});`)() as Record<
    Locale,
    Record<string, string>
  >;
}

const i18nMessages = loadI18nMessages();

const localizedT = (locale: Locale) =>
  ((key, values) => {
    let template = i18nMessages[locale][key] ?? key;
    for (const [name, value] of Object.entries(values ?? {})) {
      template = template.split(`{${name}}`).join(String(value));
    }
    return template;
  }) satisfies typeof t;

describe("parseBackendError", () => {
  it("parses a valid parameterless envelope", () => {
    expect(parseBackendError("AL_ERR:landing.noEvidence")).toEqual({
      code: "landing.noEvidence",
      params: {},
    });
  });

  it("parses a valid envelope with string params", () => {
    expect(
      parseBackendError(
        'AL_ERR:landing.protectedPath:{"paths":"docs/a.md, docs/b.md"}',
      ),
    ).toEqual({
      code: "landing.protectedPath",
      params: { paths: "docs/a.md, docs/b.md" },
    });
  });

  it.each(["AL_ERR:has space:{}", "AL_ERR::{}", "AL_ERR:code:with:colon:{}"])(
    "returns null for an invalid code envelope: %s",
    (raw) => {
      expect(parseBackendError(raw)).toBeNull();
    },
  );

  it.each([
    'AL_ERR:x.y:{"k":1}',
    'AL_ERR:x.y:{"k":null}',
    'AL_ERR:x.y:{"k":{"nested":true}}',
  ])("returns null for a non-string param value: %s", (raw) => {
    expect(parseBackendError(raw)).toBeNull();
  });

  it("round-trips escaped Chinese, newline, and quotes", () => {
    expect(
      parseBackendError(
        'AL_ERR:landing.protectedPath:{"paths":"含中文\\n\\"quoted\\""}',
      ),
    ).toEqual({
      code: "landing.protectedPath",
      params: { paths: '含中文\n"quoted"' },
    });
  });

  it("returns null for malformed JSON without throwing", () => {
    expect(
      parseBackendError("AL_ERR:landing.protectedPath:{bad-json"),
    ).toBeNull();
  });
});

describe("renderBackendError", () => {
  it("renders local-session continuation errors without leaking backend details", () => {
    const raw = "LOCAL_SESSION_UNSUPPORTED:abc";
    const rendered = renderBackendError(raw, localizedT("zh"));

    expect(rendered).toBe("本地会话暂不支持接续（此功能对本地会话尚未开放）");
    expect(rendered).not.toContain("LOCAL_SESSION_UNSUPPORTED");
    expect(rendered).not.toContain("abc");
  });

  it("passes a non-envelope string through unchanged", () => {
    expect(renderBackendError("raw backend error", t)).toBe(
      "raw backend error",
    );
  });

  it("passes an unknown code envelope through unchanged", () => {
    const raw = "AL_ERR:landing.future";
    expect(renderBackendError(raw, t)).toBe(raw);
  });

  it("renders a known code with parameter interpolation", () => {
    expect(
      renderBackendError(
        'AL_ERR:landing.protectedPath:{"paths":"docs/a.md"}',
        t,
      ),
    ).toBe("落地前检查未通过：受保护路径 docs/a.md");
  });

  it("renders a team envelope with its detail parameter", () => {
    expect(
      renderBackendError(
        'AL_ERR:team.oneshotFailed:{"detail":"模型密钥无效"}',
        t,
      ),
    ).toBe("run_oneshot_llm 失败：模型密钥无效");
  });

  it("renders file.basenameBudget with the localized bare filename", () => {
    expect(
      renderBackendError(
        'AL_ERR:file.basenameBudget:{"0":"x.md"}',
        localizedT("zh"),
      ),
    ).toBe("同名文件太多，搜索范围超限，请提供更完整的路径（x.md）");
  });

  it.each([
    [
      "zh" as const,
      "队员仍在执行上一轮派单",
      "无法开始新运行：队员仍在执行上一轮派单",
    ],
    [
      "en" as const,
      "Team members are still executing assignments from the previous run",
      "Cannot start a new run: Team members are still executing assignments from the previous run",
    ],
  ])(
    "renders run.teamMembersActive as localized text in %s",
    (locale, detail, expected) => {
      expect(
        renderBackendError(
          `AL_ERR:run.teamMembersActive:${JSON.stringify({ detail })}`,
          localizedT(locale),
        ),
      ).toBe(expected);
    },
  );

  it("renders keychain save failures with and without detail as actionable text", () => {
    const withDetail = renderBackendError(
      'AL_ERR:agent.keychainSaveFailed:{"detail":"access denied"}',
      localizedT("zh"),
    );
    const withoutDetail = renderBackendError(
      "AL_ERR:agent.keychainSaveFailed",
      localizedT("zh"),
    );

    expect(withDetail).not.toContain("AL_ERR:");
    expect(withDetail).toContain("系统钥匙串");
    expect(withDetail).toContain("未生效");
    expect(withDetail).toContain("access denied");
    expect(withoutDetail).not.toContain("AL_ERR:");
    expect(withoutDetail).toContain("系统钥匙串");
    expect(withoutDetail).toContain("请重试");
  });

  it("renders the localized keychain-unavailable detail without duplicating it", () => {
    const detail =
      "无法从系统钥匙串读取 API key。请打开 Settings，重新保存该 agent 的 API key。";
    const rendered = renderBackendError(
      `AL_ERR:agent.keychainKeyUnavailable:${JSON.stringify({ detail })}`,
      localizedT("zh"),
    );

    expect(rendered).not.toContain("AL_ERR:");
    expect(rendered).toContain("无法从系统钥匙串读取 API key");
    expect(rendered).toContain("重新保存");
    expect(rendered.split(detail)).toHaveLength(2);
  });

  it("defines both keychain error translations in zh and en", () => {
    for (const locale of ["zh", "en"] as const) {
      expect(
        i18nMessages[locale]["backend.agent.keychainSaveFailed"],
      ).toBeTypeOf("string");
      expect(
        i18nMessages[locale]["backend.agent.keychainKeyUnavailable"],
      ).toBeTypeOf("string");
    }
  });

  it("defines the local-session continuation error translation in zh and en", () => {
    for (const locale of ["zh", "en"] as const) {
      expect(
        i18nMessages[locale]["backend.continuation.localSessionUnsupported"],
      ).toBeTypeOf("string");
    }
  });
});

describe("classifyLeadError", () => {
  it.each([
    "lead.spawnDriverFailed",
    "lead.spawnLeadFailed",
    "lead.noFinalText",
    "lead.noFinalTextStderr",
    "lead.parseSpawnFailed",
    "lead.parseNoOutput",
    "lead.draftNoFinalText",
    "lead.draftNoFinalTextStderr",
  ])("classifies transient envelope code %s", (code) => {
    expect(classifyLeadError(`AL_ERR:${code}`)).toBe("transient");
  });

  it.each([
    "team.oneshotSpawnFailed",
    "team.oneshotFailed",
    "team.oneshotNoText",
    "team.summarizeSpawnFailed",
    "team.summarizeFailed",
    "team.summarizeNoText",
    "team.noMemberOutput",
    "lead.parseFailed",
  ])("keeps excluded envelope code %s generic", (code) => {
    expect(classifyLeadError(`AL_ERR:${code}`)).toBe("generic");
  });

  it.each([
    "lead.claudeOnlyBlock1",
    "lead.claudeOnlyStep",
    "lead.claudeOnlyDraft",
  ])("classifies claude-only envelope code %s", (code) => {
    expect(classifyLeadError(`AL_ERR:${code}`)).toBe("claudeOnly");
  });

  it.each([
    ["spawn 失败：No such file or directory", "transient"],
    ["lead 无终态 final_text", "transient"],
    ['lead 输出无法解析：NoOutput("无输出")', "transient"],
    [
      "块① 仅支持 native claude 队长（当前 provider=openai access=native）",
      "claudeOnly",
    ],
  ] as const)("preserves legacy classification for %s", (msg, expected) => {
    expect(classifyLeadError(msg)).toBe(expected);
  });

  it("keeps an unrelated English error generic", () => {
    expect(classifyLeadError("request failed with status 500")).toBe("generic");
  });
});

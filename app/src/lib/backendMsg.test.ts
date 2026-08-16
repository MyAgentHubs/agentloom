// @ts-expect-error - Vitest runs in Node, but this frontend tsconfig has no Node type declarations.
import { readFileSync, readdirSync } from "fs";
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
  const source = readFileSync("src/i18nMessages.ts", "utf-8");
  const match = source.match(
    /export const messages = (\{[\s\S]*?\n\} as const)/,
  );
  if (!match) throw new Error("Could not locate the i18n message tables");
  const literalText = match[1].replace(/\s+as const$/, "");
  return new Function(`"use strict"; return (${literalText});`)() as Record<
    Locale,
    Record<string, string>
  >;
}

const i18nMessages = loadI18nMessages();

// Historical debt only. New codes must never be added to this allowlist.
const HISTORICAL_MISSING_BACKEND_MESSAGE_CODES = new Set([
  "project.createDirectoryFailed",
  "project.databaseUnavailable",
  "project.emptyName",
  "project.homeNotFound",
  "project.invalidPath",
  "project.pathRequired",
  "project.renameFailed",
  "run.globallyStopped",
  "run.projectPathUnavailable",
  "wt.cleanup.uncommittedMemberChanges",
  "wt.cleanup.uncommittedSessionChanges",
  "wt.reconcile.baseRepoMissing",
  "wt.reconcile.expectedPathCanonicalizeFailed",
  "wt.reconcile.gitStatusFailed",
  "wt.reconcile.gitStatusSpawnFailed",
  "wt.reconcile.invalidSessionDir",
  "wt.reconcile.notLinkedWorktree",
  "wt.reconcile.unexpectedCommonDir",
  "wt.reconcile.unexpectedHead",
  "wt.reconcile.unexpectedPath",
  "wt.reconcile.worktreeCanonicalizeFailed",
  "wt.write.outsideAppDomain",
]);

// These codes are being added in the parallel backend task. Keeping them here
// makes this guard effective before and after that backend commit is combined.
const CONCURRENT_BACKEND_MESSAGE_CODES = [
  "cliPath.invalidCli",
  "cliPath.invalidPath",
  "cliPath.databaseUnavailable",
] as const;

const AL_ERR_LITERAL_PATTERN = /\bal_err\s*\(\s*"([^"]+)"/g;
const AL_ERR_CALL_PATTERN = /\bal_err\s*\(/g;
const TAURI_SOURCE_ROOT = "src-tauri/src";

const KNOWN_DYNAMIC_AL_ERR_CALLS = [
  {
    relativePath: "lead_step.rs",
    lineIncludes:
      'crate::ui_msg::al_err(code, &[("detail", format!("{err:?}"))])',
    // lead_parse_error_envelope can select any of these codes.
    possibleCodes: [
      "lead.parseSpawnFailed",
      "lead.parseNoOutput",
      "lead.parseFailed",
    ],
  },
  {
    relativePath: "lib.rs",
    lineIncludes: 'ui_msg::al_err(code, &[("detail", detail)])',
    // first_event_watchdog_error receives run.spawnFailed from its caller.
    possibleCodes: ["run.spawnFailed"],
  },
] as const;

type RustSourceFile = {
  path: string;
  relativePath: string;
};

type DirectoryEntry = {
  name: string;
  isDirectory: () => boolean;
  isFile: () => boolean;
};

type BackendErrorCodeAudit = {
  literalCodes: Set<string>;
  codesToCheck: Set<string>;
  unknownDynamicLocations: string[];
};

function findRustSourceFiles(
  directory: string,
  relativeDirectory = "",
): RustSourceFile[] {
  const entries = readdirSync(directory, {
    withFileTypes: true,
  }) as DirectoryEntry[];
  return entries.flatMap((entry) => {
    const relativePath = `${relativeDirectory}${entry.name}`;
    if (entry.isDirectory()) {
      return findRustSourceFiles(
        `${directory}/${entry.name}`,
        `${relativePath}/`,
      );
    }
    return entry.isFile() && entry.name.endsWith(".rs")
      ? [{ path: `${directory}/${entry.name}`, relativePath }]
      : [];
  });
}

function auditBackendErrorSources(
  sources: Readonly<Record<string, string>>,
): BackendErrorCodeAudit {
  const literalCodes = new Set<string>();
  const unknownDynamicLocations: string[] = [];

  for (const [relativePath, source] of Object.entries(sources)) {
    for (const match of source.matchAll(AL_ERR_LITERAL_PATTERN)) {
      literalCodes.add(match[1]);
    }

    for (const match of source.matchAll(AL_ERR_CALL_PATTERN)) {
      let firstArgument = (match.index ?? 0) + match[0].length;
      while (/\s/.test(source[firstArgument] ?? "")) firstArgument += 1;
      if (source[firstArgument] === '"') continue;

      const lineStart = source.lastIndexOf("\n", match.index) + 1;
      if (/\bfn\s*$/.test(source.slice(lineStart, match.index))) continue;

      const lineEnd = source.indexOf("\n", match.index);
      const sourceLine = source.slice(
        lineStart,
        lineEnd === -1 ? source.length : lineEnd,
      );
      const isKnownDynamicCall = KNOWN_DYNAMIC_AL_ERR_CALLS.some(
        (knownCall) =>
          knownCall.relativePath === relativePath &&
          sourceLine.includes(knownCall.lineIncludes),
      );
      if (isKnownDynamicCall) continue;

      const line = source.slice(0, match.index).split("\n").length;
      unknownDynamicLocations.push(`${relativePath}:${line}`);
    }
  }

  return {
    literalCodes,
    codesToCheck: new Set([
      ...literalCodes,
      ...CONCURRENT_BACKEND_MESSAGE_CODES,
      ...KNOWN_DYNAMIC_AL_ERR_CALLS.flatMap((call) => call.possibleCodes),
    ]),
    unknownDynamicLocations,
  };
}

function scanBackendErrorCodes(): BackendErrorCodeAudit {
  return auditBackendErrorSources(
    Object.fromEntries(
      findRustSourceFiles(TAURI_SOURCE_ROOT).map((file) => [
        file.relativePath,
        readFileSync(file.path, "utf-8"),
      ]),
    ),
  );
}

function assertNoUnknownDynamicAlErrCalls(
  unknownDynamicLocations: readonly string[],
): void {
  if (unknownDynamicLocations.length === 0) return;
  throw new Error(
    `Unknown non-literal al_err call sites:\n${unknownDynamicLocations.join("\n")}\n` +
      "New dynamic al_err calls must be added to KNOWN_DYNAMIC_AL_ERR_CALLS, but only after confirming that every code they can produce has both zh and en translations.",
  );
}

const localizedT = (locale: Locale) =>
  ((key, values) => {
    let template = i18nMessages[locale][key] ?? key;
    for (const [name, value] of Object.entries(values ?? {})) {
      template = template.split(`{${name}}`).join(String(value));
    }
    return template;
  }) satisfies typeof t;

describe("backend error translation coverage", () => {
  it("defines every literal Rust al_err code in both locales", () => {
    const { literalCodes, codesToCheck, unknownDynamicLocations } =
      scanBackendErrorCodes();

    if (literalCodes.size === 0) {
      throw new Error(
        "No literal al_err codes were found; the scanner must fail closed.",
      );
    }
    assertNoUnknownDynamicAlErrCalls(unknownDynamicLocations);

    const missing: string[] = [];
    for (const code of [...codesToCheck].sort()) {
      if (HISTORICAL_MISSING_BACKEND_MESSAGE_CODES.has(code)) continue;
      for (const locale of ["zh", "en"] as const) {
        const key = `backend.${code}`;
        if (typeof i18nMessages[locale][key] !== "string") {
          missing.push(`${locale}: ${key}`);
        }
      }
    }

    if (missing.length > 0) {
      throw new Error(`Missing backend translations:\n${missing.join("\n")}`);
    }
  });

  it("fails when a non-literal al_err call is not allowlisted", () => {
    const { unknownDynamicLocations } = auditBackendErrorSources({
      "new_runtime.rs": "fn report(code: &str) {\n    al_err(code, &[]);\n}",
    });

    expect(() =>
      assertNoUnknownDynamicAlErrCalls(unknownDynamicLocations),
    ).toThrow(/Unknown non-literal al_err call sites:\nnew_runtime\.rs:2/);
  });

  it("accepts the two known dynamic al_err call sites", () => {
    const { codesToCheck, unknownDynamicLocations } = auditBackendErrorSources({
      "lead_step.rs":
        '    crate::ui_msg::al_err(code, &[("detail", format!("{err:?}"))])',
      "lib.rs": '    ui_msg::al_err(code, &[("detail", detail)])',
    });

    expect(unknownDynamicLocations).toEqual([]);
    expect([...codesToCheck]).toEqual(
      expect.arrayContaining([
        "lead.parseSpawnFailed",
        "lead.parseNoOutput",
        "lead.parseFailed",
        "run.spawnFailed",
      ]),
    );
  });
});

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

import type { HighlighterCore, ThemeRegistration } from "shiki/core";

export const SUPPORTED_LANGS = [
  "typescript",
  "tsx",
  "javascript",
  "jsx",
  "json",
  "bash",
  "rust",
  "markdown",
] as const;

export const CODE_THEME = "agentloom-warm-dark";

const WARM_CODE_THEME = {
  name: CODE_THEME,
  type: "light",
  colors: {
    "editor.background": "#efe7d6",
    "editor.foreground": "#352d25",
  },
  settings: [
    { settings: { background: "#efe7d6", foreground: "#352d25" } },
    {
      scope: ["comment", "punctuation.definition.comment"],
      settings: { fontStyle: "italic", foreground: "#8a7f6a" },
    },
    {
      scope: ["keyword", "keyword.control", "storage.modifier", "storage.type"],
      settings: { foreground: "#a64b2a" },
    },
    {
      scope: ["entity.name.function", "support.function", "variable.function"],
      settings: { foreground: "#275f99" },
    },
    {
      scope: ["string", "constant.other.symbol"],
      settings: { foreground: "#4d6e30" },
    },
    {
      scope: ["constant.language", "constant.numeric", "support.constant"],
      settings: { foreground: "#8f5e1f" },
    },
    {
      scope: ["entity.name.class", "entity.name.type", "support.type"],
      settings: { foreground: "#856016" },
    },
    {
      scope: ["operator", "punctuation"],
      settings: { foreground: "#5e574a" },
    },
  ],
} satisfies ThemeRegistration;

let highlighterPromise: Promise<HighlighterCore> | null = null;

export function getHighlighter(): Promise<HighlighterCore> {
  if (!highlighterPromise) {
    highlighterPromise = (async () => {
      const [{ createHighlighterCore }, { createRegexEngine }] =
        await Promise.all([import("shiki/core"), import("./shikiEngine")]);

      return createHighlighterCore({
        themes: [WARM_CODE_THEME],
        langs: [
          import("shiki/langs/typescript.mjs"),
          import("shiki/langs/tsx.mjs"),
          import("shiki/langs/javascript.mjs"),
          import("shiki/langs/jsx.mjs"),
          import("shiki/langs/json.mjs"),
          import("shiki/langs/bash.mjs"),
          import("shiki/langs/rust.mjs"),
          import("shiki/langs/markdown.mjs"),
        ],
        engine: createRegexEngine(),
      });
    })().catch((error) => {
      // 单例失败后不永久缓存 rejected promise：清空引用，下次调用可重新尝试
      // （构建失败可能是瞬时资源加载问题，不应让该会话永久失去高亮能力）。
      highlighterPromise = null;
      throw error;
    });
  }
  return highlighterPromise;
}

export function normalizeLang(lang?: string): string {
  if (!lang) return "text";
  const alias: Record<string, string> = {
    js: "javascript",
    md: "markdown",
    rs: "rust",
    sh: "bash",
    shell: "bash",
    ts: "typescript",
  };
  const resolved = alias[lang.toLowerCase()] ?? lang.toLowerCase();
  return (SUPPORTED_LANGS as readonly string[]).includes(resolved)
    ? resolved
    : "text";
}

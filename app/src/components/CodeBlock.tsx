import { invoke } from "@tauri-apps/api/core";
import { openPath } from "@tauri-apps/plugin-opener";
import { memo, useEffect, useRef, useState } from "react";
import { CODE_THEME, getHighlighter, normalizeLang } from "../lib/highlighter";
import { useI18n } from "../i18n";

type Props = { code: string; lang?: string };
type Token = { content: string; color?: string };
type Line = Token[];

const FOLD_LINES = 30;
const tokenCache = new Map<string, Line[]>();

function plainLines(code: string): Line[] {
  return code.split("\n").map((line) => [{ content: line }]);
}

function CodeBlockImpl({ code, lang }: Props) {
  const { t } = useI18n();
  const displayLang = normalizeLang(lang);
  const isHtml =
    lang?.trim().toLowerCase() === "html" ||
    /^\s*(<!doctype html|<html[\s>])/i.test(code);
  const cacheKey = `${CODE_THEME}:${displayLang}:${code}`;
  const [tokenLines, setTokenLines] = useState<Line[] | null>(
    () => tokenCache.get(cacheKey) ?? null,
  );
  const [expanded, setExpanded] = useState(false);
  const [copied, setCopied] = useState(false);
  const aliveRef = useRef(true);

  useEffect(() => {
    aliveRef.current = true;
    const cached = tokenCache.get(cacheKey);
    if (cached) {
      setTokenLines(cached);
      return () => {
        aliveRef.current = false;
      };
    }

    setTokenLines(null);
    if (displayLang === "text") {
      const lines = plainLines(code);
      tokenCache.set(cacheKey, lines);
      setTokenLines(lines);
      return () => {
        aliveRef.current = false;
      };
    }

    (async () => {
      try {
        const highlighter = await getHighlighter();
        const loaded = highlighter.getLoadedLanguages();
        if (!loaded.includes(displayLang)) {
          const lines = plainLines(code);
          tokenCache.set(cacheKey, lines);
          if (aliveRef.current) setTokenLines(lines);
          return;
        }

        const result = highlighter.codeToTokens(code, {
          lang: displayLang,
          theme: CODE_THEME,
        });
        const lines = result.tokens.map((line) =>
          line.map((token) => ({
            color: token.color,
            content: token.content,
          })),
        );
        tokenCache.set(cacheKey, lines);
        if (aliveRef.current) setTokenLines(lines);
      } catch (error) {
        // 高亮引擎构建/分词失败时静默退化为纯文本，不让代码块整体不可用
        // （这是设计行为：宁可丢高亮也不丢内容）。不缓存失败结果，
        // 允许下次渲染时随单例重置重试。
        console.warn("代码高亮失败，退化为纯文本", error);
        if (aliveRef.current) setTokenLines(plainLines(code));
      }
    })();

    return () => {
      aliveRef.current = false;
    };
  }, [cacheKey, code, displayLang]);

  const allLines = tokenLines ?? plainLines(code);
  const total = allLines.length;
  const folded = total > FOLD_LINES && !expanded;
  const shownLines = folded ? allLines.slice(0, FOLD_LINES) : allLines;

  function copy() {
    navigator.clipboard?.writeText(code);
    setCopied(true);
    window.setTimeout(() => {
      if (aliveRef.current) setCopied(false);
    }, 1200);
  }

  async function openInBrowser() {
    try {
      const path = await invoke<string>("write_temp_html", { content: code });
      await openPath(path);
    } catch (error) {
      console.warn("打开 HTML 失败", error);
    }
  }

  return (
    <div className="mm-code">
      <div className="mm-code-head">
        <span className="lang">{displayLang}</span>
        <span className="sp" />
        {isHtml && (
          <button
            type="button"
            className="btn"
            aria-label={t("codeBlock.openInBrowser")}
            title={t("codeBlock.openTemporaryHtml")}
            onClick={openInBrowser}
          >
            <svg
              viewBox="0 0 24 24"
              width="13"
              height="13"
              fill="none"
              stroke="currentColor"
              strokeWidth="2"
              strokeLinecap="round"
              strokeLinejoin="round"
              aria-hidden="true"
            >
              <path d="M14 3h7v7M10 14L21 3M21 14v5a2 2 0 01-2 2H5a2 2 0 01-2-2V5a2 2 0 012-2h5" />
            </svg>
          </button>
        )}
        <button type="button" className="btn" onClick={copy}>
          {copied ? t("codeBlock.copied") : t("codeBlock.copy")}
        </button>
      </div>
      <pre>
        {shownLines.map((line, i) => (
          <div key={i} className="mm-code-line">
            <span className="ln">{i + 1}</span>
            {line.map((token, j) => (
              <span
                key={j}
                className={token.color ? undefined : "tx"}
                style={token.color ? { color: token.color } : undefined}
              >
                {token.content}
              </span>
            ))}
          </div>
        ))}
      </pre>
      {total > FOLD_LINES && (
        <button
          type="button"
          className="mm-code-expand"
          onClick={() => setExpanded((value) => !value)}
        >
          {expanded
            ? t("codeBlock.collapse")
            : t("codeBlock.expandLines", { n: total - FOLD_LINES })}
        </button>
      )}
    </div>
  );
}

export const CodeBlock = memo(CodeBlockImpl);

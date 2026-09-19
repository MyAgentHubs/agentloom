import { invoke } from "@tauri-apps/api/core";
import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  type ReactNode,
} from "react";
import { createPortal } from "react-dom";
import { useI18n } from "../i18n";
import "../styles/global-search.css";

export type GlobalSearchResult = {
  session_id: string;
  message_id: number | null;
  title: string;
  project: string;
  snippet: string;
  archived: boolean;
  updated_at: number;
};

type Props = {
  open: boolean;
  currentId: string | null;
  shortcutEnabled?: boolean;
  onOpen: () => void;
  onClose: () => void;
  onSelect: (result: GlobalSearchResult) => void;
};

const RECENT_STORAGE_KEY = "agentloom.globalSearch.recentSessionIds";

function readRecentSessionIds(): string[] {
  try {
    const parsed = JSON.parse(localStorage.getItem(RECENT_STORAGE_KEY) ?? "[]");
    return Array.isArray(parsed)
      ? parsed.filter((value): value is string => typeof value === "string")
      : [];
  } catch {
    return [];
  }
}

function queryTerms(query: string): string[] {
  return [...new Set(query.trim().split(/\s+/).filter(Boolean))];
}

function escapeRegex(value: string): string {
  return value.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

// 生成本次请求的序号：`Date.now() * 1000 + 本地计数`。后端 `SEARCH_REQUEST_SEQ`
// 是进程级 `static`，webview 重载（用户改分辨率/前端 HMR/意外 reload）不会重启
// Rust 进程；若序号只从组件内 `useRef(0)` 重新计数，重载后首个请求的序号会小于
// 后端已记过的历史高水位，被 `fetch_max` 判过期直接吞成空结果。用真实墙钟时间
// 做基底，只要重载间真实时间流逝（哪怕 1ms），新序号必然大于旧模块所有历史序号。
export function nextGlobalSearchRequestSeq(counter: {
  current: number;
}): number {
  counter.current += 1;
  return Date.now() * 1000 + counter.current;
}

function HighlightedText({ text, query }: { text: string; query: string }) {
  const terms = queryTerms(query);
  if (!terms.length) return text;
  const pattern = new RegExp(`(${terms.map(escapeRegex).join("|")})`, "gi");
  const normalizedTerms = new Set(
    terms.map((term) => term.toLocaleLowerCase()),
  );
  return text
    .split(pattern)
    .map((part, index) =>
      normalizedTerms.has(part.toLocaleLowerCase()) ? (
        <mark key={`${part}-${index}`}>{part}</mark>
      ) : (
        <span key={`${part}-${index}`}>{part}</span>
      ),
    );
}

export function GlobalSearch({
  open,
  currentId,
  shortcutEnabled = true,
  onOpen,
  onClose,
  onSelect,
}: Props) {
  const { t } = useI18n();
  const inputRef = useRef<HTMLInputElement>(null);
  const [query, setQuery] = useState("");
  const [results, setResults] = useState<GlobalSearchResult[]>([]);
  const [selected, setSelected] = useState(0);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [recentIds, setRecentIds] = useState(readRecentSessionIds);
  const [isComposing, setIsComposing] = useState(false);
  // requestSeqRef：最近一次已发出请求的完整序号（供 .then/.catch/.finally 判过期）。
  // requestSeqCounterRef：本组件挂载期内的本地单调计数，只作 nextGlobalSearchRequestSeq
  // 的低位输入，不直接对外比较。
  const requestSeqRef = useRef(0);
  const requestSeqCounterRef = useRef(0);

  useEffect(() => {
    if (!currentId) return;
    setRecentIds((current) => {
      const next = [
        currentId,
        ...current.filter((id) => id !== currentId),
      ].slice(0, 20);
      try {
        localStorage.setItem(RECENT_STORAGE_KEY, JSON.stringify(next));
      } catch {
        // Hardened webviews may disable localStorage; in-memory recents still work.
      }
      return next;
    });
  }, [currentId]);

  useEffect(() => {
    if (!open) return;
    setSelected(0);
    setError(null);
    // 输入法组合中（如中文拼音）：每个候选字都会触发 onChange，此时不发查询，
    // 等 compositionend 落定后（isComposing 变 false）这个 effect 会重跑再发一次。
    if (isComposing) return;
    let cancelled = false;
    const handle = window.setTimeout(
      () => {
        const seq = nextGlobalSearchRequestSeq(requestSeqCounterRef);
        requestSeqRef.current = seq;
        setLoading(true);
        void invoke<GlobalSearchResult[]>("search_sessions", {
          query,
          limit: query.trim() ? 20 : 50,
          requestSeq: seq,
        })
          .then((next) => {
            if (cancelled || seq !== requestSeqRef.current) return;
            if (!query.trim() && recentIds.length) {
              const recentOrder = new Map(
                recentIds.map((sessionId, index) => [sessionId, index]),
              );
              next.sort((a, b) => {
                const aOrder = recentOrder.get(a.session_id);
                const bOrder = recentOrder.get(b.session_id);
                if (aOrder != null || bOrder != null) {
                  return (
                    (aOrder ?? Number.MAX_SAFE_INTEGER) -
                    (bOrder ?? Number.MAX_SAFE_INTEGER)
                  );
                }
                return b.updated_at - a.updated_at;
              });
            }
            setResults(query.trim() ? next : next.slice(0, 5));
            setError(null);
          })
          .catch((cause) => {
            if (cancelled || seq !== requestSeqRef.current) return;
            setResults([]);
            setError(String(cause));
          })
          .finally(() => {
            if (!cancelled && seq === requestSeqRef.current) setLoading(false);
          });
      },
      query.trim() ? 220 : 0,
    );
    return () => {
      cancelled = true;
      window.clearTimeout(handle);
    };
  }, [open, query, recentIds, isComposing]);

  useEffect(() => {
    const onShortcut = (event: KeyboardEvent) => {
      if ((event.metaKey || event.ctrlKey) && event.key.toLowerCase() === "k") {
        if (!shortcutEnabled) return;
        event.preventDefault();
        onOpen();
      }
    };
    document.addEventListener("keydown", onShortcut);
    return () => document.removeEventListener("keydown", onShortcut);
  }, [onOpen, shortcutEnabled]);

  useEffect(() => {
    if (!open) return;
    const previouslyFocused = document.activeElement as HTMLElement | null;
    inputRef.current?.focus();
    return () => previouslyFocused?.focus();
  }, [open]);

  useEffect(() => {
    if (open && !shortcutEnabled) onClose();
  }, [onClose, open, shortcutEnabled]);

  const openResult = useCallback(
    (index: number) => {
      const result = results[index];
      if (!result) return;
      onSelect(result);
      onClose();
    },
    [onClose, onSelect, results],
  );

  useEffect(() => {
    if (!open) return;
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        event.preventDefault();
        onClose();
        return;
      }
      if ((event.metaKey || event.ctrlKey) && /^[1-9]$/.test(event.key)) {
        const index = Number(event.key) - 1;
        if (results[index]) {
          event.preventDefault();
          openResult(index);
        }
      }
    };
    document.addEventListener("keydown", onKeyDown);
    return () => document.removeEventListener("keydown", onKeyDown);
  }, [onClose, open, openResult, results]);

  const heading = query.trim()
    ? t("globalSearch.results", { n: results.length })
    : t("globalSearch.recent");
  const activeId = results[selected]
    ? `global-search-result-${selected}`
    : undefined;
  const resultRows = useMemo<ReactNode>(() => {
    if (error) {
      return (
        <div className="global-search__empty" role="alert">
          <strong>{t("globalSearch.error")}</strong>
          <span>{error}</span>
        </div>
      );
    }
    if (!loading && !results.length) {
      return (
        <div className="global-search__empty">
          <strong>
            {query.trim()
              ? t("globalSearch.noResults", { query })
              : t("globalSearch.noRecent")}
          </strong>
          <span>{t("globalSearch.noResultsHelp")}</span>
        </div>
      );
    }
    return results.map((result, index) => (
      <button
        id={`global-search-result-${index}`}
        key={result.session_id}
        type="button"
        role="option"
        aria-selected={selected === index}
        className={`global-search__result${selected === index ? " is-selected" : ""}`}
        onMouseEnter={() => setSelected(index)}
        onClick={() => openResult(index)}
      >
        <span className="global-search__title">
          <HighlightedText text={result.title} query={query} />
        </span>
        <span className="global-search__side">
          <span className="global-search__project">
            <HighlightedText text={result.project} query={query} />
          </span>
          {result.archived && (
            <span className="global-search__archived">
              {t("globalSearch.archived")}
            </span>
          )}
          {index < 9 && <kbd>⌘{index + 1}</kbd>}
        </span>
        <span className="global-search__snippet">
          <HighlightedText
            text={result.snippet || t("globalSearch.noMessages")}
            query={query}
          />
        </span>
      </button>
    ));
  }, [error, loading, openResult, query, results, selected, t]);

  if (!open) return null;
  return createPortal(
    <div
      className="global-search__scrim"
      onMouseDown={(event) => {
        if (event.target === event.currentTarget) onClose();
      }}
    >
      <section
        className="global-search"
        role="dialog"
        aria-modal="true"
        aria-label={t("globalSearch.dialogLabel")}
      >
        <div className="global-search__input-row">
          <svg viewBox="0 0 24 24" aria-hidden="true">
            <circle cx="11" cy="11" r="7" />
            <path d="M21 21l-4.3-4.3" />
          </svg>
          <input
            ref={inputRef}
            type="search"
            value={query}
            aria-label={t("globalSearch.inputLabel")}
            aria-controls="global-search-results"
            aria-activedescendant={activeId}
            placeholder={t("globalSearch.placeholder")}
            onChange={(event) => {
              setQuery(event.target.value);
              setSelected(0);
            }}
            onCompositionStart={() => setIsComposing(true)}
            onCompositionEnd={(event) => {
              setIsComposing(false);
              setQuery(event.currentTarget.value);
            }}
            onKeyDown={(event) => {
              if (event.key === "ArrowDown") {
                event.preventDefault();
                setSelected((current) =>
                  Math.min(current + 1, Math.max(results.length - 1, 0)),
                );
              } else if (event.key === "ArrowUp") {
                event.preventDefault();
                setSelected((current) => Math.max(current - 1, 0));
              } else if (event.key === "Enter") {
                event.preventDefault();
                openResult(selected);
              }
            }}
          />
          {query && (
            <button
              type="button"
              className="global-search__clear"
              aria-label={t("globalSearch.clear")}
              onClick={() => setQuery("")}
            >
              ×
            </button>
          )}
          <kbd>esc</kbd>
        </div>
        <div className="global-search__results-head">
          <span>{loading ? t("globalSearch.searching") : heading}</span>
          <span>{t("globalSearch.allProjects")}</span>
        </div>
        <div
          id="global-search-results"
          className="global-search__results"
          role="listbox"
          aria-label={heading}
        >
          {resultRows}
        </div>
        <footer className="global-search__footer">
          <span>
            <kbd>↑↓</kbd>
            {t("globalSearch.selectHint")}
          </span>
          <span>
            <kbd>↵</kbd>
            {t("globalSearch.openHint")}
          </span>
          <span>
            <kbd>⌘1</kbd>
            {t("globalSearch.quickOpenHint")}
          </span>
          <span className="global-search__local">
            {t("globalSearch.localOnly")}
          </span>
        </footer>
      </section>
    </div>,
    document.body,
  );
}

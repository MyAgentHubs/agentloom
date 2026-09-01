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
    let cancelled = false;
    const handle = window.setTimeout(
      () => {
        setLoading(true);
        void invoke<GlobalSearchResult[]>("search_sessions", {
          query,
          limit: query.trim() ? 20 : 50,
        })
          .then((next) => {
            if (cancelled) return;
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
            if (cancelled) return;
            setResults([]);
            setError(String(cause));
          })
          .finally(() => {
            if (!cancelled) setLoading(false);
          });
      },
      query.trim() ? 140 : 0,
    );
    return () => {
      cancelled = true;
      window.clearTimeout(handle);
    };
  }, [open, query, recentIds]);

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

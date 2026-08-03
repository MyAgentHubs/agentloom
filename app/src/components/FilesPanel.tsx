import {
  useEffect,
  useMemo,
  useRef,
  useState,
  type MutableRefObject,
  type ReactNode,
} from "react";
import { invoke } from "@tauri-apps/api/core";
import { useI18n } from "../i18n";
import { renderBackendError } from "../lib/backendMsg";
import { useMarkdownLib } from "../lib/useMarkdown";
import { CodeBlock } from "./CodeBlock";

type ProjectFileEntry = {
  path: string;
  name: string;
  isDir: boolean;
  depth: number;
  size: number | null;
};

type ProjectFileRead = {
  path: string;
  name: string;
  content: string;
  size: number;
  language: string;
  isMarkdown: boolean;
};

type ProjectFileListing = {
  entries: ProjectFileEntry[];
  truncated: boolean;
};

type Props = {
  sessionId: string | null;
  repoId?: string | null;
  repoName?: string | null;
};

// 与后端 PROJECT_FILE_MAX_ENTRIES（app/src-tauri/src/lib.rs）保持一致，仅用于截断提示文案。
const PROJECT_FILE_MAX_ENTRIES = 1000;

const ic = {
  viewBox: "0 0 24 24",
  fill: "none",
  stroke: "currentColor",
  strokeWidth: 2,
  strokeLinecap: "round" as const,
  strokeLinejoin: "round" as const,
  width: 14,
  height: 14,
};

function preferredFile(entries: ProjectFileEntry[]): string | null {
  const files = entries.filter((entry) => !entry.isDir);
  return (
    files.find((entry) => /^readme\.md$/i.test(entry.name))?.path ??
    files.find((entry) => entry.name.toLowerCase().endsWith(".md"))?.path ??
    files[0]?.path ??
    null
  );
}

function pathParts(repoName: string, path: string | null): string[] {
  return [repoName, ...(path ? path.split("/") : [])].filter(Boolean);
}

function escapeRegExp(s: string): string {
  return s.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

// 直接在原文（未小写化）上做大小写不敏感匹配：toLowerCase() 对少数字符
// （如 İ U+0130）不保证码元数不变，若先把 content 小写化再算下标，下标会与
// 原文错位、高亮跟着漂移。用带 i 标志的正则在原文上找，下标天然对齐原文。
function matchPositions(content: string, query: string): number[] {
  const q = query.trim();
  if (!q) return [];
  const positions: number[] = [];
  const re = new RegExp(escapeRegExp(q), "gi");
  let match: RegExpExecArray | null;
  while ((match = re.exec(content)) !== null) {
    positions.push(match.index);
    // 防御性保护：正常情况下转义后的字面量查询不会零宽匹配，但保留这一步
    // 避免任何边界情况下 lastIndex 卡住导致死循环。
    if (match.index === re.lastIndex) {
      re.lastIndex += 1;
    }
  }
  return positions;
}

// 按 matches 字符区间把纯文本切成片段，命中区间包 <mark>；当前命中额外挂 ref 以便滚动定位。
// 不用正则替换，避免特殊字符被误当元字符处理。
function renderSourceWithHighlights(
  content: string,
  matches: number[],
  queryLength: number,
  activeIndex: number,
  activeRef: MutableRefObject<HTMLElement | null>,
): ReactNode[] {
  const nodes: ReactNode[] = [];
  let cursor = 0;
  matches.forEach((start, index) => {
    if (start > cursor) {
      nodes.push(content.slice(cursor, start));
    }
    const end = start + queryLength;
    const active = index === activeIndex;
    nodes.push(
      <mark
        key={`hit-${start}`}
        ref={
          active
            ? (el: HTMLElement | null) => {
                activeRef.current = el;
              }
            : undefined
        }
        className={active ? "files-view__hit--active" : undefined}
      >
        {content.slice(start, end)}
      </mark>,
    );
    cursor = end;
  });
  if (cursor < content.length) {
    nodes.push(content.slice(cursor));
  }
  return nodes;
}

// 纯函数封装，供测试直接验证命中高亮渲染结果；组件本身仍分两步调用
// matchPositions + renderSourceWithHighlights（见下方 JSX），不经过这个入口。
export function highlightMatches(content: string, query: string): ReactNode[] {
  const matches = matchPositions(content, query);
  const queryLength = query.trim().length;
  const dummyRef: MutableRefObject<HTMLElement | null> = { current: null };
  return renderSourceWithHighlights(
    content,
    matches,
    queryLength,
    -1,
    dummyRef,
  );
}

function ancestorDirectories(path: string): string[] {
  const parts = path.split("/");
  const ancestors: string[] = [];
  for (let index = 1; index < parts.length; index += 1) {
    ancestors.push(parts.slice(0, index).join("/"));
  }
  return ancestors;
}

function visibleTreeEntries(
  entries: ProjectFileEntry[],
  filter: string,
  collapsedDirs: Set<string>,
): ProjectFileEntry[] {
  const q = filter.trim().toLowerCase();
  const filtered = q
    ? entries.filter((entry) => entry.path.toLowerCase().includes(q))
    : entries;
  if (q) return filtered;
  return filtered.filter((entry) =>
    ancestorDirectories(entry.path).every((dir) => !collapsedDirs.has(dir)),
  );
}

export function FilesPanel({ sessionId, repoId = null, repoName }: Props) {
  const { t } = useI18n();
  const markdownLib = useMarkdownLib();
  const [entries, setEntries] = useState<ProjectFileEntry[]>([]);
  const [truncated, setTruncated] = useState(false);
  const [activePath, setActivePath] = useState<string | null>(null);
  const [file, setFile] = useState<ProjectFileRead | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [filter, setFilter] = useState("");
  const [findOpen, setFindOpen] = useState(false);
  const [findQuery, setFindQuery] = useState("");
  const [findIndex, setFindIndex] = useState(0);
  const [sourceMode, setSourceMode] = useState(false);
  const [treeHidden, setTreeHidden] = useState(false);
  const [collapsedDirs, setCollapsedDirs] = useState<Set<string>>(
    () => new Set(),
  );
  const findActiveRef = useRef<HTMLElement | null>(null);

  const source = useMemo(() => {
    if (sessionId) {
      return {
        kind: "session" as const,
        id: sessionId,
        listCommand: "list_session_files",
        readCommand: "read_session_file",
        args: { sessionId },
      };
    }
    if (repoId) {
      return {
        kind: "repo" as const,
        id: repoId,
        listCommand: "list_repo_files",
        readCommand: "read_repo_file",
        args: { repoId },
      };
    }
    return null;
  }, [repoId, sessionId]);

  useEffect(() => {
    if (!source) {
      setEntries([]);
      setTruncated(false);
      setActivePath(null);
      setFile(null);
      return;
    }
    let alive = true;
    setLoading(true);
    setError(null);
    setEntries([]);
    setTruncated(false);
    setActivePath(null);
    setFile(null);
    invoke<ProjectFileListing>(source.listCommand, source.args)
      .then((listing) => {
        if (!alive) return;
        const next = listing.entries;
        setEntries(next);
        setTruncated(listing.truncated);
        // Fold-default: directories start collapsed, expand on click.
        setCollapsedDirs(
          new Set(
            next.filter((entry) => entry.isDir).map((entry) => entry.path),
          ),
        );
        setActivePath(preferredFile(next));
      })
      .catch((err) => {
        if (alive) setError(String(err));
      })
      .finally(() => {
        if (alive) setLoading(false);
      });
    return () => {
      alive = false;
    };
  }, [source]);

  useEffect(() => {
    if (!source || !activePath) {
      setFile(null);
      return;
    }
    let alive = true;
    setError(null);
    invoke<ProjectFileRead>(source.readCommand, {
      ...source.args,
      path: activePath,
    })
      .then((next) => {
        if (!alive) return;
        setFile(next);
        setFindIndex(0);
        setSourceMode(false);
      })
      .catch((err) => {
        if (alive) setError(String(err));
      });
    return () => {
      alive = false;
    };
  }, [source, activePath]);

  const visibleEntries = useMemo(() => {
    return visibleTreeEntries(entries, filter, collapsedDirs);
  }, [entries, filter, collapsedDirs]);

  const matches = useMemo(
    () => matchPositions(file?.content ?? "", findQuery),
    [file?.content, findQuery],
  );
  const findLabel =
    findQuery.trim() === ""
      ? "0 / 0"
      : matches.length === 0
        ? "0 / 0"
        : `${findIndex + 1} / ${matches.length}`;

  const stepFind = (delta: number) => {
    if (matches.length === 0) return;
    setFindIndex((idx) => (idx + delta + matches.length) % matches.length);
  };
  // 搜索激活时正文强制切到纯文本源码视图渲染高亮，不改用户的源码/预览切换选择。
  const showFindHighlight = findQuery.trim() !== "" && matches.length > 0;

  useEffect(() => {
    if (!showFindHighlight) return;
    findActiveRef.current?.scrollIntoView({ block: "nearest" });
  }, [showFindHighlight, findIndex, matches, findQuery]);
  const toggleDirectory = (path: string) => {
    setCollapsedDirs((prev) => {
      const next = new Set(prev);
      if (next.has(path)) {
        next.delete(path);
      } else {
        next.add(path);
      }
      return next;
    });
  };

  if (!source) {
    return (
      <div className="files-empty">
        <div className="files-empty__h">{t("files.emptyTitle")}</div>
        <div className="files-empty__s">{t("files.emptyDesc")}</div>
      </div>
    );
  }

  const repo = repoName ?? "Project";
  const parts = pathParts(repo, file?.path ?? activePath);
  const showMarkdown = file?.isMarkdown && !sourceMode;

  return (
    <div className="files-view">
      <div className="files-view__head">
        <div className="files-view__crumb" title={parts.join(" › ")}>
          {parts.map((part, index) => (
            <span
              key={`${part}-${index}`}
              className={index === 0 ? "repo" : "part"}
            >
              {index > 0 && <span className="sep">›</span>}
              {part}
            </span>
          ))}
        </div>
        <div className="files-view__actions">
          {file?.isMarkdown && (
            <button
              className={`files-view__icon${sourceMode ? " active" : ""}`}
              aria-label={sourceMode ? t("files.rendered") : t("files.source")}
              title={sourceMode ? t("files.rendered") : t("files.source")}
              onClick={() => setSourceMode((v) => !v)}
            >
              {sourceMode ? (
                <svg {...ic}>
                  <path d="M2 12s3.5-7 10-7 10 7 10 7-3.5 7-10 7-10-7-10-7z" />
                  <circle cx="12" cy="12" r="3" />
                </svg>
              ) : (
                <svg {...ic}>
                  <path d="M8 8l-4 4 4 4M16 8l4 4-4 4" />
                </svg>
              )}
            </button>
          )}
          <button
            className={`files-view__icon${findOpen ? " active" : ""}`}
            aria-label={t("files.find")}
            title={t("files.find")}
            onClick={() => setFindOpen((v) => !v)}
          >
            <svg {...ic}>
              <circle cx="11" cy="11" r="7" />
              <path d="M21 21l-4-4" />
            </svg>
          </button>
          <button
            className="files-view__icon"
            aria-label={t("files.copyPath")}
            title={t("files.copyPath")}
            disabled={!file && !activePath}
            onClick={() => {
              const path = file?.path ?? activePath;
              if (path) void navigator.clipboard?.writeText(path);
            }}
          >
            <svg {...ic}>
              <rect x="9" y="9" width="11" height="11" rx="2" />
              <path d="M5 15V5a2 2 0 012-2h8" />
            </svg>
          </button>
          <button
            className={`files-view__icon${treeHidden ? " active" : ""}`}
            aria-label={treeHidden ? t("files.showTree") : t("files.hideTree")}
            title={treeHidden ? t("files.showTree") : t("files.hideTree")}
            onClick={() => setTreeHidden((v) => !v)}
          >
            <svg {...ic}>
              <path d="M3 7a2 2 0 012-2h4l2 2h8a2 2 0 012 2v8a2 2 0 01-2 2H5a2 2 0 01-2-2z" />
            </svg>
          </button>
        </div>
      </div>

      {findOpen && (
        <div className="files-view__find">
          <div className="files-view__findbox">
            <svg {...ic}>
              <circle cx="11" cy="11" r="7" />
              <path d="M21 21l-4-4" />
            </svg>
            <input
              value={findQuery}
              onChange={(event) => {
                setFindQuery(event.target.value);
                setFindIndex(0);
              }}
              placeholder={t("files.findPlaceholder")}
            />
          </div>
          <span className="files-view__findcount">{findLabel}</span>
          <button
            className="files-view__findbtn"
            aria-label={t("files.prevMatch")}
            onClick={() => stepFind(-1)}
          >
            <svg {...ic}>
              <path d="M12 19V5M6 11l6-6 6 6" />
            </svg>
          </button>
          <button
            className="files-view__findbtn"
            aria-label={t("files.nextMatch")}
            onClick={() => stepFind(1)}
          >
            <svg {...ic}>
              <path d="M12 5v14M6 13l6 6 6-6" />
            </svg>
          </button>
        </div>
      )}

      {error && (
        <div className="files-view__error">{renderBackendError(error, t)}</div>
      )}
      <div
        className={`files-view__body${treeHidden ? " files-view__body--full" : ""}`}
      >
        <div className="files-view__content">
          {loading && !file ? (
            <div className="files-view__placeholder">{t("files.loading")}</div>
          ) : file ? (
            showFindHighlight ? (
              <div className="files-src">
                {renderSourceWithHighlights(
                  file.content,
                  matches,
                  findQuery.trim().length,
                  findIndex,
                  findActiveRef,
                )}
              </div>
            ) : showMarkdown ? (
              <div className="files-md">
                {markdownLib ? (
                  <markdownLib.Markdown remarkPlugins={[markdownLib.remarkGfm]}>
                    {file.content}
                  </markdownLib.Markdown>
                ) : (
                  <div style={{ whiteSpace: "pre-wrap" }}>{file.content}</div>
                )}
              </div>
            ) : (
              <div className="files-code">
                <CodeBlock code={file.content} lang={file.language} />
              </div>
            )
          ) : (
            <div className="files-view__placeholder">
              {t("files.noPreview")}
            </div>
          )}
        </div>
        {!treeHidden && (
          <div className="files-tree">
            <div className="files-tree__filter">
              <svg {...ic}>
                <circle cx="11" cy="11" r="7" />
                <path d="M21 21l-4-4" />
              </svg>
              <input
                value={filter}
                onChange={(event) => setFilter(event.target.value)}
                placeholder={t("files.filterPlaceholder")}
              />
            </div>
            {truncated && (
              <div className="files-tree__truncated">
                {t("files.truncated", { max: PROJECT_FILE_MAX_ENTRIES })}
              </div>
            )}
            <div className="files-tree__scroll">
              {visibleEntries.map((entry) => {
                const collapsed = entry.isDir && collapsedDirs.has(entry.path);
                return (
                  <button
                    key={entry.path}
                    className={`files-tree__row${
                      entry.isDir ? " dir" : ""
                    }${entry.path === activePath ? " on" : ""}${
                      entry.isDir && !collapsed ? " is-open" : ""
                    }`}
                    style={{ paddingLeft: 7 + entry.depth * 14 }}
                    aria-expanded={entry.isDir ? !collapsed : undefined}
                    aria-label={
                      entry.isDir
                        ? t(
                            collapsed
                              ? "files.expandDirectory"
                              : "files.collapseDirectory",
                            { path: entry.path },
                          )
                        : t("files.openFile", { path: entry.path })
                    }
                    onClick={() =>
                      entry.isDir
                        ? toggleDirectory(entry.path)
                        : setActivePath(entry.path)
                    }
                  >
                    <span className="files-tree__twisty" aria-hidden="true">
                      {entry.isDir && (
                        <svg {...ic}>
                          <path d="M9 6l6 6-6 6" />
                        </svg>
                      )}
                    </span>
                    <span
                      className={`files-tree__kind files-tree__kind--${
                        entry.isDir ? "dir" : "file"
                      }`}
                      aria-hidden="true"
                    >
                      {entry.isDir ? (
                        <svg {...ic}>
                          <path d="M3 7a2 2 0 012-2h4l2 2h8a2 2 0 012 2v8a2 2 0 01-2 2H5a2 2 0 01-2-2z" />
                        </svg>
                      ) : (
                        <svg {...ic}>
                          <path d="M14 2H7a2 2 0 00-2 2v16a2 2 0 002 2h10a2 2 0 002-2V7z" />
                          <path d="M14 2v5h5" />
                        </svg>
                      )}
                    </span>
                    <span className="files-tree__name" title={entry.path}>
                      {entry.name}
                    </span>
                  </button>
                );
              })}
            </div>
          </div>
        )}
      </div>
    </div>
  );
}

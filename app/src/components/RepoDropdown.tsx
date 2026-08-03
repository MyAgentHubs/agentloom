import { useEffect, useMemo, useRef, useState } from "react";
import { useI18n } from "../i18n";
import type { RepoMeta, Session } from "../types/agent";

type Props = {
  open: boolean;
  repos: RepoMeta[];
  activeRepoId: string | null;
  /** 全部 sessions · 算每行 .dd-ct（该 repo 下 session 数） */
  sessions: Session[];
  onSelectRepo: (id: string) => void;
  onClose: () => void;
};

/**
 * cluster L Phase 2 plan B Task 7 · repo dropdown（v4 state 4 严格保真）。
 * Mount 在 TopBar.tsx 内 .topbar__main 下 · open state 由 TopBar 控（B1）。
 * 仅 N repos 时 TopBar 条件 render。
 *
 * v4 真实 DOM：.dropdown.repo / .dd-search placeholder="搜索 repo…" / N 行 .dd-row[.active]
 *   含 .dd-check + .dd-av.repo + .dd-nm + .dd-ct（该 repo 下 session 数）
 *
 * 关闭机制 3 路径同 NamespaceDropdown。
 */
export function RepoDropdown({
  open,
  repos,
  activeRepoId,
  sessions,
  onSelectRepo,
  onClose,
}: Props) {
  const { t } = useI18n();
  const panelRef = useRef<HTMLDivElement>(null);
  const [query, setQuery] = useState("");

  useEffect(() => {
    if (!open) return;
    function onMouseDown(e: MouseEvent) {
      if (panelRef.current && !panelRef.current.contains(e.target as Node)) {
        onClose();
      }
    }
    document.addEventListener("mousedown", onMouseDown);
    return () => document.removeEventListener("mousedown", onMouseDown);
  }, [open, onClose]);

  useEffect(() => {
    if (!open) return;
    function onKey(e: KeyboardEvent) {
      if (e.key === "Escape") onClose();
    }
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  }, [open, onClose]);

  useEffect(() => {
    if (open) setQuery("");
  }, [open]);

  const sessCountByRepo = useMemo(() => {
    const m = new Map<string, number>();
    for (const s of sessions) {
      if (s.repo_id) m.set(s.repo_id, (m.get(s.repo_id) ?? 0) + 1);
    }
    return m;
  }, [sessions]);

  if (!open) return null;

  const q = query.trim().toLowerCase();
  const filtered = repos.filter(
    (r) => q === "" || r.name.toLowerCase().includes(q),
  );

  function pick(id: string) {
    onSelectRepo(id);
    onClose();
  }

  return (
    <div className="dropdown repo" ref={panelRef}>
      <div className="dd-search">
        <input
          placeholder={t("repoDropdown.searchPlaceholder")}
          value={query}
          onChange={(e) => setQuery(e.target.value)}
        />
      </div>
      {filtered.map((r) => {
        const active = r.id === activeRepoId;
        const initial = (r.name.slice(0, 1) || "?").toUpperCase();
        return (
          <div
            key={r.id}
            className={`dd-row${active ? " active" : ""}`}
            onClick={() => pick(r.id)}
          >
            <span className="dd-check">{active ? "✓" : ""}</span>
            <span className="dd-av repo">{initial}</span>
            <span className="dd-nm">{r.name}</span>
            <span className="dd-ct">{sessCountByRepo.get(r.id) ?? 0}</span>
          </div>
        );
      })}
    </div>
  );
}

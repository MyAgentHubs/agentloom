import { useEffect, useMemo, useRef, useState } from "react";
import { useI18n } from "../i18n";
import { NamespaceAvatar } from "./NamespaceAvatar";
import type { NamespaceMeta, RepoMeta } from "../types/agent";

type Props = {
  open: boolean;
  namespaces: NamespaceMeta[];
  activeNamespaceId: string;
  /** 所有 active repos（跨 namespace · 算每个 ns 行的 .dd-ct count） */
  allRepos: RepoMeta[];
  onSelectNamespace: (id: string) => void;
  onClose: () => void;
  onConnectGithub?: () => void;
  onManageRepos?: () => void;
  connectError?: string | null;
};

/**
 * cluster L Phase 2 plan B Task 4 · namespace dropdown（v4 state 3 严格保真）。
 * Mount 在 TopBar.tsx 内 .topbar__main 下 · open state 由 TopBar 控（B1 ownership）。
 *
 * v4 真实 DOM：.dropdown / .dd-search / .dd-section-title 含 .builtin-tag /
 *   .dd-row[.active] 含 .dd-check + NamespaceAvatar + .dd-nm 含 <small> 副标题 + .dd-ct count /
 *   .dd-div / .dd-foot.future「+ 连接 GitHub · plan 2b 后开放」
 *
 * 副标题方案：
 *   - Local 「· ~/.agentloom/local/」 hardcode（与 plan A seed path 一致）
 *   - github_org 「· {namespace.kind}」 fallback（后端暂无 description · cluster N 加 description 后替 · 不挖后端）
 * count = allRepos.filter(r => r.namespace_id === ns.id).length
 * 关闭机制 3 路径：外点 / Esc / 选项 onClose。
 */
export function NamespaceDropdown({
  open,
  namespaces,
  activeNamespaceId,
  allRepos,
  onSelectNamespace,
  onClose,
  onConnectGithub,
  onManageRepos,
  connectError,
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

  const repoCountByNs = useMemo(() => {
    const m = new Map<string, number>();
    for (const r of allRepos) {
      m.set(r.namespace_id, (m.get(r.namespace_id) ?? 0) + 1);
    }
    return m;
  }, [allRepos]);

  if (!open) return null;

  const q = query.trim().toLowerCase();
  const matches = (ns: NamespaceMeta) =>
    q === "" || ns.name.toLowerCase().includes(q);

  const local =
    namespaces.find((n) => n.kind === "local" && matches(n)) ?? null;
  const orgs = namespaces.filter((n) => n.kind === "github_org" && matches(n));

  function pick(id: string) {
    onSelectNamespace(id);
    onClose();
  }

  function subtitle(ns: NamespaceMeta): string {
    if (ns.kind === "local") return "· ~/.agentloom/local/";
    return `· ${ns.kind}`;
  }

  return (
    <div className="dropdown" ref={panelRef}>
      <div className="dd-search">
        <input
          placeholder={t("namespaceDropdown.searchPlaceholder")}
          value={query}
          onChange={(e) => setQuery(e.target.value)}
        />
      </div>
      {local && (
        <>
          <div className="dd-section-title">
            {t("namespaceDropdown.builtin")}{" "}
            <span className="builtin-tag">
              {t("namespaceDropdown.cannotDelete")}
            </span>
          </div>
          <div
            className={`dd-row${activeNamespaceId === local.id ? " active" : ""}`}
            onClick={() => pick(local.id)}
          >
            <span className="dd-check">
              {activeNamespaceId === local.id ? "✓" : ""}
            </span>
            <NamespaceAvatar namespace={local} size={16} />
            <span className="dd-nm">
              {local.name} <small>{subtitle(local)}</small>
            </span>
            <span className="dd-ct">{repoCountByNs.get(local.id) ?? 0}</span>
          </div>
        </>
      )}

      {orgs.length > 0 && (
        <>
          <div className="dd-div" />
          <div className="dd-section-title">GitHub</div>
          {orgs.map((ns) => {
            const active = activeNamespaceId === ns.id;
            return (
              <div
                key={ns.id}
                className={`dd-row${active ? " active" : ""}`}
                onClick={() => pick(ns.id)}
              >
                <span className="dd-check">{active ? "✓" : ""}</span>
                <NamespaceAvatar namespace={ns} size={16} />
                <span className="dd-nm">
                  {ns.name} <small>{subtitle(ns)}</small>
                </span>
                <span className="dd-ct">{repoCountByNs.get(ns.id) ?? 0}</span>
              </div>
            );
          })}
        </>
      )}

      <div className="dd-div" />
      <div className="dd-row dd-manage" onClick={() => onManageRepos?.()}>
        {t("namespaceDropdown.manageRepos")}
      </div>
      <div className="dd-div" />
      <div
        className="dd-foot"
        role="button"
        tabIndex={0}
        onClick={() => onConnectGithub?.()}
        onKeyDown={(e) => {
          if (e.key === "Enter" || e.key === " ") onConnectGithub?.();
        }}
      >
        <span className="dd-foot__ic">＋</span>
        {t("namespaceDropdown.connectGithub")}
      </div>
      {connectError && (
        <div className="dd-foot__err" role="alert">
          {connectErrorText(connectError, t)}
        </div>
      )}
    </div>
  );
}

function connectErrorText(
  code: string,
  t: ReturnType<typeof useI18n>["t"],
): string {
  if (code === "NOT_GIT") return t("repoConnection.error.notGit");
  if (code === "NOT_GITHUB") return t("repoConnection.error.notGithub");
  if (code === "NO_COMMITS") return t("repoConnection.error.noCommits");
  if (code.startsWith("ALREADY_ADDED"))
    return t("repoConnection.error.alreadyAdded");
  return t("repoConnection.error.generic");
}

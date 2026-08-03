import { useEffect, useRef, useState } from "react";
import type { MouseEvent, ReactNode } from "react";
import type { GroupMeta, Session } from "../types/agent";
import { useI18n } from "../i18n";

type Props = {
  group: GroupMeta;
  sessions: Session[];
  expanded: boolean;
  onToggle: () => void;
  onRename?: (id: string, name: string) => void;
  onRequestDelete?: (g: GroupMeta) => void;
  renderSess: (s: Session) => ReactNode;
};

export function SessionGroupSection({
  group,
  sessions,
  expanded,
  onToggle,
  onRename,
  onRequestDelete,
  renderSess,
}: Props) {
  const { t } = useI18n();
  const [hovered, setHovered] = useState(false);
  const [menuOpen, setMenuOpen] = useState(false);
  const [renaming, setRenaming] = useState(false);
  const [renameVal, setRenameVal] = useState(group.name);
  const inputRef = useRef<HTMLInputElement>(null);

  useEffect(() => {
    if (renaming) {
      const el = inputRef.current;
      if (el) {
        el.focus();
        el.select();
      }
    }
  }, [renaming]);

  function beginRename() {
    setRenameVal(group.name);
    setMenuOpen(false);
    setRenaming(true);
  }

  function cancelRename() {
    setRenaming(false);
  }

  function commitRename() {
    const next = renameVal.trim();
    setRenaming(false);
    if (next && next !== group.name) onRename?.(group.id, next);
  }

  function onHeadClick(e: MouseEvent<HTMLDivElement>) {
    if (renaming) return;
    const target = e.target as HTMLElement;
    if (target.closest("[data-action], .sb-group__more-wrap")) return;
    onToggle();
  }

  return (
    <div
      className={`sb-group${expanded ? "" : " collapsed"}`}
      onMouseEnter={() => setHovered(true)}
      onMouseLeave={() => {
        setHovered(false);
        setMenuOpen(false);
      }}
    >
      <div className="sb-group__head" onClick={onHeadClick}>
        <span className="sb-group__chev">{expanded ? "▾" : "▸"}</span>
        {renaming ? (
          <input
            ref={inputRef}
            className="sb-group__rename-input"
            type="text"
            value={renameVal}
            onClick={(e) => e.stopPropagation()}
            onChange={(e) => setRenameVal(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter" && !e.nativeEvent.isComposing)
                commitRename();
              else if (e.key === "Escape") cancelRename();
            }}
            onBlur={cancelRename}
          />
        ) : (
          <>
            <span className="sb-group__name">{group.name}</span>
            <span className="sb-group__count">{sessions.length}</span>
            {(hovered || menuOpen) && (
              <span
                className="sb-group__more-wrap"
                onClick={(e) => e.stopPropagation()}
              >
                <button
                  type="button"
                  className="sb-group__more"
                  data-action="group-more"
                  onClick={() => setMenuOpen((v) => !v)}
                >
                  ⋯
                </button>
                {menuOpen && (
                  <span className="sb-group__menu">
                    <button
                      type="button"
                      className="sb-group__mi"
                      data-action="group-rename"
                      onClick={beginRename}
                    >
                      {t("sessionGroup.rename")}
                    </button>
                    <button
                      type="button"
                      className="sb-group__mi sb-group__mi--danger"
                      data-action="group-delete"
                      onClick={() => {
                        setMenuOpen(false);
                        onRequestDelete?.(group);
                      }}
                    >
                      {t("sessionGroup.delete")}
                    </button>
                  </span>
                )}
              </span>
            )}
          </>
        )}
      </div>
      {expanded && (
        <div className="sb-group__body">{sessions.map(renderSess)}</div>
      )}
    </div>
  );
}

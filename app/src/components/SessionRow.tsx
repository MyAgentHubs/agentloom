import { useEffect, useRef, useState } from "react";
import type { Session, GroupMeta } from "../types/agent";
import { useDropdown } from "../hooks/useDropdown";
import { SessionMenu } from "./SessionMenu";
import { useI18n } from "../i18n";
import { formatRelativeTime } from "../lib/relativeTime";

export type SessionDotStatus = "running" | "attention" | "done";

type Props = {
  session: Session;
  active: boolean;
  running: boolean;
  /** 左栏行状态点三态（切走后仍知 agent 死活）：undefined/null 时退化用 running 二态（向后兼容） */
  dotStatus?: SessionDotStatus | null;
  isArchived: boolean;
  onSelect: (id: string) => void;
  onRename: (id: string, title: string) => void;
  onRequestDelete?: (session: Session) => void;
  onTogglePin: (id: string, next: boolean) => void;
  onToggleUnread: (id: string, next: boolean) => void;
  onToggleArchive: (id: string, next: boolean) => void;
  onHandover?: (id: string) => void;
  handoverBusy?: boolean;
  parentTitle?: string | null;
  hasContinuationThread?: boolean;
  groups?: GroupMeta[];
  onMoveSessionToGroup?: (sessionId: string, groupId: string | null) => void;
  onCreateGroup?: (name: string) => Promise<string> | void;
};

const pencilIcon = (
  <svg
    viewBox="0 0 24 24"
    fill="none"
    stroke="currentColor"
    strokeWidth={2}
    strokeLinecap="round"
    strokeLinejoin="round"
  >
    <path d="M12 20h9" />
    <path d="M16.5 3.5a2.1 2.1 0 013 3L7 19l-4 1 1-4 12.5-12.5z" />
  </svg>
);
const moreIcon = (
  <svg viewBox="0 0 24 24" fill="currentColor" stroke="none">
    <circle cx="5" cy="12" r="1.6" />
    <circle cx="12" cy="12" r="1.6" />
    <circle cx="19" cy="12" r="1.6" />
  </svg>
);
const pinIcon = (
  <svg
    viewBox="0 0 24 24"
    fill="none"
    stroke="currentColor"
    strokeWidth={2}
    strokeLinecap="round"
    strokeLinejoin="round"
  >
    <path d="M12 17v5" />
    <path d="M9 9V3.5a.5.5 0 01.5-.5h5a.5.5 0 01.5.5V9l2.5 3.5a1 1 0 01-.8 1.6H7.3a1 1 0 01-.8-1.6L9 9z" />
  </svg>
);

export function SessionRow({
  session,
  active,
  running,
  dotStatus = null,
  isArchived,
  onSelect,
  onRename,
  onRequestDelete,
  onTogglePin,
  onToggleUnread,
  onToggleArchive,
  onHandover,
  handoverBusy = false,
  parentTitle = null,
  hasContinuationThread: hasContinuationThreadProp,
  groups,
  onMoveSessionToGroup,
  onCreateGroup,
}: Props) {
  const { locale, t } = useI18n();
  const [hovered, setHovered] = useState(false);
  const [editing, setEditing] = useState(false);
  const [draft, setDraft] = useState(session.title);
  const savingRef = useRef(false);
  const inputRef = useRef<HTMLInputElement>(null);
  const dd = useDropdown();
  const [menuPos, setMenuPos] = useState<{ top: number; left: number } | null>(
    null,
  );

  const MENU_W = 206;
  function openMenu() {
    const el = dd.containerRef.current;
    if (el) {
      const r = el.getBoundingClientRect();
      const left = Math.max(
        8,
        Math.min(r.left, window.innerWidth - MENU_W - 8),
      );
      setMenuPos({ top: r.bottom + 4, left });
    }
    dd.setOpen(true);
  }

  const showActions = (hovered || dd.open) && !editing;
  const isChild = session.parent_session_id !== null;
  const hasContinuationThread =
    hasContinuationThreadProp ??
    (session.parent_session_id !== null ||
      session.continued_to_session_id !== null);
  const lineageParentTitle =
    parentTitle ?? t("continuation.lineage.fallbackParent");

  // 进入编辑态时可靠聚焦 + 全选（WKWebView 下 autoFocus + onFocus 不可靠 ·
  // 不聚焦会导致 Enter keydown 收不到 = 「回车没反应」+ 原名称不被全选）。
  useEffect(() => {
    if (editing) {
      const el = inputRef.current;
      if (el) {
        el.focus();
        el.select();
      }
    }
  }, [editing]);

  useEffect(() => {
    if (!dd.open) return;
    const close = () => dd.setOpen(false);
    window.addEventListener("scroll", close, true);
    window.addEventListener("resize", close);
    return () => {
      window.removeEventListener("scroll", close, true);
      window.removeEventListener("resize", close);
    };
  }, [dd.open]);

  function startRename() {
    setDraft(session.title);
    setEditing(true);
    dd.setOpen(false);
  }

  function commitRename() {
    if (savingRef.current) return;
    savingRef.current = true;
    const next = draft.trim();
    setEditing(false);
    if (next && next !== session.title) onRename(session.id, next);
    // 释放标志（下一 tick）
    setTimeout(() => {
      savingRef.current = false;
    }, 0);
  }

  function cancelRename() {
    setEditing(false);
    savingRef.current = false;
  }

  // dotStatus 优先（三态：running 暖橙脉动 / attention 红 / done 绿）；
  // 未传（undefined/null）时退化 running 二态（run/idle）——保既有调用点/测试不破。
  const dotClass =
    dotStatus === "running"
      ? " run"
      : dotStatus === "attention"
        ? " attention"
        : dotStatus === "done"
          ? " done"
          : running
            ? " run"
            : " idle";

  return (
    <div
      data-session-id={session.id}
      data-parent-session-id={session.parent_session_id ?? undefined}
      data-continued-to-session-id={
        session.continued_to_session_id ?? undefined
      }
      className={`sess${active ? " active" : ""}${
        isChild ? " sess--child" : ""
      }`}
      onClick={() => onSelect(session.id)}
      onMouseEnter={() => setHovered(true)}
      onMouseLeave={() => setHovered(false)}
      onContextMenu={(e) => {
        e.preventDefault();
        setHovered(true);
        dd.setOpen(true);
      }}
    >
      {isChild && <span className="sess__lin" aria-hidden="true" />}
      <span className={`sess__dot${dotClass}`} />
      {session.pinned && !isArchived && (
        <span className="sess__pin" aria-label={t("sessionRow.pinned")}>
          {pinIcon}
        </span>
      )}
      {editing ? (
        <span className="sess__rename" onClick={(e) => e.stopPropagation()}>
          <input
            ref={inputRef}
            type="text"
            value={draft}
            onChange={(e) => setDraft(e.target.value)}
            onKeyDown={(e) => {
              // IME 组词中按 Enter 是选词确认 · 不当提交（项目 composer 同款守卫）
              if (e.key === "Enter" && !e.nativeEvent.isComposing)
                commitRename();
              else if (e.key === "Escape") cancelRename();
            }}
            onBlur={cancelRename}
          />
          <span className="sess__keyhint">
            <span className="sess-kbd">↵</span>
            {t("sessionRow.saveShort")} <span className="sess-kbd">esc</span>
            {t("sessionRow.cancelShort")}
          </span>
        </span>
      ) : (
        <>
          <span
            className={`sess__nm${session.unread ? " sess__nm--unread" : ""}`}
          >
            {session.title}
          </span>
          {session.continued_to_session_id && (
            <span
              className="sess__lineage sess__lineage--parent"
              data-testid="session-lineage-parent"
            >
              {t("continuation.lineage.parentBadge")}
            </span>
          )}
          {session.parent_session_id && (
            <span
              className="sess__lineage sess__lineage--child"
              data-testid="session-lineage-child"
              title={t("continuation.lineage.childTooltip", {
                title: lineageParentTitle,
              })}
              aria-label={t("continuation.lineage.childTooltip", {
                title: lineageParentTitle,
              })}
            >
              ↳
            </span>
          )}
          {session.unread && (
            <span
              className="sess__unread"
              aria-label={t("sessionRow.unread")}
            />
          )}
          {!showActions && (
            <span className="sess__time">
              {formatRelativeTime(session.created_at, locale)}
            </span>
          )}
        </>
      )}

      {showActions && (
        <span className="sess__acts" onClick={(e) => e.stopPropagation()}>
          <button
            type="button"
            className="sess__iconbtn"
            data-action="row-rename"
            title={t("sessionRow.rename")}
            onClick={startRename}
          >
            {pencilIcon}
          </button>
          <div className="dd" ref={dd.containerRef}>
            <button
              type="button"
              className={`sess__iconbtn${dd.open ? " on" : ""}`}
              data-action="row-more"
              title={t("sessionRow.more")}
              {...dd.triggerProps}
              onClick={() => (dd.open ? dd.setOpen(false) : openMenu())}
            >
              {moreIcon}
            </button>
            {dd.open && (
              <SessionMenu
                pinned={session.pinned}
                unread={session.unread}
                isArchived={isArchived}
                running={running}
                alreadyContinued={!!session.continued_to_session_id}
                hasContinuationThread={hasContinuationThread}
                handoverBusy={handoverBusy}
                groups={groups ?? []}
                currentGroupId={session.group_id ?? null}
                style={
                  menuPos
                    ? {
                        position: "fixed",
                        top: menuPos.top,
                        left: menuPos.left,
                      }
                    : undefined
                }
                onRename={startRename}
                onTogglePin={(next) => onTogglePin(session.id, next)}
                onToggleUnread={(next) => onToggleUnread(session.id, next)}
                onToggleArchive={(next) => onToggleArchive(session.id, next)}
                onDelete={() => {
                  dd.setOpen(false);
                  onRequestDelete?.(session);
                }}
                onHandover={() => onHandover?.(session.id)}
                onClose={() => dd.setOpen(false)}
                onMoveSessionToGroup={(gid) =>
                  onMoveSessionToGroup?.(session.id, gid)
                }
                onCreateGroupAndMove={async (name) => {
                  const id = await onCreateGroup?.(name);
                  if (id) onMoveSessionToGroup?.(session.id, id);
                }}
              />
            )}
          </div>
        </span>
      )}
    </div>
  );
}

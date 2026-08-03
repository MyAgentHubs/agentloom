import { useRef, useState, type CSSProperties } from "react";
import type { GroupMeta } from "../types/agent";
import { useI18n } from "../i18n";

type Props = {
  pinned: boolean;
  unread: boolean;
  isArchived: boolean;
  running: boolean;
  alreadyContinued?: boolean;
  hasContinuationThread?: boolean;
  handoverBusy?: boolean;
  groups?: GroupMeta[];
  currentGroupId?: string | null;
  style?: CSSProperties;
  onRename: () => void;
  onTogglePin: (next: boolean) => void;
  onToggleUnread: (next: boolean) => void;
  onToggleArchive: (next: boolean) => void;
  onDelete: () => void;
  onHandover?: () => void;
  onClose: () => void;
  onMoveSessionToGroup?: (groupId: string | null) => void;
  onCreateGroupAndMove?: (name: string) => void;
};

function Kbd({ k }: { k: string }) {
  return <span className="sess-kbd">{k}</span>;
}

export function SessionMenu({
  pinned,
  unread,
  isArchived,
  running,
  alreadyContinued = false,
  hasContinuationThread = false,
  handoverBusy = false,
  groups = [],
  currentGroupId = null,
  style,
  onRename,
  onTogglePin,
  onToggleUnread,
  onToggleArchive,
  onDelete,
  onHandover,
  onClose,
  onMoveSessionToGroup,
  onCreateGroupAndMove,
}: Props) {
  const { t } = useI18n();
  const [view, setView] = useState<"menu" | "move" | "newgroup">("menu");
  const [newGroupName, setNewGroupName] = useState("");
  const [isComposing, setIsComposing] = useState(false);
  const newGroupInputRef = useRef<HTMLInputElement>(null);

  function run(fn: () => void) {
    fn();
    onClose();
  }

  const handoverDisabled =
    isArchived || running || alreadyContinued || handoverBusy || !onHandover;
  const handoverTitle = isArchived
    ? t("continuation.menu.disabled.archived")
    : running
      ? t("continuation.menu.disabled.running")
      : alreadyContinued
        ? t("continuation.menu.disabled.continued")
        : handoverBusy
          ? t("continuation.menu.disabled.assembling")
          : undefined;

  if (view === "move") {
    return (
      <div
        className="sess-menu"
        role="menu"
        style={style}
        onClick={(e) => e.stopPropagation()}
      >
        <button
          type="button"
          role="menuitem"
          className="sess-mi"
          data-action="move-back"
          onClick={() => setView("menu")}
        >
          <span className="sess-mi__lbl">{t("sessionMenu.back")}</span>
        </button>
        <div className="sess-mdiv" />
        <button
          type="button"
          role="menuitem"
          className="sess-mi"
          data-action="move-ungrouped"
          data-checked={currentGroupId === null ? "true" : undefined}
          onClick={() => {
            onMoveSessionToGroup?.(null);
            onClose();
          }}
        >
          <span className="sess-mi__lbl">
            {currentGroupId === null ? "✓ " : ""}
            {t("sessionMenu.ungrouped")}
          </span>
        </button>
        {groups.map((g) => (
          <button
            key={g.id}
            type="button"
            role="menuitem"
            className="sess-mi"
            data-group-id={g.id}
            data-checked={currentGroupId === g.id ? "true" : undefined}
            onClick={() => {
              onMoveSessionToGroup?.(g.id);
              onClose();
            }}
          >
            <span className="sess-mi__lbl">
              {currentGroupId === g.id ? "✓ " : ""}
              {g.name}
            </span>
          </button>
        ))}
        <div className="sess-mdiv" />
        <button
          type="button"
          role="menuitem"
          className="sess-mi"
          data-action="new-group-menu"
          onClick={() => setView("newgroup")}
        >
          <span className="sess-mi__lbl">{t("sessionMenu.newGroup")}</span>
        </button>
      </div>
    );
  }

  if (view === "newgroup") {
    return (
      <div
        className="sess-menu"
        role="menu"
        style={style}
        onClick={(e) => e.stopPropagation()}
      >
        <button
          type="button"
          role="menuitem"
          className="sess-mi"
          data-action="move-back"
          onClick={() => setView("move")}
        >
          <span className="sess-mi__lbl">{t("sessionMenu.back")}</span>
        </button>
        <div className="sess-mdiv" />
        <div className="sess-mi" onClick={(e) => e.stopPropagation()}>
          <input
            ref={newGroupInputRef}
            type="text"
            data-role="new-group-input"
            autoFocus
            value={newGroupName}
            placeholder={t("sessionMenu.groupNamePlaceholder")}
            onChange={(e) => setNewGroupName(e.target.value)}
            onCompositionStart={() => setIsComposing(true)}
            onCompositionEnd={() => setIsComposing(false)}
            onKeyDown={(e) => {
              if (
                e.key === "Enter" &&
                !isComposing &&
                !e.nativeEvent.isComposing
              ) {
                const name = newGroupName.trim();
                if (name) {
                  onCreateGroupAndMove?.(name);
                  onClose();
                }
              } else if (e.key === "Escape") {
                setView("menu");
              }
            }}
          />
        </div>
      </div>
    );
  }

  return (
    <div
      className="sess-menu"
      role="menu"
      style={style}
      onClick={(e) => e.stopPropagation()}
    >
      {!isArchived && (
        <button
          type="button"
          role="menuitem"
          className="sess-mi"
          data-action="pin"
          onClick={() => run(() => onTogglePin(!pinned))}
        >
          <span className="sess-mi__lbl">
            {pinned ? t("sessionMenu.unpin") : t("sessionMenu.pin")}
          </span>
          <Kbd k="P" />
        </button>
      )}
      <button
        type="button"
        role="menuitem"
        className="sess-mi"
        data-action="unread"
        onClick={() => run(() => onToggleUnread(!unread))}
      >
        <span className="sess-mi__lbl">
          {unread ? t("sessionMenu.markRead") : t("sessionMenu.markUnread")}
        </span>
        <Kbd k="U" />
      </button>
      <div className="sess-mdiv" />
      <button
        type="button"
        role="menuitem"
        className="sess-mi"
        data-action="rename"
        onClick={() => run(onRename)}
      >
        <span className="sess-mi__lbl">{t("sessionMenu.rename")}</span>
        <Kbd k="R" />
      </button>
      {!isArchived && (
        <button
          type="button"
          role="menuitem"
          className="sess-mi"
          data-action="move-to-group"
          onClick={() => setView("move")}
        >
          <span className="sess-mi__lbl">
            {hasContinuationThread
              ? t("sessionMenu.moveContinuationGroup")
              : t("sessionMenu.moveToGroup")}
          </span>
        </button>
      )}
      <button
        type="button"
        role="menuitem"
        className="sess-mi"
        data-action="handover"
        disabled={handoverDisabled}
        title={handoverTitle}
        onClick={() => run(() => onHandover?.())}
      >
        <span className="sess-mi__lbl">{t("continuation.menu.handover")}</span>
        <Kbd k="H" />
      </button>
      <div className="sess-mdiv" />
      <button
        type="button"
        role="menuitem"
        className="sess-mi"
        data-action="archive"
        onClick={() => run(() => onToggleArchive(!isArchived))}
      >
        <span className="sess-mi__lbl">
          {isArchived
            ? hasContinuationThread
              ? t("sessionMenu.restoreContinuationGroup")
              : t("sessionMenu.restore")
            : hasContinuationThread
              ? t("sessionMenu.archiveContinuationGroup")
              : t("sessionMenu.archive")}
        </span>
        <Kbd k="A" />
      </button>
      <div className="sess-mdiv" />
      <button
        type="button"
        role="menuitem"
        className="sess-mi sess-mi--danger"
        data-action="delete"
        disabled={running}
        title={running ? t("sessionMenu.stopBeforeDelete") : undefined}
        onClick={() => onDelete()}
      >
        <span className="sess-mi__lbl">{t("sessionMenu.delete")}</span>
        <Kbd k="D" />
      </button>
    </div>
  );
}

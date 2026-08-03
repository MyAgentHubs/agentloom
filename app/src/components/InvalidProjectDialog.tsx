import { invoke } from "@tauri-apps/api/core";
import { useI18n } from "../i18n";

type Props = {
  state: { repoId: string; kind: "invalid" | "archived" };
  /** action: "archived" | "restored" | "switched_default" · 让 App.tsx 知道做什么后续（refresh repos + 切 view） */
  onResolved: (action: "archived" | "restored" | "switched_default") => void;
  onClose: () => void;
};

/**
 * cluster L plan 2a · invalid / archived 项目修正对话框
 * 触发：① crumb dropdown 点 invalid 行（直接弹）② 发消息 / refreshReview 时 IPC 返
 *      PROJECT_INVALID:<id> / PROJECT_ARCHIVED:<id> 错误码（App.tsx handleProjectError 弹）
 * spec §7 case 5 期望 UX：「修正路径 / 归档 / 永久删除」对话框（强提示）
 * 本 plan 仅实现「归档 / 恢复 / 跳到默认会话」3 操作 · 「修正路径」UX 推后（spec §9 out of scope）
 */
export function InvalidProjectDialog({ state, onResolved, onClose }: Props) {
  const { t } = useI18n();
  const isInvalid = state.kind === "invalid";

  async function onArchive() {
    try {
      await invoke("archive_repo", { id: state.repoId });
      onResolved("archived");
    } catch {
      onClose();
    }
  }

  async function onRestore() {
    try {
      await invoke("restore_repo", { id: state.repoId });
      onResolved("restored");
    } catch {
      onClose();
    }
  }

  function onSwitchDefault() {
    onResolved("switched_default");
  }

  return (
    <div className="dialog__backdrop">
      <div className="dialog" role="dialog" aria-modal="true">
        <h2 className="dialog__title">
          {isInvalid
            ? t("invalidProjectDialog.title.invalid")
            : t("invalidProjectDialog.title.archived")}
        </h2>
        <p className="dialog__body">
          {isInvalid
            ? t("invalidProjectDialog.body.invalid")
            : t("invalidProjectDialog.body.archived")}
        </p>
        <div className="dialog__actions">
          <button
            type="button"
            className="dialog__btn"
            onClick={onSwitchDefault}
          >
            {t("invalidProjectDialog.switchDefault")}
          </button>
          {isInvalid ? (
            <button
              type="button"
              className="dialog__btn dialog__btn--danger"
              onClick={onArchive}
            >
              {t("invalidProjectDialog.archive")}
            </button>
          ) : (
            <button
              type="button"
              className="dialog__btn dialog__btn--primary"
              onClick={onRestore}
            >
              {t("invalidProjectDialog.restore")}
            </button>
          )}
        </div>
      </div>
    </div>
  );
}

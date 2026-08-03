import { useI18n } from "../i18n";

type Props = {
  /** 会话标题（主·粗·session-centric） */
  title: string;
  /** repo 名（次级灰·上下文）·可选 */
  repoLabel?: string;
  status: "idle" | "working";
  /** working 态副标（如 "working · 14s"）；idle 不传 */
  workingLabel?: string;
  onMenu?: () => void;
};

/**
 * surface header 左侧会话状态条（spec §2.C·UX round-2 Q2）：
 * 会话标题为主 + · repo 次级灰 + idle/working 状态点 + ⋯。
 * 注：现无 branch/dirty 数据源·repoLabel 用 repo 名·idle 不显「干净」字样。
 */
export function SessionContextBar({
  title,
  repoLabel,
  status,
  workingLabel,
  onMenu,
}: Props) {
  const { t } = useI18n();

  return (
    <div className="sf-ctx">
      <span className="sf-ctx__title">{title}</span>
      {repoLabel && <span className="sf-ctx__sub">· {repoLabel}</span>}
      <span className={`st${status === "working" ? " run" : ""}`}>
        <span className="d" />
        {status === "working" ? (workingLabel ?? "working") : null}
      </span>
      <span
        className="dots"
        role="button"
        aria-label={t("sessionContextBar.menu")}
        tabIndex={0}
        onClick={onMenu}
        onKeyDown={(e) => {
          if (e.key === "Enter" || e.key === " ") onMenu?.();
        }}
      >
        ⋯
      </span>
    </div>
  );
}

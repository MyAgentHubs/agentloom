import type { ChangedFile, MemberUnit } from "../types/agent";
import { useI18n, type TranslationKey } from "../i18n";
import { MessageContent } from "./MessageContent";
import { useMarkdown } from "../lib/useMarkdown";

interface TaskInspectorProps {
  member: MemberUnit;
  onClose: () => void;
  onBackToList?: () => void;
}

export function TaskInspector({
  member,
  onClose,
  onBackToList,
}: TaskInspectorProps) {
  const { t } = useI18n();
  const MarkdownBody = useMarkdown();

  function formatArtifact(files: ChangedFile[] | undefined): string {
    if (!files || files.length === 0) return "—";
    const count = files.length;
    const totalIns = files.reduce((s, f) => s + f.insertions, 0);
    const totalDel = files.reduce((s, f) => s + f.deletions, 0);
    const filesText = t("inspector.filesUnit", { n: count });
    if (totalIns + totalDel > 0) {
      return `${filesText} · +${totalIns} −${totalDel}`;
    }
    return filesText;
  }

  const statusL = (s: MemberUnit["status"]) =>
    t(("inspector.statusLabel." + s) as TranslationKey);
  const result = member.result;
  const hasOutput = member.blocks.length > 0;

  function statusIcon() {
    if (member.status === "running") {
      return (
        <span className="task-inspector__spin">
          <svg viewBox="0 0 24 24">
            <path d="M12 3a9 9 0 109 9" />
          </svg>
        </span>
      );
    }
    if (member.status === "done") {
      return <span className="task-inspector__status--done" />;
    }
    if (member.status === "failed") {
      return <span className="task-inspector__status--failed" />;
    }
    return null;
  }

  return (
    <div className="task-inspector">
      {onBackToList && (
        <button
          type="button"
          className="task-inspector__back"
          onClick={onBackToList}
        >
          {t("inspector.backToList")}
        </button>
      )}
      <button className="task-inspector__back" onClick={onClose}>
        {t("inspector.close")}
      </button>

      <div className="task-inspector__card">
        {member.sub &&
          (MarkdownBody ? (
            <div className="task-inspector__title">
              <MarkdownBody streaming={false}>{member.sub}</MarkdownBody>
            </div>
          ) : (
            <div
              className="task-inspector__title"
              style={{ whiteSpace: "pre-wrap" }}
            >
              {member.sub}
            </div>
          ))}
        <div className="task-inspector__status-line">
          {statusIcon()}
          <span>
            {t("inspector.status")}: {statusL(member.status)}
          </span>
        </div>
        <div className="task-inspector__kv">
          <span className="task-inspector__k">{t("inspector.owner")}</span>
          <span className="task-inspector__v">{member.name}</span>
          <span className="task-inspector__k">{t("inspector.artifacts")}</span>
          <span className="task-inspector__v">
            {formatArtifact(result?.changed_files)}
          </span>
        </div>
      </div>

      {member.status === "failed" && result?.failure_reason && (
        <div className="task-inspector__card">
          <h4>{t("inspector.failureReason")}</h4>
          <p className="task-inspector__failure">{result.failure_reason}</p>
          {typeof result.exit_code === "number" && (
            <p className="task-inspector__failure-exit">
              {t("memberDrillIn.exitCode", { code: result.exit_code })}
            </p>
          )}
          {result.stderr_tail && (
            <div className="task-inspector__card--raw">
              <details>
                <summary>{t("inspector.stderrTail")}</summary>
                <pre className="task-inspector__trace-block">
                  {result.stderr_tail}
                </pre>
              </details>
            </div>
          )}
        </div>
      )}

      <div className="task-inspector__card">
        <h4>{t("inspector.toolTrace")}</h4>
        {hasOutput ? (
          <MessageContent blocks={member.blocks} />
        ) : (
          <p className="task-inspector__empty">{t("inspector.noOutput")}</p>
        )}
      </div>
    </div>
  );
}

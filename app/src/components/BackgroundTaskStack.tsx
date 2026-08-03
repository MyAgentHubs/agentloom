import { memo } from "react";
import type { MemberUnit } from "../types/agent";
import { taskRowView } from "../lib/taskStatus";
import { isTeamRunComplete } from "../lib/teamReducer";
import { useI18n } from "../i18n";

function TaskRow({
  runId,
  m,
  onOpenMember,
}: {
  runId: string;
  m: MemberUnit;
  onOpenMember?: (r: string, a: string) => void;
}) {
  const { t } = useI18n();
  const v = taskRowView(m);
  // KISS 一行 bar：worker 名 + 进展/子任务（灰·截断）+ scoped 状态徽标；整行点开右面板明细。
  const name =
    typeof v.name === "string" ? v.name : t(v.name.key, v.name.values);
  const secondary =
    v.rawProgress ?? (v.progress ? t(v.progress) : null) ?? name;
  const label = t(v.label);
  return (
    <div
      className={`task-row st-${v.dotClass}`}
      role="button"
      tabIndex={0}
      title={label}
      onClick={() => onOpenMember?.(runId, m.assignment_id)}
      onKeyDown={(e) => {
        if (e.key === "Enter") onOpenMember?.(runId, m.assignment_id);
      }}
    >
      <div className="tbody">
        <span className="tnm">{m.name}</span>
        {secondary && <span className="tprog">{secondary}</span>}
      </div>
      <span className={`task-badge st-${v.dotClass}`}>{label}</span>
    </div>
  );
}
const MemoRow = memo(TaskRow);

/** 后台任务条（块 B·取代 TeamRunBlock 主区 LiveStreamCard 循环）。lead 叙事壳全空（§7.1-B P1-5）。
 * 行 memo 防 running 每 tick 重渲卡死（spec 强制·先例 MemberDrillIn）。点行→右面板 Task Inspector。 */
function BackgroundTaskStackImpl({
  runId,
  members,
  onOpenMember,
  onUndoRun,
}: {
  runId: string;
  lead?: string | null;
  members: MemberUnit[];
  onOpenMember?: (runId: string, assignmentId: string) => void;
  onUndoRun?: (runId: string) => void;
}) {
  const { t } = useI18n();
  if (members.length === 0) return null;
  const completed = isTeamRunComplete({
    run_id: runId,
    goal: null,
    lead: null,
    members,
  });
  return (
    <div className="taskstack">
      {members.map((m) => (
        <MemoRow
          key={m.assignment_id}
          runId={runId}
          m={m}
          onOpenMember={onOpenMember}
        />
      ))}
      {completed && (
        <footer className="pf-row">
          <span className="pf-sp" />
          <button
            type="button"
            className="pf-view"
            onClick={() => onUndoRun?.(runId)}
          >
            {t("taskStack.undoRun")}
          </button>
        </footer>
      )}
    </div>
  );
}
export const BackgroundTaskStack = memo(BackgroundTaskStackImpl);

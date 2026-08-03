import { useState } from "react";
import type { GateAction } from "../lib/gateReducer";
import type { ParsedAssignment } from "../types/gate";
import type { AgentProfile } from "../types/agent";
import { useI18n } from "../i18n";

type Props = {
  assignments: ParsedAssignment[];
  autoDispatch: boolean;
  enabledAgents: AgentProfile[];
  onAction: (a: GateAction) => void;
};

export function AssignmentEditor({
  assignments,
  autoDispatch,
  enabledAgents,
  onAction,
}: Props) {
  const { t } = useI18n();
  const [openId, setOpenId] = useState<string | null>(null);

  return (
    <div className="assign">
      <div className="assign__head">
        <span className="assign__t">{t("assignmentEditor.title")}</span>
        <button
          type="button"
          role="switch"
          aria-checked={autoDispatch}
          aria-label={t("assignmentEditor.autoDispatch")}
          className={`assign__sw${autoDispatch ? " is-on" : ""}`}
          onClick={() => onAction({ type: "toggleAutoDispatch" })}
        >
          {t("assignmentEditor.autoDispatch")}
        </button>
      </div>

      {assignments.map((s) => (
        <div className="assign__row" key={s.subtaskId}>
          <div className="assign__task">
            <div className="assign__d">{s.subtask}</div>
            {s.scopeFiles.length > 0 && (
              <div className="assign__f">{s.scopeFiles.join(" · ")}</div>
            )}
          </div>
          <div className="assign__chipwrap">
            <button
              type="button"
              className="assign__chip"
              aria-label={t("assignmentEditor.reassignAria")}
              onClick={() =>
                setOpenId((p) => (p === s.subtaskId ? null : s.subtaskId))
              }
            >
              {s.assignee ? (
                <>
                  <span className="assign__a" aria-hidden>
                    {s.assignee.provider.slice(0, 1).toUpperCase()}
                  </span>
                  <span className="assign__nm">{s.assignee.provider}</span>
                </>
              ) : (
                <span className="assign__nm assign__nm--none">
                  {t("assignmentEditor.unassigned")} ▾
                </span>
              )}
              <span className="assign__cv" aria-hidden>
                ▾
              </span>
            </button>
            {openId === s.subtaskId && (
              <div className="assign__dd">
                <div className="assign__dh">
                  {t("assignmentEditor.reassignTo")}
                </div>
                {enabledAgents.map((ag) => (
                  <button
                    type="button"
                    key={ag.id}
                    className={`assign__di${s.assignee?.agentId === ag.id ? " is-sel" : ""}`}
                    onClick={() => {
                      onAction({
                        type: "reassign",
                        subtaskId: s.subtaskId,
                        assignee: {
                          agentId: ag.id,
                          provider: ag.provider,
                          model: ag.primary_model ?? "",
                        },
                      });
                      setOpenId(null);
                    }}
                  >
                    <span className="assign__a" aria-hidden>
                      {ag.provider.slice(0, 1).toUpperCase()}
                    </span>
                    {ag.name}
                    {s.assignee?.agentId === ag.id && (
                      <span className="assign__ck" aria-hidden>
                        ✓
                      </span>
                    )}
                  </button>
                ))}
                <div className="assign__note">
                  {t("assignmentEditor.availabilityNote")}
                </div>
              </div>
            )}
          </div>
          <button
            type="button"
            className="assign__rm"
            aria-label={t("assignmentEditor.removeMember")}
            onClick={() =>
              onAction({ type: "removeAssignment", subtaskId: s.subtaskId })
            }
          >
            ✕
          </button>
        </div>
      ))}

      <div className="assign__foot">
        <button
          type="button"
          className="assign__addb"
          onClick={() => onAction({ type: "addAssignment" })}
        >
          {t("assignmentEditor.addTask")}
        </button>
        <span className="assign__lead">
          {t("assignmentEditor.leadValidationNote")}
        </span>
      </div>
    </div>
  );
}

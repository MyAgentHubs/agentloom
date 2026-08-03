import { useEffect, useState } from "react";
import { useDropdown } from "../hooks/useDropdown";
import { useI18n } from "../i18n";
import { hasLeadCapability } from "../lib/agentCapabilities";
import type { AgentProfile } from "../types/agent";
import { AgentAvatar } from "./AgentAvatar";

type Props = {
  agents?: AgentProfile[];
  agentId: string;
  leadId: string | null;
  memberIds: string[];
  onAgentChange: (id: string) => void;
  onSetLead: (id: string | null, memberIds?: string[]) => void;
  onToggleMember: (id: string) => void;
  onMenuAgents?: () => void;
  teamMode?: boolean;
  disabled?: boolean;
  loading?: boolean;
  saving?: boolean;
};

type TeamDraft = {
  leadId: string | null;
  memberIds: string[];
  sawSaving: boolean;
};

function sortEnabledAgents(agents: AgentProfile[]): AgentProfile[] {
  return agents
    .filter((agent) => agent.enabled)
    .sort(
      (a, b) =>
        a.name.localeCompare(b.name) ||
        a.id.localeCompare(b.id) ||
        a.sort_order - b.sort_order,
    );
}

function orderMenuAgents(
  agents: AgentProfile[],
  leadId: string | null,
  memberIds: string[],
): AgentProfile[] {
  if (leadId === null) return agents;

  const byId = new Map(agents.map((agent) => [agent.id, agent]));
  const orderedIds = new Set<string>();
  const out: AgentProfile[] = [];
  const push = (id: string) => {
    const agent = byId.get(id);
    if (!agent || orderedIds.has(id)) return;
    orderedIds.add(id);
    out.push(agent);
  };

  push(leadId);
  for (const id of memberIds) {
    if (id !== leadId) push(id);
  }
  for (const agent of agents) {
    push(agent.id);
  }
  return out;
}

function avatarKind(agent: AgentProfile | undefined, fallback: string): string {
  return agent?.provider || agent?.id || fallback || "agent";
}

function triggerAriaLabel(
  t: ReturnType<typeof useI18n>["t"],
  inTeam: boolean,
  name: string,
  memberCount: number,
  loading: boolean,
): string {
  const loadingSuffix = loading
    ? t("composer.agentSelector.loadingSuffix")
    : "";
  if (inTeam) {
    return t("composer.agentSelector.trigger.team", {
      name,
      count: memberCount,
      loading: loadingSuffix,
    });
  }
  return t("composer.agentSelector.trigger.solo", {
    name,
    loading: loadingSuffix,
  });
}

function agentDescription(
  t: ReturnType<typeof useI18n>["t"],
  agent: AgentProfile,
  canLead: boolean,
): string {
  const provider = agent.provider || agent.access || "Agent";
  return canLead
    ? t("composer.agentSelector.description.canLead", { provider })
    : t("composer.agentSelector.description.unavailable", { provider });
}

function sameMemberSet(a: string[], b: string[]): boolean {
  if (a.length !== b.length) return false;
  const ids = new Set(a);
  return b.every((id) => ids.has(id));
}

function CrownIcon() {
  return (
    <svg viewBox="0 0 24 24" aria-hidden="true">
      <path d="M3 7l4 5 5-7 5 7 4-5v11H3z" />
    </svg>
  );
}

export function ComposerAgentSelector({
  agents = [],
  agentId,
  leadId,
  memberIds,
  onAgentChange,
  onSetLead,
  onToggleMember,
  onMenuAgents,
  teamMode,
  disabled = false,
  loading = false,
  saving = false,
}: Props) {
  const { t } = useI18n();
  const dd = useDropdown();
  const [optimisticTeam, setOptimisticTeam] = useState<TeamDraft | null>(null);
  const hasTeamDraft = optimisticTeam !== null;
  const viewLeadId = hasTeamDraft ? optimisticTeam.leadId : leadId;
  const viewMemberIds = hasTeamDraft ? optimisticTeam.memberIds : memberIds;
  const enabledAgents = sortEnabledAgents([...agents]);
  const menuAgents = orderMenuAgents(enabledAgents, viewLeadId, viewMemberIds);
  const enabledIds = new Set(enabledAgents.map((agent) => agent.id));
  const currentAgent =
    enabledAgents.find((agent) => agent.id === agentId) ?? enabledAgents[0];
  const leadAgent =
    viewLeadId === null
      ? undefined
      : enabledAgents.find((agent) => agent.id === viewLeadId);
  const inTeam = viewLeadId !== null;
  const configuringTeam = inTeam || teamMode === true;
  const actionDisabled = disabled || loading;
  const memberCount = viewMemberIds.filter(
    (id) => enabledIds.has(id) && id !== viewLeadId,
  ).length;
  const triggerAgent = inTeam ? leadAgent : currentAgent;
  const triggerName = loading
    ? "…"
    : inTeam
      ? (leadAgent?.name ?? viewLeadId ?? "Agent")
      : (currentAgent?.name ?? "Agent");
  const triggerStatusName = inTeam
    ? (leadAgent?.name ?? viewLeadId ?? "Agent")
    : (currentAgent?.name ?? "Agent");
  const triggerLabel = triggerAriaLabel(
    t,
    inTeam,
    triggerStatusName,
    memberCount,
    loading,
  );

  useEffect(() => {
    if (!optimisticTeam) return;
    const matchesProps =
      leadId === optimisticTeam.leadId &&
      sameMemberSet(memberIds, optimisticTeam.memberIds);
    if (matchesProps || (optimisticTeam.sawSaving && !saving)) {
      setOptimisticTeam(null);
    } else if (saving && !optimisticTeam.sawSaving) {
      setOptimisticTeam({ ...optimisticTeam, sawSaving: true });
    }
  }, [leadId, memberIds, optimisticTeam, saving]);

  return (
    <div className="cas" ref={dd.containerRef}>
      <button
        type="button"
        className={`cas-btn${inTeam ? " cas-btn--team" : ""}`}
        aria-label={triggerLabel}
        {...dd.triggerProps}
        disabled={actionDisabled}
        onClick={dd.toggle}
      >
        {inTeam && (
          <span className="cas-btn__crown" aria-hidden="true">
            <CrownIcon />
          </span>
        )}
        <AgentAvatar kind={avatarKind(triggerAgent, agentId)} />
        {inTeam ? (
          <>
            <span className="cas-btn__role">
              {t("composer.agentSelector.role.lead")}
            </span>
            <span className="cas-btn__name">{triggerName}</span>
            <span className="cas-btn__members">
              {t("composer.agentSelector.members.count", {
                count: memberCount,
              })}
            </span>
          </>
        ) : (
          <span className="cas-btn__name">{triggerName}</span>
        )}
        <span className="cas-btn__chev">▾</span>
      </button>

      {dd.open && (
        <div
          className={`cas-pop${configuringTeam ? " cas-pop--team" : ""}`}
          role="menu"
        >
          <div className="cas-title">
            {configuringTeam
              ? t("composer.agentSelector.title.team")
              : t("composer.agentSelector.title.solo")}
          </div>
          {inTeam && (
            <>
              <div
                className="cas-auto is-disabled"
                title={t("composer.agentSelector.auto.teamUnavailableTitle")}
                aria-disabled="true"
              >
                <span className="cas-auto__icon" aria-hidden="true">
                  ✦
                </span>
                <span className="cas-main__text">
                  <span className="cas-main__name">Auto</span>
                  <span className="cas-main__desc">
                    {t("composer.agentSelector.auto.teamUnavailable")}
                  </span>
                </span>
              </div>
              <div className="cas-div" />
            </>
          )}
          {enabledAgents.length === 0 ? (
            <div className="cas-empty">{t("composer.agentSelector.empty")}</div>
          ) : (
            <div className="cas-list">
              {menuAgents.map((agent) => {
                const isLead = inTeam && agent.id === viewLeadId;
                const canLead = hasLeadCapability(agent);
                const selected = inTeam
                  ? isLead
                  : agent.id === currentAgent?.id;
                const rowDisabled = actionDisabled || inTeam;
                const crownDisabled = actionDisabled || (!canLead && !isLead);

                return (
                  <div
                    key={agent.id}
                    className={`cas-row${
                      selected ? " is-selected" : ""
                    }${rowDisabled ? " is-row-disabled" : ""}`}
                    role="none"
                  >
                    <button
                      type="button"
                      role="menuitemradio"
                      aria-label={agent.name}
                      aria-checked={selected}
                      className="cas-main"
                      disabled={rowDisabled}
                      onClick={() => {
                        if (rowDisabled) return;
                        onAgentChange(agent.id);
                        dd.close();
                      }}
                    >
                      <AgentAvatar kind={avatarKind(agent, agent.id)} />
                      <span className="cas-main__text">
                        <span className="cas-main__name">{agent.name}</span>
                        {inTeam && (
                          <span className="cas-main__desc">
                            {agentDescription(t, agent, canLead)}
                          </span>
                        )}
                      </span>
                    </button>
                    <span className="cas-ctl">
                      {canLead || inTeam ? (
                        <button
                          type="button"
                          className={`cas-lead-star${
                            isLead ? " is-on" : ""
                          }${!canLead ? " is-disabled" : ""}`}
                          aria-label={
                            isLead
                              ? t("composer.agentSelector.action.cancelLead", {
                                  name: agent.name,
                                })
                              : !canLead
                                ? t("lead.crown.disabledTip")
                                : t("composer.agentSelector.action.setLead", {
                                    name: agent.name,
                                  })
                          }
                          aria-pressed={isLead}
                          title={
                            !canLead && !isLead
                              ? t("lead.crown.disabledTip")
                              : undefined
                          }
                          disabled={crownDisabled}
                          onClick={(event) => {
                            event.stopPropagation();
                            if (actionDisabled) return;
                            if (isLead) {
                              setOptimisticTeam({
                                leadId: null,
                                memberIds: [],
                                sawSaving: false,
                              });
                              onSetLead(null);
                              dd.close();
                              return;
                            }
                            if (!canLead) return;
                            const baseMemberIds =
                              !inTeam && viewMemberIds.length === 0
                                ? enabledAgents.map((enabled) => enabled.id)
                                : viewMemberIds;
                            const nextMemberIds = baseMemberIds.filter(
                              (id) => id !== agent.id,
                            );
                            setOptimisticTeam({
                              leadId: agent.id,
                              memberIds: nextMemberIds,
                              sawSaving: false,
                            });
                            dd.setOpen(true);
                            onSetLead(agent.id, nextMemberIds);
                          }}
                        >
                          <CrownIcon />
                        </button>
                      ) : null}
                      {inTeam && isLead && (
                        <span className="cas-meta">
                          {t("composer.agentSelector.role.lead")}
                        </span>
                      )}
                      {inTeam && !isLead && (
                        <button
                          type="button"
                          className={`cas-mtoggle${
                            viewMemberIds.includes(agent.id) ? " is-on" : ""
                          }`}
                          aria-label={t("composer.agentSelector.memberAria", {
                            name: agent.name,
                          })}
                          aria-pressed={viewMemberIds.includes(agent.id)}
                          disabled={actionDisabled}
                          onClick={(event) => {
                            event.stopPropagation();
                            if (actionDisabled) return;
                            setOptimisticTeam({
                              leadId: viewLeadId,
                              memberIds: viewMemberIds.includes(agent.id)
                                ? viewMemberIds.filter((id) => id !== agent.id)
                                : [...viewMemberIds, agent.id],
                              sawSaving: false,
                            });
                            onToggleMember(agent.id);
                          }}
                        />
                      )}
                    </span>
                  </div>
                );
              })}
            </div>
          )}
          {inTeam && (
            <div className="cas-foot">{t("composer.agentSelector.foot")}</div>
          )}
          <button
            type="button"
            className="cas-manage"
            disabled={actionDisabled}
            onClick={() => {
              if (actionDisabled) return;
              onMenuAgents?.();
              dd.close();
            }}
          >
            {t("composer.agentSelector.manage")}
          </button>
        </div>
      )}
    </div>
  );
}

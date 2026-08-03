import { useDropdown } from "../hooks/useDropdown";
import type { AgentProfile } from "../types/agent";
import { useI18n } from "../i18n";
import { AgentAvatar } from "./AgentAvatar";

type Props = {
  agents?: AgentProfile[];
  agentId: string;
  onAgentChange: (id: string) => void;
  onMenuAgents?: () => void;
  label?: string;
  disabled?: boolean;
  loading?: boolean;
  disabledIds?: string[];
};

function sortAgents(agents: AgentProfile[]): AgentProfile[] {
  return agents
    .filter((agent) => agent.enabled)
    .sort((a, b) => a.sort_order - b.sort_order || a.id.localeCompare(b.id));
}

export function AgentDropdown({
  agents = [],
  agentId,
  onAgentChange,
  onMenuAgents,
  label,
  disabled,
  loading = false,
  disabledIds,
}: Props) {
  const { t } = useI18n();
  const dd = useDropdown();
  const dynamicAgents = sortAgents([...agents]);
  const showLoading = loading;

  let triggerName: string;
  let triggerAvatarKind: string;
  if (showLoading) {
    triggerName = "…";
    triggerAvatarKind = "agent";
  } else {
    const cur = dynamicAgents.find((x) => x.id === agentId) ?? dynamicAgents[0];
    triggerName = cur?.name ?? "Agent";
    triggerAvatarKind = cur?.provider || cur?.id || agentId || "agent";
  }

  return (
    <div className="dd" ref={dd.containerRef}>
      <button
        type="button"
        className="composer__agent"
        aria-label={t("agentDropdown.selectAria")}
        {...dd.triggerProps}
        disabled={disabled || showLoading}
        onClick={dd.toggle}
      >
        <span className="composer__agent-pre">{label ?? "AGENT"}</span>
        <AgentAvatar kind={triggerAvatarKind} />
        {triggerName}
        <span className="composer__agent-chev">▾</span>
      </button>
      {dd.open && (
        <div className="dd__menu" role="menu">
          <div className="dd__h">{t("agentDropdown.title")}</div>
          {dynamicAgents.length === 0 ? (
            <div className="dd__empty">{t("agentDropdown.empty")}</div>
          ) : (
            dynamicAgents.map((agent) => {
              const selected = agent.id === agentId;
              const itemDisabled = disabledIds?.includes(agent.id) ?? false;
              return (
                <button
                  key={agent.id}
                  type="button"
                  role="menuitemradio"
                  aria-checked={selected}
                  disabled={itemDisabled}
                  className={`dd__item dd__item--agent${
                    selected ? " dd__item--on" : ""
                  }${itemDisabled ? " dd__item--disabled" : ""}`}
                  onClick={() => {
                    onAgentChange(agent.id);
                    dd.close();
                  }}
                >
                  <AgentAvatar kind={agent.provider || agent.id} />
                  <span className="dd__item-name">{agent.name}</span>
                  {selected && <span className="dd__check">✓</span>}
                </button>
              );
            })
          )}
          <button
            type="button"
            className="dd__item dd__foot-action"
            onClick={() => {
              onMenuAgents?.();
              dd.close();
            }}
          >
            {t("agentDropdown.manage")}
          </button>
        </div>
      )}
    </div>
  );
}

import { useState, useRef, useEffect } from "react";
import type { AgentProfile } from "../types/agent";
import { hasLeadCapability } from "../lib/agentCapabilities";
import { useI18n, type I18nKey } from "../i18n";

type Props = {
  agents: AgentProfile[]; // enabled agents
  leadId: string; // 解析后的当前 Lead（调用方已回退全局）
  rosterIds: string[] | null; // null = 全 enabled
  onSetLead: (id: string) => void;
  onToggleRoster: (id: string, allEnabledIds: string[]) => void; // 全集上下文·见 T4
  runningCount: number | null; // null = 未运行·显名单数
};

export function canBeLead(a: AgentProfile): boolean {
  return hasLeadCapability(a);
}

// §8.6 能力 hint = 公开知识常识表（推荐≠强制）。未知 provider 不显 hint。
const CAP_HINT_KEYS: Partial<Record<string, I18nKey>> = {
  claude: "teamBar.capHint.claude",
  codex: "teamBar.capHint.codex",
  gemini: "teamBar.capHint.gemini",
  kimi: "teamBar.capHint.kimi",
  deepseek: "teamBar.capHint.deepseek",
};

export function TeamBar({
  agents,
  leadId,
  rosterIds,
  onSetLead,
  onToggleRoster,
  runningCount,
}: Props) {
  const { t } = useI18n();
  const [open, setOpen] = useState(false);
  const rootRef = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (!open) return;
    function handleMouseDown(e: MouseEvent) {
      if (rootRef.current && !rootRef.current.contains(e.target as Node)) {
        setOpen(false);
      }
    }
    document.addEventListener("mousedown", handleMouseDown);
    return () => document.removeEventListener("mousedown", handleMouseDown);
  }, [open]);
  const lead = agents.find((a) => a.id === leadId);
  const inRoster = (id: string) => rosterIds === null || rosterIds.includes(id);
  const memberCount = rosterIds === null ? agents.length : rosterIds.length;

  return (
    <div className="team-bar" ref={rootRef}>
      <button
        type="button"
        className="team-bar__row"
        aria-label={open ? t("teamBar.collapse") : t("teamBar.expand")}
        onClick={() => setOpen((v) => !v)}
      >
        <span className="team-bar__seg">
          {t("teamBar.roleLead")} · <b>{lead?.name ?? leadId}</b>
        </span>
        <span className="team-bar__sep" />
        <span className="team-bar__seg">
          {runningCount !== null
            ? t("teamBar.barRunning", {
                running: runningCount,
                total: memberCount,
              })
            : t("teamBar.barMembers", { n: memberCount })}
        </span>
        <span className="team-bar__chev">{open ? "▴" : "▾"}</span>
      </button>

      {open && (
        <div className="team-bar__panel">
          <div className="team-bar__title">{t("teamBar.panelTitle")}</div>
          <div className="team-bar__h">{t("teamBar.leadHead")}</div>
          <div className="team-bar__leads">
            {agents.map((a) => {
              const ok = canBeLead(a);
              return (
                <button
                  key={a.id}
                  type="button"
                  className={`team-bar__lead${a.id === leadId ? " is-sel" : ""}`}
                  disabled={!ok}
                  aria-pressed={a.id === leadId}
                  onClick={() => ok && onSetLead(a.id)}
                >
                  <span className="team-bar__nm">{a.name}</span>
                  {!ok && (
                    <span className="team-bar__why">
                      {t("teamBar.leadCantBe")}
                    </span>
                  )}
                </button>
              );
            })}
          </div>

          <div className="team-bar__h">{t("teamBar.rosterHead")}</div>
          <div className="team-bar__roster">
            {agents.map((a) => {
              const hintKey = CAP_HINT_KEYS[a.provider];
              return (
                <label
                  key={a.id}
                  className="team-bar__pick"
                  aria-label={t("teamBar.memberAria", { name: a.name })}
                >
                  <input
                    type="checkbox"
                    checked={inRoster(a.id)}
                    onChange={() =>
                      onToggleRoster(
                        a.id,
                        agents.map((x) => x.id),
                      )
                    }
                  />
                  <span className="team-bar__nm">{a.name}</span>
                  {hintKey && (
                    <span className="team-bar__hint">{t(hintKey)}</span>
                  )}
                </label>
              );
            })}
          </div>
        </div>
      )}
    </div>
  );
}

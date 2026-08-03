import { useCallback, useEffect, useMemo, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useI18n } from "../../i18n";
import type { CSSProperties } from "react";
import type { RuntimeDetect } from "../../lib/agentAvailability";
import type { AgentProfile } from "../../types/agent";
import { AgentAvatar } from "../AgentAvatar";
import { AgentForm } from "./AgentForm";

const styles = {
  header: {
    justifyContent: "space-between",
    marginBottom: 12,
  },
  headerCopy: {
    display: "flex",
    flexDirection: "column",
    gap: 2,
    minWidth: 0,
  },
  list: {
    display: "flex",
    flexDirection: "column",
    gap: 7,
    listStyle: "none",
    margin: 0,
    padding: 0,
    maxWidth: 760,
  },
  row: {
    alignItems: "center",
    display: "flex",
    gap: 11,
    padding: "10px 12px",
  },
  body: {
    flex: 1,
    minWidth: 0,
  },
  name: {
    color: "var(--ink)",
    fontSize: 13,
    fontWeight: 600,
    overflow: "hidden",
    textOverflow: "ellipsis",
    whiteSpace: "nowrap",
  },
  meta: {
    alignItems: "center",
    color: "var(--ink-3)",
    display: "flex",
    flexWrap: "wrap",
    fontSize: 10.5,
    gap: 8,
    marginTop: 2,
  },
  description: {
    color: "var(--ink-3)",
    fontSize: 11.5,
    lineHeight: 1.5,
    marginTop: 3,
    maxWidth: 640,
  },
  model: {
    color: "var(--ink-2)",
  },
  keyState: {
    alignItems: "center",
    color: "var(--ink-3)",
    display: "inline-flex",
    gap: 5,
  },
  keyDot: {
    borderRadius: "50%",
    height: 7,
    width: 7,
  },
  keyDotReady: {
    background: "var(--green)",
    boxShadow: "0 0 0 2px rgba(106, 155, 92, 0.13)",
  },
  keyDotMissing: {
    background: "var(--ink-4)",
  },
  actions: {
    alignItems: "center",
    display: "flex",
    gap: 6,
    marginLeft: 8,
  },
  autoDetect: {
    alignItems: "center",
    background: "var(--bg-sunken)",
    border: "1px solid var(--line-soft)",
    borderRadius: 5,
    color: "var(--ink-3)",
    display: "inline-flex",
    fontSize: 10,
    lineHeight: 1,
    padding: "4px 8px",
    whiteSpace: "nowrap",
  },
  empty: {
    color: "var(--ink-3)",
    fontSize: 12,
    padding: "18px 0",
  },
} satisfies Record<string, CSSProperties>;

type Translator = ReturnType<typeof useI18n>["t"];

function accessLabel(access: string, t: Translator): string {
  if (access === "borrow") return t("settings.agentAccess.borrow");
  if (access === "harness") return t("settings.agentAccess.harness");
  return t("settings.agentAccess.native");
}

function accessChipClass(access: string): "cc" | "harness" | "native" {
  if (access === "borrow") return "cc";
  if (access === "harness") return "harness";
  return "native";
}

function providerModel(agent: AgentProfile, t: Translator): string {
  const model = agent.primary_model?.trim();
  if (agent.provider && model) return `${agent.provider} · ${model}`;
  if (agent.provider) return agent.provider;
  return model || t("settings.agents.providerModel.unset");
}

const agentNameCollator = new Intl.Collator("en", {
  sensitivity: "base",
  numeric: true,
});

function compareRaw(a: string, b: string): number {
  return a < b ? -1 : a > b ? 1 : 0;
}

function sortAgents(agents: AgentProfile[]): AgentProfile[] {
  return [...agents].sort((a, b) => {
    return (
      agentNameCollator.compare(a.name, b.name) ||
      agentNameCollator.compare(a.provider, b.provider) ||
      agentNameCollator.compare(a.id, b.id) ||
      compareRaw(a.name, b.name) ||
      compareRaw(a.provider, b.provider) ||
      compareRaw(a.id, b.id)
    );
  });
}

type SettingsAgentsProps = {
  onAgentsChanged?: () => void;
  runtimeDetect?: RuntimeDetect;
};

export function SettingsAgents({
  onAgentsChanged,
  runtimeDetect,
}: SettingsAgentsProps = {}) {
  const { t } = useI18n();
  const [agents, setAgents] = useState<AgentProfile[]>([]);
  const [loading, setLoading] = useState(true);
  const [deletingId, setDeletingId] = useState<string | null>(null);
  const [formAgent, setFormAgent] = useState<AgentProfile | null>(null);
  const [formOpen, setFormOpen] = useState(false);

  const sortedAgents = useMemo(() => sortAgents(agents), [agents]);
  const nextSortOrder = useMemo(
    () =>
      sortedAgents.reduce(
        (maxSortOrder, agent) => Math.max(maxSortOrder, agent.sort_order),
        -1,
      ) + 1,
    [sortedAgents],
  );

  const loadAgents = useCallback(async () => {
    setLoading(true);
    try {
      const next = await invoke<AgentProfile[]>("list_agents");
      setAgents(Array.isArray(next) ? next : []);
    } catch {
      setAgents([]);
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void loadAgents();
  }, [loadAgents]);

  async function deleteAgent(agent: AgentProfile) {
    if (agent.access === "native" || deletingId) return;
    setDeletingId(agent.id);
    try {
      await invoke("delete_agent", { id: agent.id });
      await loadAgents();
      onAgentsChanged?.();
    } catch {
      await loadAgents();
    } finally {
      setDeletingId(null);
    }
  }

  function openCreateForm() {
    setFormAgent(null);
    setFormOpen(true);
  }

  function openEditForm(agent: AgentProfile) {
    setFormAgent(agent);
    setFormOpen(true);
  }

  async function refreshAfterSave() {
    setFormOpen(false);
    setFormAgent(null);
    await loadAgents();
    onAgentsChanged?.();
  }

  return (
    <>
      <div className="ob-disc-h" style={styles.header}>
        <div style={styles.headerCopy}>
          <span className="t">{t("settings.nav.agents")}</span>
          <span className="cnt">
            {t("settings.agents.configuredCount", {
              n: sortedAgents.length,
            })}
          </span>
          <span style={styles.description}>
            {t("settings.agents.description")}
          </span>
        </div>
        <button
          type="button"
          className="ob-btn primary"
          onClick={openCreateForm}
        >
          {t("settings.agents.add")}
        </button>
      </div>

      {formOpen ? (
        <AgentForm
          key={formAgent?.id ?? "new-agent"}
          agent={formAgent}
          nextSortOrder={nextSortOrder}
          onCancel={() => setFormOpen(false)}
          onSaved={refreshAfterSave}
        />
      ) : null}

      {loading ? (
        <div className="ob-sk line w2" />
      ) : sortedAgents.length === 0 ? (
        <div style={styles.empty}>{t("settings.agents.empty")}</div>
      ) : (
        <ul style={styles.list} aria-label={t("settings.agents.listAria")}>
          {sortedAgents.map((agent) => {
            const label = accessLabel(agent.access, t);
            const nativeRuntimeAvailable =
              runtimeDetect?.[agent.provider] === true;
            const stateReady =
              agent.access === "native"
                ? nativeRuntimeAvailable
                : agent.has_key;
            return (
              <li
                key={agent.id}
                className="st-agent"
                data-testid={`agent-row-${agent.id}`}
                style={styles.row}
              >
                <AgentAvatar kind={agent.provider || agent.id} />
                <div style={styles.body}>
                  <div style={styles.name}>{agent.name}</div>
                  <div style={styles.meta}>
                    <span
                      className={`st-agent-chip ${accessChipClass(agent.access)}`}
                    >
                      {label}
                    </span>
                    <span style={styles.model}>{providerModel(agent, t)}</span>
                    {agent.access !== "native" ||
                    runtimeDetect !== undefined ? (
                      <span style={styles.keyState}>
                        <span
                          aria-hidden="true"
                          style={{
                            ...styles.keyDot,
                            ...(stateReady
                              ? styles.keyDotReady
                              : styles.keyDotMissing),
                          }}
                        />
                        {agent.access === "native"
                          ? nativeRuntimeAvailable
                            ? t("settings.agentKeyState.detected")
                            : t("settings.agentKeyState.notInstalled")
                          : agent.has_key
                            ? t("settings.agentKeyState.configured")
                            : t("settings.agentKeyState.missing")}
                      </span>
                    ) : null}
                  </div>
                </div>
                <div style={styles.actions}>
                  {agent.access === "native" ? (
                    <>
                      <span
                        style={styles.autoDetect}
                        title={t("settings.agents.nativeAutoDetectTitle")}
                      >
                        {t("settings.agents.nativeAutoDetect")}
                      </span>
                      <button
                        type="button"
                        className="ob-btn"
                        onClick={() => openEditForm(agent)}
                      >
                        {t("settings.agents.edit")}
                      </button>
                    </>
                  ) : (
                    <>
                      <button
                        type="button"
                        className="ob-btn"
                        onClick={() => openEditForm(agent)}
                      >
                        {t("settings.agents.edit")}
                      </button>
                      <button
                        type="button"
                        className="ob-btn"
                        aria-label={t("settings.agents.deleteAria", {
                          name: agent.name,
                        })}
                        disabled={deletingId === agent.id}
                        onClick={() => void deleteAgent(agent)}
                      >
                        {t("settings.agents.delete")}
                      </button>
                    </>
                  )}
                </div>
              </li>
            );
          })}
        </ul>
      )}
    </>
  );
}

import { useState } from "react";
import { InputArea } from "./InputArea";
import { RepoDocumentPanel } from "./RepoDocumentPanel";
import type {
  AgentProfile,
  ComposerRuntimeConfig,
  RepoMeta,
} from "../types/agent";
import { useI18n } from "../i18n";
import type { Mode } from "./ModeDropdown";

type Props = {
  activeRepo: RepoMeta | null;
  composerBusy?: boolean;
  running?: boolean;
  agents?: AgentProfile[];
  agentId?: string;
  onAgentChange?: (agentId: string) => void;
  onMenuAgents?: () => void;
  mode?: Mode;
  onModeChange?: (m: Mode) => void;
  onSend?: (text: string, mode: Mode, config?: ComposerRuntimeConfig) => void;
  onStop?: () => void;
  canSend?: boolean;
  teamLeadId?: string | null;
  rosterIds?: string[] | null;
  onSetLead?: (id: string | null, memberIds?: string[]) => void;
  onToggleRoster?: (id: string, allEnabledIds: string[]) => void;
};

const noop = () => {};

/**
 * cluster L plan 2a · 项目简介全屏页（view='intro'）
 * 真相源：c1-repo-intro.html line 73-141 layout（不真实现 README 渲染 / lightbox / AI 解析 · spec §9 out of scope）
 * - 关联项目 → 显项目名 + path + 项目简报 / Daily tab
 * - 默认 session（无关联项目）→ 显默认会话页头，文档生成保持禁用
 */
export function ProjectIntroPage({
  activeRepo,
  composerBusy = true,
  running = false,
  agents,
  agentId = "claude",
  onAgentChange = noop,
  onMenuAgents,
  mode = "normal",
  onModeChange = noop,
  onSend,
  onStop = noop,
  canSend,
  teamLeadId,
  rosterIds,
  onSetLead,
  onToggleRoster,
}: Props) {
  const { t } = useI18n();
  const [activeTab, setActiveTab] = useState<"intro" | "daily">("intro");
  const input = (
    <InputArea
      composerBusy={composerBusy || !onSend}
      running={running}
      memberRunning={false}
      agents={agents}
      agentId={agentId}
      onAgentChange={onAgentChange}
      onMenuAgents={onMenuAgents}
      sessionId={null}
      mode={mode}
      onModeChange={onModeChange}
      onSend={onSend ?? noop}
      onStop={onStop}
      canSend={canSend}
      teamLeadId={teamLeadId}
      rosterIds={rosterIds}
      onSetLead={onSetLead}
      onToggleRoster={onToggleRoster}
    />
  );

  return (
    <main className="session">
      <section className="intro">
        <div className="intro__inner">
          <div className="intro__head">
            <h1 className="intro__title">
              {activeRepo?.name ?? t("projectIntro.defaultSession")}
            </h1>
            <div
              className="intro__tabs"
              role="tablist"
              aria-label={t("projectIntro.tabsAria")}
            >
              <button
                type="button"
                role="tab"
                aria-selected={activeTab === "intro"}
                className={`intro__tab${activeTab === "intro" ? " intro__tab--on" : ""}`}
                onClick={() => setActiveTab("intro")}
              >
                {t("projectIntro.tabIntro")}
              </button>
              <button
                type="button"
                role="tab"
                aria-selected={activeTab === "daily"}
                className={`intro__tab${activeTab === "daily" ? " intro__tab--on" : ""}`}
                onClick={() => setActiveTab("daily")}
              >
                {t("projectIntro.tabDaily")}
              </button>
            </div>
          </div>
          <p className="intro__path">
            {activeRepo?.path ?? t("projectIntro.defaultPath")}
          </p>
          <RepoDocumentPanel
            repoId={activeRepo?.id ?? null}
            agentId={agentId}
            kind={activeTab}
          />
        </div>
      </section>
      {input}
    </main>
  );
}

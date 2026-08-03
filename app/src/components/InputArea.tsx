import {
  useEffect,
  useRef,
  useState,
  type ClipboardEvent,
  type KeyboardEvent,
  type ReactNode,
} from "react";
import { invoke } from "@tauri-apps/api/core";
import { open as openFileDialog } from "@tauri-apps/plugin-dialog";
import type { Mode } from "./ModeDropdown";
import { ComposerAgentSelector } from "./ComposerAgentSelector";
import type {
  AgentProfile,
  ChatMessage,
  ComposerRuntimeConfig,
  DecisionCardBlock,
} from "../types/agent";
import { quoteLabel, quotePreview, quoteTooltip } from "../lib/quoteMessage";
import { useI18n } from "../i18n";
import {
  advanceStreamActivity,
  type StreamActivityState,
} from "../lib/runningStatus";
import {
  formatTokenCount,
  sessionUsageDetail,
  sessionUsageTotal,
  type SessionUsage,
} from "../lib/sessionUsage";
import { WorkingClock } from "./WorkingClock";
import { PendingDecisionBar } from "./PendingDecisionBar";

type Props = {
  composerBusy: boolean;
  running: boolean;
  memberRunning: boolean;
  agents?: AgentProfile[];
  agentId: string;
  onAgentChange: (agentId: string) => void;
  onMenuAgents?: () => void;
  mode: Mode;
  onModeChange: (m: Mode) => void;
  onSend: (text: string, mode: Mode, config?: ComposerRuntimeConfig) => void;
  onMemberIdle?: () => void;
  onStop: () => void;
  quoted?: ChatMessage | null;
  quoteKey?: string | null;
  onClearQuote?: () => void;
  pendingDecision?: DecisionCardBlock | null;
  onDecisionChoose?: (decisionId: string, option: string) => void;
  canSend?: boolean;
  readonlyReason?: string | null;
  loading?: boolean;
  teamSaving?: boolean;
  teamLeadId?: string | null;
  rosterIds?: string[] | null;
  onSetLead?: (id: string | null, memberIds?: string[]) => void;
  onToggleRoster?: (id: string, allEnabledIds: string[]) => void;
  runMeta?: string | null;
  sessionUsage?: SessionUsage | null;
  runStartedAt?: number | null;
  workingTokens?: number | null;
  lastStepSummary?: string | null;
  streamMessages?: ChatMessage[];
  sessionId?: string | null;
  /** 状态行派单归因（UX②）：等哪个 worker——存在时替换「Silent for Ns · Long-running…」两段。 */
  activeWorker?: { name: string; sub: string; count: number } | null;
};

const MAX_H = 160;
const EMPTY_STREAM_MESSAGES: ChatMessage[] = [];

type RunningStatusDetailsProps = {
  running: boolean;
  sessionId: string | null;
  workingSeconds: number;
  workingTokens: number | null;
  lastStepSummary: string | null;
  streamMessages: ChatMessage[];
  activeWorker?: { name: string; sub: string; count: number } | null;
  children: (details: string) => ReactNode;
};

function RunningStatusDetails({
  running,
  sessionId,
  workingSeconds,
  workingTokens,
  lastStepSummary,
  streamMessages,
  activeWorker,
  children,
}: RunningStatusDetailsProps) {
  const { t } = useI18n();
  const streamActivityRef = useRef<StreamActivityState | null>(null);
  const streamActivity = advanceStreamActivity(streamActivityRef.current, {
    running,
    sessionId,
    workingSeconds,
    messages: streamMessages,
    workingTokens,
  });
  useEffect(() => {
    if (streamActivity) {
      streamActivityRef.current = streamActivity;
    }
  }, [streamActivity]);

  const details = [
    ` · ${workingSeconds}s`,
    workingTokens != null && workingTokens > 0
      ? ` · ↑ ${formatTokenCount(workingTokens)} tok`
      : null,
    lastStepSummary
      ? ` · ${t("stream.status.lastStep", { summary: lastStepSummary })}`
      : null,
    streamActivity?.silenceSeconds != null
      ? activeWorker
        ? ` · ${
            activeWorker.count > 1
              ? t("stream.status.waitingOnWorkers", {
                  count: activeWorker.count,
                  name: activeWorker.name,
                })
              : t("stream.status.waitingOnWorker", { name: activeWorker.name })
          }`
        : ` · ${t("stream.status.silent", {
            seconds: streamActivity.silenceSeconds,
          })} · ${t("stream.status.longTask")}`
      : null,
  ]
    .filter(Boolean)
    .join("");

  return <>{children(details)}</>;
}

function RunningStatusClock({
  startedAt,
  running,
  sessionId,
  workingTokens,
  lastStepSummary,
  streamMessages,
  activeWorker,
  children,
}: Omit<RunningStatusDetailsProps, "workingSeconds"> & {
  startedAt: number | null;
}) {
  return (
    <WorkingClock startedAt={startedAt}>
      {(workingSeconds) => (
        <RunningStatusDetails
          running={running}
          sessionId={sessionId}
          workingSeconds={workingSeconds}
          activeWorker={activeWorker}
          workingTokens={workingTokens}
          lastStepSummary={lastStepSummary}
          streamMessages={streamMessages}
        >
          {children}
        </RunningStatusDetails>
      )}
    </WorkingClock>
  );
}

type AttachmentContent = {
  name: string;
  kind: "text" | "image" | "binary";
  content: string;
  truncated: boolean;
  byteLen: number;
};

function langHint(name: string): string {
  const extension = name.split(".").pop()?.toLowerCase();
  const languages: Record<string, string> = {
    svg: "xml",
    ts: "typescript",
    tsx: "tsx",
    js: "javascript",
    jsx: "jsx",
    py: "python",
    rs: "rust",
    md: "markdown",
    json: "json",
    sh: "bash",
    bash: "bash",
    yml: "yaml",
    yaml: "yaml",
    html: "html",
    css: "css",
    toml: "toml",
  };

  return extension ? (languages[extension] ?? "") : "";
}

function arrayBufferToBase64(buffer: ArrayBuffer): string {
  const bytes = new Uint8Array(buffer);
  const chunks: string[] = [];
  const chunkSize = 0x8000;
  for (let offset = 0; offset < bytes.length; offset += chunkSize) {
    const chunk = bytes.subarray(offset, offset + chunkSize);
    chunks.push(String.fromCharCode.apply(null, chunk as unknown as number[]));
  }
  return btoa(chunks.join(""));
}

export function InputArea({
  composerBusy,
  running,
  memberRunning,
  agents,
  agentId,
  onAgentChange,
  onMenuAgents,
  mode: _mode,
  onModeChange,
  onSend,
  onMemberIdle,
  onStop,
  quoted = null,
  quoteKey = null,
  onClearQuote,
  pendingDecision = null,
  onDecisionChoose,
  canSend = true,
  readonlyReason = null,
  loading = false,
  teamSaving = false,
  teamLeadId,
  rosterIds,
  onSetLead,
  onToggleRoster,
  runMeta = null,
  sessionUsage = null,
  runStartedAt = null,
  workingTokens = null,
  lastStepSummary = null,
  streamMessages = EMPTY_STREAM_MESSAGES,
  sessionId = null,
  activeWorker = null,
}: Props) {
  const { t } = useI18n();
  const [draft, setDraft] = useState("");
  const [guardHint, setGuardHint] = useState<string | null>(null);
  const [attachments, setAttachments] = useState<
    { path: string; name: string }[]
  >([]);
  const taRef = useRef<HTMLTextAreaElement>(null);
  const composingRef = useRef(false);

  useEffect(() => {
    if (quoteKey) taRef.current?.focus();
  }, [quoteKey]);

  useEffect(() => {
    if (!memberRunning) setGuardHint(null);
  }, [memberRunning]);

  function autosize(el = taRef.current) {
    if (!el) return;
    el.style.height = "auto";
    el.style.height = `${Math.min(el.scrollHeight, MAX_H)}px`;
    el.style.overflowY = el.scrollHeight > MAX_H ? "auto" : "hidden";
  }

  const enabledAgentIds = (agents ?? [])
    .filter((agent) => agent.enabled)
    .map((agent) => agent.id);
  const selectorLeadId = teamLeadId ?? null;
  const selectorMemberIds = selectorLeadId !== null ? (rosterIds ?? []) : [];
  const selectorTeamMode = selectorLeadId !== null;
  const activeMode: Mode =
    selectorTeamMode && selectorLeadId !== null ? "team" : "normal";
  const totalTokens = sessionUsage ? sessionUsageTotal(sessionUsage) : 0;
  const usageMeta =
    totalTokens > 0
      ? `${t("composer.usage.total")} ${formatTokenCount(totalTokens)} tok`
      : null;
  const statusMeta = running
    ? t("stream.status.working")
    : memberRunning
      ? t("composer.status.membersWorking")
      : [runMeta, usageMeta].filter(Boolean).join(" · ") || null;
  const statusTitle =
    !running && usageMeta && sessionUsage
      ? sessionUsageDetail(sessionUsage)
      : undefined;
  const readonly = readonlyReason !== null && readonlyReason.length > 0;
  const renderWorkingBar = (details: string) => {
    const fullText = `${statusMeta ?? ""}${details}`;
    return (
      <div
        className="composer__working"
        data-testid="composer-working"
        title={fullText}
      >
        <span className="composer__working-text">
          <span role="status" aria-live="polite">
            {statusMeta}
          </span>
          <span aria-hidden="true">{details}</span>
        </span>
      </div>
    );
  };

  function mergeAttachmentPaths(paths: string[]) {
    setAttachments((prev) => {
      const next = [...prev];
      const seen = new Set(prev.map((attachment) => attachment.path));
      for (const path of paths) {
        if (seen.has(path)) continue;
        next.push({ path, name: path.split(/[\\/]/).pop() ?? path });
        seen.add(path);
      }
      return next;
    });
  }

  async function attachFile() {
    if (readonly) return;
    const sel = await openFileDialog({ multiple: true });
    const paths = sel === null ? [] : Array.isArray(sel) ? sel : [sel];
    mergeAttachmentPaths(paths);
    const el = taRef.current;
    el?.focus();
  }

  async function onPaste(event: ClipboardEvent<HTMLTextAreaElement>) {
    if (readonly) return;
    const imageItems = Array.from(event.clipboardData?.items ?? []).filter(
      (item) => item.kind === "file" && item.type.startsWith("image/"),
    );
    if (imageItems.length === 0) return;

    event.preventDefault();
    for (const item of imageItems) {
      const file = item.getAsFile();
      if (!file) continue;
      try {
        const imageBase64 = arrayBufferToBase64(await file.arrayBuffer());
        const path = await invoke<string>("save_pasted_image", {
          imageBase64,
          mediaType: file.type,
        });
        mergeAttachmentPaths([path]);
      } catch (error) {
        console.error("Failed to paste image attachment", error);
      }
    }
  }

  async function submit() {
    const text = draft.trim();
    const recoverableMemberBlock =
      memberRunning && !running && !loading && sessionId !== null;
    if (
      (!text && attachments.length === 0) ||
      (composerBusy && !recoverableMemberBlock) ||
      !canSend ||
      readonly
    )
      return;

    if (recoverableMemberBlock) {
      try {
        const stillRunning = await invoke<boolean>("is_team_session_running", {
          sessionId,
        });
        if (stillRunning) {
          setGuardHint(t("composer.memberActiveHint"));
          return;
        }
        onMemberIdle?.();
      } catch {
        setGuardHint(t("composer.memberRecheckFailedHint"));
        return;
      }
    }

    const blocks: string[] = [];
    for (const attachment of attachments) {
      try {
        const content = await invoke<AttachmentContent>("read_attachment", {
          path: attachment.path,
        });
        if (content.kind === "text") {
          blocks.push(
            `Attached file: ${attachment.path}\n\`\`\`${langHint(attachment.name)}\n${content.content}\n\`\`\`${content.truncated ? "\n(truncated to 256 KB)" : ""}`,
          );
        } else if (content.kind === "image") {
          blocks.push(
            `![${t("composer.attachment.imageAlt")}](<${attachment.path}>)`,
          );
        } else {
          blocks.push(
            `Attached file: ${attachment.path} (binary — content not included)`,
          );
        }
      } catch (error) {
        blocks.push(
          `Attached file: ${attachment.path} (could not read: ${String(error)})`,
        );
      }
    }

    const composed = [text, ...blocks].filter(Boolean).join("\n\n");
    onSend(composed, activeMode);
    setGuardHint(null);
    setDraft("");
    setAttachments([]);

    const el = taRef.current;
    if (el) {
      el.style.height = "auto";
      el.style.overflowY = "hidden";
    }
  }

  function onKeyDown(e: KeyboardEvent<HTMLTextAreaElement>) {
    if (e.key !== "Enter") return;

    const composing =
      composingRef.current || e.nativeEvent.isComposing || e.keyCode === 229;
    if (e.shiftKey || composing) return;

    e.preventDefault();
    void submit();
  }

  const handleSetLead = (id: string | null, memberIds?: string[]) => {
    onSetLead?.(id, memberIds);
    onModeChange(id === null ? "normal" : "team");
  };
  const handleToggleMember = (id: string) => {
    onToggleRoster?.(id, enabledAgentIds);
  };

  return (
    <div className="composer">
      {pendingDecision && (
        <PendingDecisionBar
          block={pendingDecision}
          onChoose={onDecisionChoose}
        />
      )}
      {quoted && (
        <div className="composer__quote">
          <svg
            className="composer__quote-icon"
            viewBox="0 0 24 24"
            fill="none"
            stroke="currentColor"
            strokeWidth="2"
            strokeLinecap="round"
            strokeLinejoin="round"
            aria-hidden="true"
          >
            <polyline points="9 14 4 9 9 4" />
            <path d="M20 20v-7a4 4 0 0 0-4-4H4" />
          </svg>
          <span className="composer__quote-label">{quoteLabel(quoted, t)}</span>
          <span className="composer__quote-text" title={quoteTooltip(quoted)}>
            {quotePreview(quoted)}
          </span>
          <button
            type="button"
            className="composer__quote-clear"
            aria-label={t("composer.quote.clear")}
            title={t("composer.quote.clear")}
            onClick={onClearQuote}
          >
            <svg
              viewBox="0 0 24 24"
              fill="none"
              stroke="currentColor"
              strokeWidth="2"
              strokeLinecap="round"
              aria-hidden="true"
            >
              <path d="M18 6L6 18M6 6l12 12" />
            </svg>
          </button>
        </div>
      )}
      <div className="composer__box">
        {running && runStartedAt != null ? (
          <RunningStatusClock
            startedAt={runStartedAt}
            running={running}
            sessionId={sessionId ?? null}
            workingTokens={workingTokens}
            lastStepSummary={lastStepSummary}
            streamMessages={streamMessages}
            activeWorker={activeWorker}
          >
            {renderWorkingBar}
          </RunningStatusClock>
        ) : running || memberRunning ? (
          renderWorkingBar("")
        ) : null}
        {attachments.length > 0 && (
          <div className="composer__attachments">
            {attachments.map((attachment) => (
              <span
                key={attachment.path}
                className="composer__chip"
                title={attachment.path}
              >
                <svg
                  className="composer__chip-ic"
                  viewBox="0 0 24 24"
                  fill="none"
                  stroke="currentColor"
                  strokeWidth="2"
                >
                  <path d="M8 3h6l4 4v12a2 2 0 0 1-2 2H8a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2z" />
                </svg>
                <span className="composer__chip-name">{attachment.name}</span>
                <button
                  type="button"
                  className="composer__chip-x"
                  aria-label={t("composer.attachment.remove")}
                  onClick={() =>
                    setAttachments((prev) =>
                      prev.filter((item) => item.path !== attachment.path),
                    )
                  }
                >
                  ×
                </button>
              </span>
            ))}
          </div>
        )}
        <textarea
          ref={taRef}
          className="composer__input"
          rows={1}
          placeholder={t("composer.input.placeholder")}
          value={draft}
          disabled={readonly}
          onChange={(e) => {
            setGuardHint(null);
            setDraft(e.target.value);
            autosize(e.currentTarget);
          }}
          onKeyDown={onKeyDown}
          onPaste={(event) => void onPaste(event)}
          onCompositionStart={() => {
            composingRef.current = true;
          }}
          onCompositionEnd={() => {
            composingRef.current = false;
          }}
        />
        <div className="composer__row">
          {/* 单档 Auto（信任落地）。保留组件位以便未来加严审/Plan 档，
              但当前诚实呈现：非可切换的静态指示，非伪装的下拉触发器。 */}
          <div
            className="composer__permission-wrap"
            data-testid="composer-permission"
          >
            <span
              className="composer__permission is-static"
              role="note"
              aria-label={t("composer.permission.label")}
              title={`${t("composer.permission.trustBase")} · ${t("composer.permission.autoOnly")}`}
            >
              <svg viewBox="0 0 24 24" aria-hidden="true">
                <rect x="3" y="11" width="18" height="11" rx="2" />
                <path d="M7 11V7a5 5 0 0110 0v4" />
              </svg>
              <span className="composer__permission-label">
                {t("composer.permission.shortLabel")}
              </span>
              <span className="composer__permission-value">Auto</span>
            </span>
          </div>
          <button
            type="button"
            className="composer__icon"
            disabled={readonly}
            onClick={() => void attachFile()}
            aria-label={t("composer.attachment.label")}
            title={t("composer.attachment.label")}
          >
            <svg viewBox="0 0 24 24" strokeLinecap="round">
              <path d="M21.44 11.05l-9.19 9.19a6 6 0 01-8.49-8.49l9.19-9.19a4 4 0 015.66 5.66l-9.2 9.19a2 2 0 01-2.83-2.83l8.49-8.48" />
            </svg>
          </button>
          <button
            type="button"
            className="composer__icon"
            disabled
            aria-label={t("composer.voice.label")}
            title={t("composer.voice.comingSoon")}
          >
            <svg viewBox="0 0 24 24" strokeLinecap="round">
              <rect x="9" y="2" width="6" height="12" rx="3" />
              <path d="M5 10v1a7 7 0 0014 0v-1M12 18v4" />
            </svg>
          </button>
          <span className="composer__sp" />
          <ComposerAgentSelector
            agents={agents}
            agentId={agentId}
            leadId={selectorLeadId}
            memberIds={selectorMemberIds}
            teamMode={selectorTeamMode}
            onAgentChange={onAgentChange}
            onSetLead={handleSetLead}
            onToggleMember={handleToggleMember}
            onMenuAgents={onMenuAgents}
            disabled={composerBusy || readonly}
            loading={loading}
            saving={teamSaving}
          />
          {!running && (
            <button
              type="button"
              className="composer__send"
              aria-label={t("composer.send")}
              onClick={() => void submit()}
              disabled={
                (!draft.trim() && attachments.length === 0) ||
                (composerBusy &&
                  !(memberRunning && !running && !loading && sessionId)) ||
                !canSend ||
                readonly
              }
            >
              <svg
                viewBox="0 0 24 24"
                strokeLinecap="round"
                strokeLinejoin="round"
              >
                <path d="M22 2L11 13M22 2l-7 20-4-9-9-4 20-7z" />
              </svg>
            </button>
          )}
          {(running || memberRunning) && (
            <button
              type="button"
              className="composer__stop"
              aria-label={t("composer.stop")}
              onClick={onStop}
            >
              <svg viewBox="0 0 24 24">
                <rect x="6" y="6" width="12" height="12" rx="1" />
              </svg>
            </button>
          )}
        </div>
      </div>
      <div className="composer__hint">
        <span
          className={`composer__hint-l${readonly ? " composer__hint-l--readonly" : ""}`}
          role={guardHint ? "status" : undefined}
          aria-live={guardHint ? "polite" : undefined}
        >
          {readonly ? readonlyReason : (guardHint ?? t("composer.hint.send"))}
        </span>
        {running && runStartedAt != null ? (
          <span className="composer__hint-cost">
            {workingTokens != null && workingTokens > 0
              ? `↑ ${formatTokenCount(workingTokens)} tok`
              : null}
          </span>
        ) : !running && !memberRunning && statusMeta ? (
          <span
            className="composer__hint-cost"
            aria-live="polite"
            title={statusTitle}
          >
            {statusMeta}
          </span>
        ) : null}
      </div>
    </div>
  );
}

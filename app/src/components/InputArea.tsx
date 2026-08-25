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
import type { QueuedMessage } from "../lib/composerQueue";

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
  /**
   * msgfix2 Q1：后端复核证实「member 真在跑」（recoverableMemberBlock 分支）时，
   * 改把 composed 文本入队而非拒发。只在这条分支用到——`running`=true 的整体
   * busy 早退改走 onSend 本身（App 层的 onSend 已识别 running 并入队）。
   */
  onQueueMessage?: (text: string, mode: Mode) => void;
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
  /** msgfix2 Q1：本会话运行中排队的消息（chip 列表，FIFO 顺序）。 */
  queuedMessages?: QueuedMessage[];
  /** 显式停止后暂停自动递送——chip 呈现「待手动发送」态 + 单条发送按钮。 */
  queuePaused?: boolean;
  /** 把队列条目移出队列并把其文本回填给调用方（App 层实际执行移除，回填由这里拿到的文本做）。 */
  onEditQueuedMessage?: (id: string) => string | null;
  onRemoveQueuedMessage?: (id: string) => void;
  /** queuePaused 态下单条手动递送。 */
  onSendQueuedMessage?: (id: string) => void;
};

const MAX_H = 160;
// 超长文本每键读 scrollHeight 会触发同步强制布局，耗时随全文长度线性增长——
// 超过此阈值时跳过测量、直接锁最大高度+内部滚动，避免打字卡死。
const AUTOSIZE_MAX_CHARS = 20000;
// textarea 装几万字符后每次编辑触发全文断行 relayout·原生代价 autosize 早退救不了·
// 超长粘贴转附件根治：粘贴文本超此阈值时不进输入框，落盘转成附件 chip。
const PASTE_TO_ATTACHMENT_CHARS = 10_000;
const EMPTY_STREAM_MESSAGES: ChatMessage[] = [];
const EMPTY_QUEUE: QueuedMessage[] = [];

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
  queuedMessages = EMPTY_QUEUE,
  queuePaused = false,
  onEditQueuedMessage,
  onRemoveQueuedMessage,
  onSendQueuedMessage,
  onQueueMessage,
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
    if (el.value.length > AUTOSIZE_MAX_CHARS) {
      // 超长文本：不读 scrollHeight，直接锁最大高度+内部滚动。
      el.style.height = `${MAX_H}px`;
      el.style.overflowY = "auto";
      return;
    }
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
    if (imageItems.length > 0) {
      // 既有语义：剪贴板同时有图+文时，只处理图片（文本部分与改动前一样被丢弃）。
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
      return;
    }

    const text = event.clipboardData?.getData?.("text/plain") ?? "";
    if (text.length <= PASTE_TO_ATTACHMENT_CHARS) return;

    event.preventDefault();
    try {
      const path = await invoke<string>("save_pasted_text", { text });
      mergeAttachmentPaths([path]);
    } catch (error) {
      console.error("Failed to paste text attachment", error);
      // 落盘失败：回退把原文本插回输入框光标处，宁可卡也不丢用户内容。
      const el = taRef.current;
      const start = el?.selectionStart ?? draft.length;
      const end = el?.selectionEnd ?? draft.length;
      setDraft((prev) => prev.slice(0, start) + text + prev.slice(end));
    }
  }

  async function composeText(rawText: string): Promise<string> {
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
    return [rawText, ...blocks].filter(Boolean).join("\n\n");
  }

  function resetComposerAfterSubmit() {
    setGuardHint(null);
    setDraft("");
    setAttachments([]);
    const el = taRef.current;
    if (el) {
      el.style.height = "auto";
      el.style.overflowY = "hidden";
    }
  }

  async function submit() {
    const text = draft.trim();
    // loading（agents/messages 还没就绪）仍整段拒发——排队只对「已就绪但正忙」有意义。
    if (
      (!text && attachments.length === 0) ||
      !canSend ||
      readonly ||
      loading
    ) {
      return;
    }

    // memberRunning 是前端派生、可能陈旧（dispatch_card 状态）：lead 自己的 run 已经
    // 收尾（!running）但 member 卡还没来得及收敛时，先问后端复核一次再决定。
    const recoverableMemberBlock =
      memberRunning && !running && sessionId !== null;
    if (recoverableMemberBlock) {
      try {
        const stillRunning = await invoke<boolean>("is_team_session_running", {
          sessionId,
        });
        if (stillRunning) {
          // msgfix2 Q1：member 真在跑——不再拒发，改投队列（完成后自动递送）。
          const composed =
            attachments.length > 0 ? await composeText(text) : text;
          onQueueMessage?.(composed, activeMode);
          resetComposerAfterSubmit();
          return;
        }
        onMemberIdle?.();
        // 复核证实已 idle：往下走正常发送路径（不入队，直接发）。
      } catch {
        setGuardHint(t("composer.memberRecheckFailedHint"));
        return;
      }
    }

    // running=true（solo 在跑 / lead 在跑）且非 recoverableMemberBlock：不再在这里拒发，
    // 直接交给 onSend——App 层的 onSend 会识别「该 session 已在跑」并改投队列。
    // 无附件时不 await（composeText 本身是 async 函数，await 一定会让出一个 microtask
    // tick）——保住「Enter 发送后同步调用 onSend」这条既有语义，别让排队特性引入
    // 一个全局性的多余异步跳变，坑到一堆同步 fireEvent 断言。
    const composed = attachments.length > 0 ? await composeText(text) : text;
    onSend(composed, activeMode);
    resetComposerAfterSubmit();
  }

  function onKeyDown(e: KeyboardEvent<HTMLTextAreaElement>) {
    if (e.key !== "Enter") return;

    const composing =
      composingRef.current || e.nativeEvent.isComposing || e.keyCode === 229;
    if (e.shiftKey || composing) return;

    e.preventDefault();
    void submit();
  }

  // 队列条目「编辑」：移出队列（App 层实际删除并把文本吐回来），回填 textarea。
  // 非空则把当前内容前置拼接（选最简且不丢内容的实现——不弹确认、不覆盖用户已敲的字）。
  function handleEditQueuedMessage(id: string) {
    const text = onEditQueuedMessage?.(id);
    if (text == null) return;
    setDraft((prev) => (prev.trim().length > 0 ? `${prev}\n\n${text}` : text));
    const el = taRef.current;
    if (el) {
      el.focus();
      requestAnimationFrame(() => autosize(el));
    }
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
      {queuedMessages.length > 0 && (
        <div className="composer__queue" data-testid="composer-queue">
          <div className="composer__queue-label">
            {t("composer.queued.count", { count: queuedMessages.length })}
            {queuePaused && (
              <span className="composer__queue-paused-hint">
                {" "}
                · {t("composer.queued.pausedHint")}
              </span>
            )}
          </div>
          {queuedMessages.map((qm) => (
            <div
              key={qm.id}
              className="composer__queue-item"
              data-testid="composer-queue-item"
            >
              <span className="composer__queue-text" title={qm.text}>
                {qm.text.length > 80 ? `${qm.text.slice(0, 80)}…` : qm.text}
              </span>
              <div className="composer__queue-actions">
                {queuePaused && (
                  <button
                    type="button"
                    className="composer__queue-btn composer__queue-btn--send"
                    onClick={() => onSendQueuedMessage?.(qm.id)}
                    aria-label={t("composer.queued.send")}
                    title={t("composer.queued.send")}
                  >
                    {t("composer.queued.send")}
                  </button>
                )}
                <button
                  type="button"
                  className="composer__queue-btn"
                  onClick={() => handleEditQueuedMessage(qm.id)}
                  aria-label={t("composer.queued.edit")}
                  title={t("composer.queued.edit")}
                >
                  {t("composer.queued.edit")}
                </button>
                <button
                  type="button"
                  className="composer__queue-btn composer__queue-btn--remove"
                  onClick={() => onRemoveQueuedMessage?.(qm.id)}
                  aria-label={t("composer.queued.remove")}
                  title={t("composer.queued.remove")}
                >
                  ×
                </button>
              </div>
            </div>
          ))}
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
          {
            // msgfix2 Q1：不再因 running/memberRunning 隐藏发送——运行中点它是「排队」
            // 而非「发送」，只有 loading（还没就绪）/ canSend=false / readonly 才禁用。
          }
          <button
            type="button"
            className="composer__send"
            aria-label={t("composer.send")}
            onClick={() => void submit()}
            disabled={
              (!draft.trim() && attachments.length === 0) ||
              !canSend ||
              readonly ||
              loading
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

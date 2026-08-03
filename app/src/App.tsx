import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import "./styles/global.css";
import { DEFAULT_LOCAL_PROJECT_ID, useI18n } from "./i18n";
import { SessionMain } from "./components/SessionMain";
import type { ContinuationDraftState } from "./components/ContinuationBriefPanel";
import { OverviewHome } from "./components/OverviewHome";
import { Sidebar } from "./components/Sidebar";
import { RightPanel } from "./components/RightPanel";
import type { RightPanelTab } from "./components/RightPanelTabs";
import { GoalCriteriaPanel } from "./components/GoalCriteriaPanel";
import { SurfaceHeader } from "./components/SurfaceHeader";
import { ProjectIntroPage } from "./components/ProjectIntroPage";
import { RepoDocumentProvider } from "./contexts/RepoDocumentProvider";
import type { Mode } from "./components/ModeDropdown";
import { RepoManagePanel } from "./components/RepoManagePanel";
import { SettingsAgents } from "./components/settings/SettingsAgents";
import { SettingsLanguage } from "./components/settings/SettingsLanguage";
import { SettingsSearch } from "./components/settings/SettingsSearch";
import { ArchivedProjectsPanel } from "./components/settings/ArchivedProjectsPanel";
import { SettingsSheet } from "./components/settings/SettingsSheet";
import type { SettingsPage } from "./components/settings/SettingsShell";
import {
  NewProjectSheet,
  type NewProjectArgs,
} from "./components/NewProjectSheet";
import { InvalidProjectDialog } from "./components/InvalidProjectDialog";
import { ConfirmDialog } from "./components/ConfirmDialog";
import { AgentInstallGuideDialog } from "./components/AgentInstallGuideDialog";
import { Lightbox } from "./components/Lightbox";
import { isAgentAvailable, type RuntimeDetect } from "./lib/agentAvailability";
import { shouldShowInstallGuide } from "./lib/agentOnboarding";
import { loadLastAgentId, saveLastAgentId } from "./lib/agentPrefStore";
import {
  deriveComposerBusy,
  deriveSendGate,
  deriveStickyAgentId,
  resolveFallbackAgentId,
} from "./lib/sendGate";
import {
  useTeamConfig,
  load as loadTeamConfig,
  saveSessionTeamConfig,
  type TeamConfig,
} from "./lib/useTeamConfig";
import { computeGate } from "./lib/ghGate";
import { runClones } from "./lib/cloneOrchestrator";
import {
  deriveView,
  deserializeRepoCache,
  mergeRefresh,
  pruneSelection,
  REPO_CACHE_STORAGE_KEY,
  serializeRepoCache,
} from "./lib/repoCache";
import {
  isReviewPanelVisible,
  shouldFetchOnSwitch,
} from "./lib/reviewRefreshGate";
import {
  appendRunCard,
  appendScopeChangeCard,
  appendTextDelta,
  appendThinkingDelta,
  appendToolStarted,
  appendApprovalRequested,
  applyToolCompleted,
  applyApprovalResolved,
  assistantText,
  ensureStreamTail,
  hasRunningTool,
  sweepRunning,
} from "./lib/streamBlocks";
import { isHiddenTool } from "./lib/streamItems";
import {
  applyTeamEvent,
  isDispatchEnvelope,
  isOrchestratedDispatch,
  isTeamRunComplete,
  teamRunToBlock,
} from "./lib/teamReducer";
import {
  upsertDispatchCard,
  memberByAssignment,
  runIdByAssignment,
  collectReloadRunInfo,
  clearStaleRunningDispatchCards,
  hasRunningDispatchCard,
  orchestratedGoalSource,
  latestDispatchRunIds,
  hydrateWorkerReportCards,
} from "./lib/dispatchCards";
import { startMemberIdlePoll } from "./lib/memberIdlePoll";
import {
  isLandingBlockedError,
  nextCodingAction,
  selectCodingVerifier,
  shouldEnterCodingLoop,
  type CodingState,
} from "./lib/codingLoop";
import { advanceCodingLoop } from "./lib/codingLoopDriver";
import {
  classifyLeadError,
  parseBackendError,
  renderBackendError,
} from "./lib/backendMsg";
import { humanizeStopReason } from "./lib/stopReason";
import { deriveSessionTitle } from "./lib/sessionTitle";
import {
  draftFromResult,
  emptyDraft,
  gateReducer,
  type GateAction,
  type GateDraft,
} from "./lib/gateReducer";
import type { GateView } from "./lib/gateView";
import type { LeadView } from "./lib/leadView";
import {
  buildCodingVerdictSummary,
  buildFallbackRawSummary,
  buildFailureFindings,
  buildPendingSummary,
  buildSinglePassthroughSummary,
  buildSynthesisSummary,
  memberFinalText,
  summaryStatusOf,
} from "./lib/leadSummary";
import type {
  ProposeOutcome,
  ProposeResult,
  ParsedAssignment,
} from "./types/gate";
import { parseAssignments, assignmentsToCriteria } from "./types/gate";
import type {
  AcceptanceCriterion,
  AgentProfile,
  AgentEventEnvelope,
  ChatMessage,
  DecisionCardBlock,
  ReviewResult,
  RepoMeta,
  Session,
  NamespaceMeta,
  AppContext,
  Block,
  ComposerRuntimeConfig,
  GroupMeta,
  GoalContract,
  LeadAction,
  LeadStepOutcome,
  MemberUnit,
  TeamRun,
  TeamRunPendingRow,
  CodingTaskBlock,
  SessionGoal,
  ContinuationHandoffDraft,
} from "./types/agent";
import type { UndoResultRecord } from "./types/undo";
import type {
  CloneProgressEntry,
  CloneRowState,
  RepoCacheEntry,
  RepoOpenSessionTarget,
  RemoteRepo,
  RepoFilter,
  RepoKey,
} from "./types/repoManage";
import { repoKey } from "./types/repoManage";
import {
  accumulateSessionUsage,
  accumulateWorkingTokens,
  sessionUsageFromSession,
  type SessionUsage,
} from "./lib/sessionUsage";
import { AboutDialog } from "./components/AboutDialog";

type RunInfo = {
  startedAt: number;
  workingTokens: number | null;
  engine: string;
  agent_id: string;
  agent_name_snapshot: string | null;
};
type UsageDeltaEnvelope = {
  session_id: string;
  dispatch?: AgentEventEnvelope["dispatch"];
  kind: "usage_delta";
  input_tokens: number | null;
  output_tokens: number | null;
};
type AppAgentEventEnvelope = AgentEventEnvelope | UsageDeltaEnvelope;
type SessionMutator = (
  sid: string,
  fn: (msgs: ChatMessage[]) => ChatMessage[],
) => void;
type AppAgentEventBatchPayload = {
  batches: Array<{
    session_id: string;
    dispatch?: AgentEventEnvelope["dispatch"];
    events: Array<{ seq: number; kind: string; [key: string]: unknown }>;
  }>;
};

type BatchEventEnvelope = {
  session_id: string;
  dispatch?: AgentEventEnvelope["dispatch"];
  kind: string;
  [key: string]: unknown;
};

export function applyEventTransportBatch<T>(
  payload: AppAgentEventBatchPayload,
  getMessages: () => Map<string, T[]>,
  applyEvent: (
    event: BatchEventEnvelope,
    mutate: (sid: string, fn: (items: T[]) => T[]) => void,
  ) => void,
  isTerminal: (event: BatchEventEnvelope) => boolean,
  onMessagesChange: (messages: Map<string, T[]>) => void,
  cloneMessages: (messages: Map<string, T[]>) => Map<string, T[]> = (
    messages,
  ) => new Map(messages),
): { messagesChanged: boolean; hasTerminal: boolean } {
  let next: Map<string, T[]> | null = null;
  let hasTerminal = false;
  const mutate = (sid: string, fn: (items: T[]) => T[]) => {
    if (next === null) next = cloneMessages(getMessages());
    const current = next.get(sid) ?? [];
    next.set(sid, fn(current));
    onMessagesChange(next);
  };

  for (const batch of payload.batches) {
    for (const sequenced of batch.events) {
      const { seq: _seq, ...event } = sequenced;
      const envelope: BatchEventEnvelope = {
        ...event,
        session_id: batch.session_id,
        ...(batch.dispatch ? { dispatch: batch.dispatch } : {}),
      };
      applyEvent(envelope, mutate);
      hasTerminal ||= isTerminal(envelope);
    }
  }

  return { messagesChanged: next !== null, hasTerminal };
}
/** 左栏行状态点三态（切走后仍知 agent 死活）：running=跑着；attention=needs_decision/error/blocked；done=completed。 */
type SessionDotStatus = "running" | "attention" | "done";
type RunCommitState = {
  run_id: string;
  state: string;
  undo_total: number;
  undo_undone: number;
};
type RunCardState = NonNullable<Extract<Block, { type: "run_card" }>["state"]>;
type AppView = "overview" | "session" | "intro";
type AppRoute = {
  view: AppView;
  sessionId: string | null;
  namespaceId: string | null;
  repoId: string | null;
};
type CodingLoopDisplayMeta = {
  worker_name: string;
  step_done?: number;
  step_total?: number;
};

const NAV_HISTORY_LIMIT = 50;

/**
 * 剪枝导航历史栈（删除/归档会话时调用）
 * 移除所有匹配 sessionId 的条目，修正索引，合并相邻重复条目
 */
export function pruneNavHistory(
  history: AppRoute[],
  index: number,
  sessionId: string,
): { history: AppRoute[]; index: number } {
  // 1. 移除所有匹配 sessionId 的条目，并统计索引修正
  const filtered: AppRoute[] = [];
  let removedBeforeCurrent = 0;
  let currentRemoved = false; // 当前索引条目是否被删除

  for (let i = 0; i < history.length; i++) {
    const route = history[i];
    const isMatch = route.view === "session" && route.sessionId === sessionId;

    if (!isMatch) {
      filtered.push(route);
    } else {
      if (i < index) {
        removedBeforeCurrent++;
      } else if (i === index) {
        currentRemoved = true;
      }
    }
  }

  // 2. 修正索引：减去之前删除的，如果当前也被删除则再减 1
  let currentIndexAfterFilter = index - removedBeforeCurrent;
  if (currentRemoved) {
    currentIndexAfterFilter -= 1;
  }
  if (currentIndexAfterFilter < -1) {
    currentIndexAfterFilter = -1;
  }
  if (currentIndexAfterFilter >= filtered.length) {
    currentIndexAfterFilter = filtered.length - 1;
  }

  // 3. 合并相邻重复路由条目
  const merged: AppRoute[] = [];
  for (let i = 0; i < filtered.length; i++) {
    const current = filtered[i];
    const prev = merged.length > 0 ? merged[merged.length - 1] : null;

    if (prev && appRouteKey(prev) === appRouteKey(current)) {
      // 合并：跳过当前条目，调整索引
      if (i < currentIndexAfterFilter) {
        currentIndexAfterFilter--;
      }
    } else {
      merged.push(current);
    }
  }

  // 再次 clamp 索引（合并后可能变小）
  if (currentIndexAfterFilter < -1) {
    currentIndexAfterFilter = -1;
  }
  if (currentIndexAfterFilter >= merged.length) {
    currentIndexAfterFilter = merged.length - 1;
  }

  return { history: merged, index: currentIndexAfterFilter };
}

function appRouteKey(route: AppRoute): string {
  if (route.view === "session") return `session:${route.sessionId ?? ""}`;
  return `${route.view}:${route.namespaceId ?? ""}:${route.repoId ?? ""}`;
}

function routeFromState(
  view: AppView,
  currentId: string | null,
  namespaceId: string,
  repoId: string | null,
): AppRoute {
  return {
    view,
    sessionId: view === "session" ? currentId : null,
    namespaceId: view === "session" ? null : namespaceId,
    repoId: view === "session" ? null : repoId,
  };
}

type RuntimeTeamConfig = {
  hasSavedLead: boolean;
  effectiveLeadId: string;
  memberPoolIds: string[];
};
type DetectResult = {
  available: boolean;
  version: string | null;
  path: string | null;
};
type GhAccount = { login: string; active: boolean };
type InstallState = { installing: boolean; error?: string };
type ClonedRepoResult = {
  namespace_id: string;
  repo_id: string;
  dest: string;
};
type LoadRepoListOptions = { force?: boolean };

const REPO_CACHE_STALE_MS = 5 * 60 * 1000;
const REPO_CACHE_AUTO_DEBOUNCE_MS = 45 * 1000;
// 设置 > 仓库列表缓存持久化：重启时先从 localStorage hydrate 旧列表（stale-while-revalidate
// 的「stale」那一半跨重启延续），读取失败（无 localStorage / 隐私模式等）静默兜底为空。
function readPersistedRepoCache(): Record<string, RepoCacheEntry> {
  try {
    return deserializeRepoCache(localStorage.getItem(REPO_CACHE_STORAGE_KEY));
  } catch {
    return {};
  }
}
const CLONE_CONCURRENCY = 4;
const CLONE_SETTLE_LINGER_MS = 1500;
const HANDOFF_BUSY_RETRY_LIMIT = 20;
const HANDOFF_BUSY_RETRY_DELAY_MS = 25;
// T5「看一眼再派」：lead 选 dispatch_worker 时前端生成的本地确认卡（不持久化·不走后端
// choose_decision_card），用此前缀的 per-card 唯一 source_run_id（`local-dispatch-${uuid}`）
// 标记，与后端 ask_user 衍生的 dispatch_confirm 卡区分；同时让同一会话内多张本地确认卡
// （派单→取消→再派单）的 React turn key 不撞（MessageStream 据 source_run_id 取 key）。
const LOCAL_DISPATCH_PREFIX = "local-dispatch";
// MCP 队长决策卡（ask_user / propose_verifier）的 source_run_id 前缀（须与后端
// lead_tools::MCP_LEAD_DECISION_PREFIX 一致）。据此按卡身份路由·MCP 卡绝不回退 legacy lead_step。
const MCP_LEAD_PREFIX = "mcp-lead";
// worker 完成自动唤醒 lead 续跑：同一 session 连续自动续喂上限（防「lead 反复小额派单低效
// 空转」的兜底，不是防死循环——同 session 单 worker 硬闸已保证不会爆炸）。用户手动发消息即重置。
const AUTO_RESUME_MAX_STREAK = 10;
// 自动续喂竞速重试：reader 摘 member（src-tauri/src/member_runner.rs:2272）早于
// dispatch intent guard 释放（要等 run_single_worker 整体返回，含 persist/finalize/Stage①
// 收尾），前端若抢在 guard drop 前 invoke 会撞 AL_ERR:run.teamMembersActive 被拒。这是
// 暂时性竞态、不是真失败——短延迟后重试一次即可，不必长等（guard 通常很快释放）。
const AUTO_RESUME_RACE_RETRY_DELAY_MS = 800;

/** 用户手动发消息 = 明确接管，清零该 session 的自动续喂连续计数（纯函数，便于单测）。 */
export function resetAutoResumeStreak(
  streakRef: { current: Map<string, number> },
  sid: string,
): void {
  streakRef.current.delete(sid);
}

export function pickDisplayGoal(
  teamGoal: GoalContract | null,
  sessionGoal: SessionGoal | null,
): GoalContract | null {
  if (teamGoal) return teamGoal;
  if (sessionGoal) {
    return {
      goal: sessionGoal.text,
      goal_title: sessionGoal.title ?? undefined,
      status: "frozen" as const,
      criteria: [],
    };
  }
  return null;
}

function splitRepoKey(key: RepoKey): { repoOwner: string; name: string } {
  const [, repoOwner = "", name = ""] = key.split("/");
  return { repoOwner, name };
}

function cloneEntryRepoOwner(entry?: CloneProgressEntry): string | undefined {
  return entry?.[("ow" + "ner") as keyof CloneProgressEntry] as
    | string
    | undefined;
}

function normalizeRepoListError(e: unknown, login: string): string {
  const message = String(e);
  if (message.includes("OFFLINE")) return "OFFLINE";
  if (message.startsWith("NO_TOKEN")) return `NO_TOKEN:${login}`;
  return message;
}

function sameRepoSelection(a: Set<RepoKey>, b: Set<RepoKey>): boolean {
  if (a.size !== b.size) return false;
  for (const key of a) {
    if (!b.has(key)) return false;
  }
  return true;
}

function undoFeedbackKey(sessionId: string, runId: string): string {
  return `${sessionId}:${runId}`;
}

function runCardStateFromLedger(summary?: RunCommitState): RunCardState {
  const total = Math.max(0, summary?.undo_total ?? 0);
  const undone = Math.min(total, Math.max(0, summary?.undo_undone ?? 0));
  if (total > 0 && undone === total) return "undone";
  if (undone > 0) return "partially_undone";
  return "active";
}

function withRunCardStates(
  messages: ChatMessage[],
  runStates?: Map<string, RunCommitState>,
  undoFeedback?: Map<string, UndoResultRecord>,
  sessionId?: string,
): ChatMessage[] {
  return messages.map((message) => ({
    ...message,
    content: message.content.map((block) => {
      if (block.type !== "run_card") return block;
      const summary = runStates?.get(block.run_id);
      const undoResult = sessionId
        ? undoFeedback?.get(undoFeedbackKey(sessionId, block.run_id))
        : undefined;
      return {
        ...block,
        state: runCardStateFromLedger(summary),
        undo_total: Math.max(0, summary?.undo_total ?? 0),
        undo_undone: Math.max(0, summary?.undo_undone ?? 0),
        undo_result: undoResult,
      };
    }),
  }));
}

function sendMessagePayload(
  sessionId: string,
  agentId: string,
  message: string,
  config?: ComposerRuntimeConfig,
) {
  return config?.reasoningTier
    ? {
        sessionId,
        agentId,
        message,
        reasoningTier: config.reasoningTier,
        criteria: [],
      }
    : { sessionId, agentId, message, criteria: [] };
}

// 块 B（T5·P1-2·GUI 验收折轻）：run 级抑制——只消「真空壳」。
// 用户定：完成态任务条 + verdict 都留（任务条=KISS 一行进右面板看过程·verdict=结论）·不再因 verdict 消任务条。
//   空 members 的 team_run → 消（空 turn）。
//   非空 team_run 即使属于 coding run 也保留：它承载 lead / member metadata。
//   RunLeadTurn 在有 coding_task 时会隐藏 worker task stack，避免同 run 双任务条。
//   coding_task 本身永不消（它就是 coding run 的持久任务条·terminal 也留·与 verdict 并存=用户要的）。
//   非 coding run 的 terminal team_run 不再消（BackgroundTaskStack 已能好好渲 DONE 队员行·非旧空壳）。
export function suppressBlockBShells(
  msgs: ChatMessage[],
  liveCodingRunIds: Set<string>,
): ChatMessage[] {
  void liveCodingRunIds;
  return msgs.filter((m) => {
    const blocks = (m.content ?? []) as any[];
    const tr = blocks.find((b) => b.type === "team_run");
    if (tr) {
      if (tr.members.length === 0) return false; // 空 members 空 turn
    }
    return true;
  });
}

/** 改动条交付动作（commit/push/create_pr/publish）要落到「当前会话正在执行的那个 coding run」。
 *  遍历 codingLoops 的 values 找 sessionId 匹配的——同一 session 可能挂多个 run·取最近的一个：
 *  优先 phase 在 applying/applied（最接近交付的活跃落地）·否则取最后遇到的。无匹配 → null。 */
export function runIdForActiveCodingSession(
  loops: Map<string, CodingState>,
  sid: string,
): string | null {
  let fallback: string | null = null;
  let landing: string | null = null;
  for (const s of loops.values()) {
    if (s.sessionId !== sid) continue;
    fallback = s.runId;
    if (s.phase === "applying" || s.phase === "applied") landing = s.runId;
  }
  return landing ?? fallback;
}

let appRenderTraced = false;

const traceAppBoot = (label: string) => {
  if (!import.meta.env.PROD) return;
  const ms = performance.now();
  try {
    void invoke("boot_trace", { label, ms }).catch(() => {});
  } catch {
    // 非 Tauri 环境（例如测试）没有可用的 invoke。
  }
};

function AppContent() {
  if (!appRenderTraced) {
    appRenderTraced = true;
    traceAppBoot("App render start");
  }

  const { t } = useI18n();
  useLayoutEffect(() => {
    traceAppBoot(
      `App layoutEffect = commit 完成 visibility=${document.visibilityState} focus=${document.hasFocus()}`,
    );
  }, []);
  useEffect(() => {
    traceAppBoot("App effect");
  }, []);
  const tRef = useRef(t);
  tRef.current = t;
  const dispatchConfirmOk = t("app.dispatch.confirm");
  const dispatchConfirmCancel = t("app.dispatch.cancel");
  const [messagesBySession, setMessagesBySession] = useState<
    Map<string, ChatMessage[]>
  >(new Map());
  const messagesRef = useRef<Map<string, ChatMessage[]>>(new Map());
  const gateReqSeqRef = useRef<Map<string, number>>(new Map());
  const [teamRunsBySession, setTeamRunsBySession] = useState<
    Map<string, Map<string, TeamRun>>
  >(new Map());
  const [gateBySession, setGateBySession] = useState<Map<string, GateView>>(
    new Map(),
  );
  const [leadViewBySession, setLeadViewBySession] = useState<
    Map<string, LeadView>
  >(new Map());
  const leadChoosingRef = useRef<Set<string>>(new Set());
  const setLeadView = (sid: string, v: LeadView | null) =>
    setLeadViewBySession((prev) => {
      const n = new Map(prev);
      if (v) n.set(sid, v);
      else n.delete(sid);
      return n;
    });
  // 冻结发起链 in-flight 标志（P2-1·禁用主按钮防双击·简单档单布尔·gate 卡同时只渲当前会话）
  const [gateFreezing, setGateFreezing] = useState(false);
  const [frozenGoalBySession, setFrozenGoalBySession] = useState<
    Map<string, { runId: string; goal: GoalContract }>
  >(new Map());
  const [sessionGoalBySession, setSessionGoalBySession] = useState<
    Map<string, SessionGoal>
  >(new Map());
  const teamRunsRef = useRef(teamRunsBySession);
  const persistedRunsRef = useRef<Set<string>>(new Set());
  // 块②a-1 bug#3：isHiddenTool（队长编排/交互工具·决策卡为唯一呈现）的 tool_started 不建裸卡·
  // 记其 id·tool_completed 时静默跳过（不 warn / 不 applyToolCompleted）。completion 只带 id 不带 name·故跨两事件用此 ref 关联。
  const hiddenToolIdsRef = useRef<Set<string>>(new Set());
  const leadAgentIdByRunRef = useRef<Map<string, string>>(new Map());
  const codingLoopsRef = useRef<Map<string, CodingState>>(new Map());
  const leadRationaleByRunRef = useRef<Map<string, string>>(new Map());
  const autonomyRef = useRef<Map<string, string>>(new Map());
  const codingLoopDisplayRef = useRef<Map<string, CodingLoopDisplayMeta>>(
    new Map(),
  );
  const goalTitleFetchedRef = useRef<Set<string>>(new Set());
  // worker 完成自动唤醒 lead 续跑：按 `${sid}:${assignment_id}` 去重（防事件重放/重复批次
  // 导致多次 invoke），只在 resume_lead_session **成功**后才永久记入——即便当时 lead 在跑而未
  // 触发也不补触发（不排队，见 AUTO_RESUME_MAX_STREAK 旁注）。失败（含竞速重试后仍失败）不烧
  // 这个键：不是「这单自动续喂被永久放弃」，是留给后续真实事件重放/重触发条件自然补上。
  const autoResumeTriggeredRef = useRef<Set<string>>(new Set());
  // 同步「进行中」守卫：防同一 dedupKey 在单个事件批次内被重放触发并发 invoke（例如
  // applyAgentEvent 在同一 tick 内收到重复事件）。成功后随 autoResumeTriggeredRef 一起留住；
  // 失败（含重试耗尽）后摘除，让真实的事件重放有机会重新尝试。
  const autoResumeInFlightRef = useRef<Set<string>>(new Set());
  // 连续自动续喂计数（per-session）；用户手动发消息（onSend）时清零。
  const autoResumeStreakRef = useRef<Map<string, number>>(new Map());
  const [codingBlocksByRun, setCodingBlocksByRun] = useState<
    Map<string, CodingTaskBlock>
  >(new Map());
  const [acceptanceByRun, setAcceptanceByRun] = useState<
    Map<string, AcceptanceCriterion[]>
  >(new Map());
  const [goalTitleByRun, setGoalTitleByRun] = useState<Map<string, string>>(
    new Map(),
  );
  const [interruptedRunsBySession, setInterruptedRunsBySession] = useState<
    Map<string, TeamRunPendingRow[]>
  >(new Map());
  const [dismissedInterruptedRuns, setDismissedInterruptedRuns] = useState<
    Set<string>
  >(new Set());
  const [runStatesBySession, setRunStatesBySession] = useState<
    Map<string, Map<string, RunCommitState>>
  >(new Map());
  const [runningSessions, setRunningSessions] = useState<Map<string, RunInfo>>(
    new Map(),
  );
  const runningSessionsRef = useRef<Map<string, RunInfo>>(new Map());
  const stopIssuedAtRef = useRef<Map<string, number>>(new Map());
  // 左栏行状态点三态：running 由 runningSessions 派生即可（此 map 记 running + 终态 attention/done，
  // 用 ref 镜像给 deps=[] 的 agent-event 监听器读最新值，避免 stale 闭包）。
  const [sessionStatusById, setSessionStatusById] = useState<
    Map<string, SessionDotStatus>
  >(new Map());
  const sessionStatusRef = useRef<Map<string, SessionDotStatus>>(new Map());
  const [agents, setAgents] = useState<AgentProfile[]>([]);
  const [agentsReady, setAgentsReady] = useState(false);
  const [agentId, setAgentId] = useState(() => loadLastAgentId() ?? "claude");
  const handleUserSelectAgent = useCallback((id: string) => {
    setAgentId(id);
    saveLastAgentId(id);
  }, []);
  const [mode, setMode] = useState<Mode>("normal");
  const [runtimeDetect, setRuntimeDetect] = useState<RuntimeDetect | undefined>(
    undefined,
  );
  const [done, setDone] = useState<{
    cost_usd: number | null;
    output_tokens: number | null;
    elapsed_sec: number | null;
  } | null>(null);
  const [sessionUsage, setSessionUsage] = useState<SessionUsage>({
    input: 0,
    output: 0,
  });
  const flushRafRef = useRef<number | null>(null);
  const loadingSessionsRef = useRef<Set<string>>(new Set());
  const [loadingSessionIds, setLoadingSessionIds] = useState<Set<string>>(
    new Set(),
  );
  const [sessions, setSessions] = useState<Session[]>([]);
  const [currentId, setCurrentId] = useState<string | null>(null);
  const [continuationParentId, setContinuationParentId] = useState<
    string | null
  >(null);
  const [continuationDrafts, setContinuationDrafts] = useState<
    Map<string, ContinuationDraftState>
  >(new Map());
  const continuationDraftsRef = useRef<Map<string, ContinuationDraftState>>(
    new Map(),
  );
  const continuationDraftGenerationRef = useRef<Map<string, number>>(new Map());
  const continuationDraftRequestIdRef = useRef<Map<string, string>>(new Map());
  const continuationRequestSeqRef = useRef(0);
  const continuationCancellationRef = useRef<Map<string, Promise<void>>>(
    new Map(),
  );
  const [continuationReadySessionIds, setContinuationReadySessionIds] =
    useState<Set<string>>(new Set());
  const [continuationStarting, setContinuationStarting] = useState(false);
  const [continuationAssemblingId, setContinuationAssemblingId] = useState<
    string | null
  >(null);
  const continuationAssembleSeqRef = useRef(0);
  const teamCfg = useTeamConfig(currentId ?? "");
  const [draftTeamCfg, setDraftTeamCfg] = useState<TeamConfig>({
    leadId: null,
    rosterIds: [],
  });
  const [review, setReview] = useState<ReviewResult | null>(null);
  const reviewCacheRef = useRef(new Map<string, ReviewResult | null>());
  const reviewRequestGenerationRef = useRef(0);
  const [undoFeedback, setUndoFeedback] = useState<
    Map<string, UndoResultRecord>
  >(() => new Map());
  const [undoTarget, setUndoTarget] = useState<{
    sessionId: string;
    runId: string;
  } | null>(null);
  const [sidebarOpen, setSidebarOpen] = useState(true);
  const [aboutOpen, setAboutOpen] = useState(false);
  const [rightPanelOpen, setRightPanelOpen] = useState(false);
  const [rightPanelTab, setRightPanelTab] = useState<RightPanelTab | null>(
    null,
  );
  const rightPanelTabRef = useRef<RightPanelTab | null>(rightPanelTab);
  useLayoutEffect(() => {
    rightPanelTabRef.current = rightPanelTab;
  }, [rightPanelTab]);
  const [previewPath, setPreviewPath] = useState<string | null>(null);
  const [previewSessionId, setPreviewSessionId] = useState<string | null>(null);
  const tabBeforePreviewRef = useRef<RightPanelTab | null>(null);
  const [lightbox, setLightbox] = useState<{
    path: string;
    sessionId: string | null;
  } | null>(null);
  const [rightPanelExpanded, setRightPanelExpanded] = useState(false);
  const [showTaskList, setShowTaskList] = useState(false);
  const [drillRun, setDrillRun] = useState<{
    runId: string;
    assignmentId: string;
  } | null>(null);
  const [inspectorTarget, setInspectorTarget] = useState<string | null>(null);
  const tabBeforeDrillRef = useRef<RightPanelTab | null>(null);
  const [goalExpanded, setGoalExpanded] = useState(false);
  // activeRepoId NULL = 默认 session 心智（无关联项目）
  // view：overview 总览 / session 会话 / intro 项目简介（设置 + 管理仓库已迁 settingsOpen overlay sheet·不在 AppView）
  // cluster L Phase 2 plan B Task 1：namespace 模型升级 + allRepos 给 NamespaceDropdown 算 count
  const [namespaces, setNamespaces] = useState<NamespaceMeta[]>([]);
  const [activeNamespaceId, setActiveNamespaceId] = useState<string>("local");
  const [reposInActiveNs, setReposInActiveNs] = useState<RepoMeta[]>([]);
  const [allRepos, setAllRepos] = useState<RepoMeta[]>([]);
  const [connectError, setConnectError] = useState<string | null>(null);
  const [editingRepo, setEditingRepo] = useState<RepoMeta | null>(null);
  const [ghAccounts, setGhAccounts] = useState<GhAccount[]>([]);
  const [selectedLogin, setSelectedLogin] = useState("");
  const [repoCacheByLogin, setRepoCacheByLogin] = useState<
    Record<string, RepoCacheEntry>
  >(() => readPersistedRepoCache());
  const repoCacheByLoginRef =
    useRef<Record<string, RepoCacheEntry>>(repoCacheByLogin);
  const [search, setSearch] = useState("");
  const [filter, setFilter] = useState<RepoFilter>("all");
  const [selectedByLogin, setSelectedByLogin] = useState<
    Record<string, Set<RepoKey>>
  >({});
  const [cloneProgress, setCloneProgress] = useState<
    Record<RepoKey, CloneProgressEntry>
  >({});
  const cloneProgressRef = useRef<Record<RepoKey, CloneProgressEntry>>({});
  const cloneSettleTimersRef = useRef<
    Record<string, ReturnType<typeof setTimeout>>
  >({});
  const setCloneProgressEntries = useCallback(
    (
      updater: (
        prev: Record<RepoKey, CloneProgressEntry>,
      ) => Record<RepoKey, CloneProgressEntry>,
    ) => {
      setCloneProgress((prev) => {
        const next = updater(prev);
        if (next === prev) return prev;
        cloneProgressRef.current = next;
        return next;
      });
    },
    [],
  );
  const [gitInstalled, setGitInstalled] = useState<boolean | null>(null);
  const [ghInstalled, setGhInstalled] = useState<boolean | null>(null);
  const [ghAccountError, setGhAccountError] = useState<string | undefined>();
  const repoToolsLoadedRef = useRef(false);
  const repoToolsLoadingRef = useRef(false);
  const [installState, setInstallState] = useState<InstallState>({
    installing: false,
  });
  const [canBrew, setCanBrew] = useState(false);
  const [repoGroupExpanded, setRepoGroupExpanded] = useState<
    Record<string, boolean>
  >({});
  void activeNamespaceId;
  const [activeRepoId, setActiveRepoId] = useState<string | null>(
    "local-default",
  );
  const [view, setView] = useState<AppView>("session");
  const viewRef = useRef<AppView>("session");
  const navHistoryRef = useRef<AppRoute[]>([]);
  const navIndexRef = useRef(-1);
  const pendingNavKeyRef = useRef<string | null>(null);
  const [navState, setNavState] = useState({
    canGoBack: false,
    canGoForward: false,
  });

  const syncNavState = useCallback(() => {
    const index = navIndexRef.current;
    const length = navHistoryRef.current.length;
    setNavState({
      canGoBack: index > 0,
      canGoForward: index >= 0 && index < length - 1,
    });
  }, []);

  const commitNavRoute = useCallback(
    (route: AppRoute) => {
      const key = appRouteKey(route);
      const history = navHistoryRef.current;
      const index = navIndexRef.current;
      if (index >= 0 && appRouteKey(history[index]) === key) {
        syncNavState();
        return;
      }

      const base = history.slice(0, Math.max(index + 1, 0));
      const next = [...base, route].slice(-NAV_HISTORY_LIMIT);
      navHistoryRef.current = next;
      navIndexRef.current = next.length - 1;
      syncNavState();
    },
    [syncNavState],
  );

  useEffect(() => {
    const route = routeFromState(
      view,
      currentId,
      activeNamespaceId,
      activeRepoId,
    );
    if (route.view === "session" && !route.sessionId) return;
    const key = appRouteKey(route);
    const pendingKey = pendingNavKeyRef.current;
    if (pendingKey) {
      if (pendingKey === key) {
        pendingNavKeyRef.current = null;
        syncNavState();
      }
      return;
    }
    commitNavRoute(route);
  }, [
    activeNamespaceId,
    activeRepoId,
    commitNavRoute,
    currentId,
    syncNavState,
    view,
  ]);

  function navigateHistory(delta: -1 | 1) {
    const history = navHistoryRef.current;
    let nextIndex = navIndexRef.current + delta;

    // 循环查找有效条目（保险二：跳过已删除/已归档会话）
    while (nextIndex >= 0 && nextIndex < history.length) {
      const route = history[nextIndex];

      // 检查 session 路由条目是否有效
      if (route.view === "session" && route.sessionId) {
        const session = sessions.find((s) => s.id === route.sessionId);
        // 会话不存在或已归档 -> 跳过，继续同方向找下一条
        if (!session || session.archived) {
          nextIndex += delta;
          continue;
        }
      }

      // 找到有效条目
      navIndexRef.current = nextIndex;
      pendingNavKeyRef.current = appRouteKey(route);
      syncNavState();

      if (route.view === "session") {
        if (route.sessionId) void openSession(route.sessionId);
        return;
      }
      if (route.namespaceId) {
        setActiveNamespaceId(route.namespaceId);
        setActiveRepoId(route.repoId);
        setReposInActiveNs(
          allRepos.filter((repo) => repo.namespace_id === route.namespaceId),
        );
        setRepoGroupExpanded(route.repoId ? { [route.repoId]: true } : {});
      }
      setView(route.view);
      return;
    }

    // 边界没有有效条目 -> 原地不动
  }
  // max（rightPanelExpanded）是「当前所看内容」的视图内临时态：
  // 切视图（view）或切会话（currentId）即退出 max，切回不自动恢复。
  // settingsOpen 为 overlay（非导航·设置 sheet 盖在上层）故不纳入依赖：
  // 关掉设置后右面板仍保持原 max 态。
  useLayoutEffect(() => {
    viewRef.current = view;
  }, [view]);
  useEffect(() => {
    setRightPanelExpanded(false);
    setUndoTarget(null);
    setUndoFeedback(new Map());
  }, [view, currentId]);
  const [installGuideDismissed, setInstallGuideDismissed] = useState(false);
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [newProjectOpen, setNewProjectOpen] = useState(false);
  const [settingsPage, setSettingsPage] = useState<SettingsPage>("agents");
  const openSettings = useCallback((page: SettingsPage = "agents") => {
    setInstallGuideDismissed(true);
    setSettingsPage(page);
    setSettingsOpen(true);
  }, []);
  const [invalidDialog, setInvalidDialog] = useState<{
    repoId: string;
    kind: "invalid" | "archived";
  } | null>(null);
  const [deleteTarget, setDeleteTarget] = useState<{
    id: string;
    title: string;
  } | null>(null);
  const [removeProjectTarget, setRemoveProjectTarget] = useState<{
    id: string;
    name: string;
  } | null>(null);
  const [groups, setGroups] = useState<GroupMeta[]>([]);
  const [groupExpanded, setGroupExpanded] = useState<Record<string, boolean>>(
    {},
  );
  const onToggleGroup = (id: string) =>
    setGroupExpanded((m) => ({ ...m, [id]: !(m[id] ?? true) }));
  const [groupDeleteTarget, setGroupDeleteTarget] = useState<{
    id: string;
    name: string;
  } | null>(null);
  const [toast, setToast] = useState<string | null>(null);
  const currentIdRef = useRef<string | null>(null);
  const sessionsRef = useRef(sessions);
  const stickyDoneRef = useRef<string | null>(null);
  const initedRef = useRef(false);
  const repoListRequestRef = useRef(0);
  const autoRepoRefreshAtRef = useRef<Record<string, number>>({});

  const messages = currentId ? (messagesBySession.get(currentId) ?? []) : [];
  const liveRunsByRun = useMemo((): Record<string, TeamRun> => {
    const runMap = teamRunsBySession.get(currentId ?? "");
    if (!runMap) return {};
    return Object.fromEntries(
      [...runMap.entries()].filter(
        ([, run]) =>
          !persistedRunsRef.current.has(`${currentId ?? ""}:${run.run_id}`),
      ),
    );
  }, [teamRunsBySession, currentId]);
  const liveGateMsgs = useMemo((): (ChatMessage & { id: string })[] => {
    const view = gateBySession.get(currentId ?? "");
    if (!view) return [];
    const block: Block =
      view.kind === "draft" || view.kind === "proposing"
        ? { type: "gate_card", session_id: currentId ?? "" }
        : { type: "draft_failed", session_id: currentId ?? "" };
    return [
      {
        id: crypto.randomUUID(),
        role: "assistant",
        content: [block],
        engine: "agent-team",
      },
    ];
  }, [gateBySession, currentId]);
  const liveCodingByRun = useMemo((): Record<string, CodingTaskBlock> => {
    if (!currentId) return {};
    return Object.fromEntries(
      [...codingBlocksByRun.entries()].filter(
        ([runId]) => codingLoopsRef.current.get(runId)?.sessionId === currentId,
      ),
    );
  }, [codingBlocksByRun, currentId]);
  const displayMessages = useMemo(() => {
    if (!currentId) return messages;
    const liveCodingRunIds = new Set(
      [...codingBlocksByRun.keys()].filter(
        (rid) => codingLoopsRef.current.get(rid)?.sessionId === currentId,
      ),
    );
    return [
      ...suppressBlockBShells(
        withRunCardStates(
          messages,
          runStatesBySession.get(currentId),
          undoFeedback,
          currentId,
        ),
        liveCodingRunIds,
      ),
      ...liveGateMsgs,
    ];
  }, [
    currentId,
    messages,
    runStatesBySession,
    undoFeedback,
    codingBlocksByRun,
    liveGateMsgs,
  ]);
  const pendingDecision = useMemo((): DecisionCardBlock | null => {
    for (
      let messageIndex = messages.length - 1;
      messageIndex >= 0;
      messageIndex--
    ) {
      const content = messages[messageIndex].content;
      for (let blockIndex = content.length - 1; blockIndex >= 0; blockIndex--) {
        const block = content[blockIndex];
        if (block.type === "decision_card") {
          return block.status === "pending" ? block : null;
        }
      }
    }
    return null;
  }, [messages]);
  const currentGoalData = useMemo<{
    goal: GoalContract | null;
    members: MemberUnit[];
    runId: string;
  } | null>(() => {
    const runMap = teamRunsBySession.get(currentId ?? "");
    if (runMap && runMap.size > 0) {
      const entries = [...runMap.entries()];
      const [rid, latest] = entries[entries.length - 1];
      return { goal: latest.goal, members: latest.members, runId: rid };
    }

    for (let i = messages.length - 1; i >= 0; i--) {
      const blocks = messages[i].content;
      for (let j = blocks.length - 1; j >= 0; j--) {
        const block = blocks[j];
        if (block.type === "team_run") {
          return {
            goal: block.goal,
            members: block.members,
            runId: block.run_id,
          };
        }
      }
    }

    // team_run 优先；无 team_run 但有冻结 gate 契约（B2·真 fan-out 前）→ 用冻结契约
    // （criteria 已是 freeze 后 DB 回读·id 一致·目标条带/验收清单可正确渲 + waive 命中 DB 行）
    const frozen = frozenGoalBySession.get(currentId ?? "");
    if (frozen) return { goal: frozen.goal, members: [], runId: frozen.runId };
    const orch = orchestratedGoalSource(
      messages,
      t("dispatchCard.goalFallback"),
    );
    if (orch) return orch;
    // 会话级 goal 末位兜底（dispatch/team_run/frozen/orchestrated 链优先·队长直接回复也显目标条）
    const teamGoal = null; // team/frozen/orch all returned above already
    const sg = sessionGoalBySession.get(currentId ?? "") ?? null;
    const picked = pickDisplayGoal(teamGoal, sg);
    if (picked) return { goal: picked, members: [], runId: "" };
    return null;
  }, [
    teamRunsBySession,
    currentId,
    messages,
    frozenGoalBySession,
    sessionGoalBySession,
    t,
  ]);
  const goalMembers = currentGoalData?.members ?? [];
  const memberRunning = useMemo(
    () => hasRunningDispatchCard(messages),
    [messages],
  );
  const isOrchestratedRun = useMemo(
    () => latestDispatchRunIds(messages).length > 0,
    [messages],
  );
  const currentDisplayGoal = useMemo<GoalContract | null>(() => {
    if (!currentGoalData?.goal) return null;
    const persistedCriteria = acceptanceByRun.get(currentGoalData.runId);
    const criteria =
      persistedCriteria && persistedCriteria.length > 0
        ? persistedCriteria
        : currentGoalData.goal.criteria;
    const titleRunIds = latestDispatchRunIds(messages);
    const goalTitle =
      goalTitleByRun.get(currentGoalData.runId) ??
      titleRunIds.map((r) => goalTitleByRun.get(r)).find(Boolean) ??
      currentGoalData.goal.goal_title;
    return { ...currentGoalData.goal, criteria, goal_title: goalTitle };
  }, [acceptanceByRun, currentGoalData, goalTitleByRun, messages]);
  const goalRunComplete =
    goalMembers.length > 0 &&
    goalMembers.every(
      (member) =>
        member.status === "done" ||
        member.status === "failed" ||
        member.status === "stopped",
    );
  const goalRunActive = goalMembers.length > 0 && !goalRunComplete;
  const goalRunHasMemberFailure = goalMembers.some(
    (member) => member.status === "failed" || member.status === "stopped",
  );
  const goalTotalTokens = goalMembers.reduce(
    (total, member) => total + member.input_tokens + member.output_tokens,
    0,
  );
  const goalTotalCostUsd = goalMembers.some(
    (member) => member.cost_usd !== null,
  )
    ? goalMembers.reduce((total, member) => total + (member.cost_usd ?? 0), 0)
    : null;
  const goalPanel = currentDisplayGoal ? (
    <GoalCriteriaPanel
      goal={currentDisplayGoal}
      totalTokens={goalTotalTokens}
      totalCostUsd={goalTotalCostUsd}
      runId={currentGoalData?.runId}
      onWaive={(criterionId, reason) => {
        const rid = currentGoalData?.runId;
        if (!rid || !currentId) return;
        invoke("waive_acceptance", {
          sessionId: currentId,
          runId: rid,
          criterionId,
          reason,
        })
          .then(() =>
            invoke<AcceptanceCriterion[]>("list_acceptance", {
              sessionId: currentId,
              runId: rid,
            }),
          )
          .then((cs) =>
            setAcceptanceByRun((prev) => {
              const next = new Map(prev);
              next.set(rid, cs);
              return next;
            }),
          )
          .catch((e) => setToast(String(e)));
      }}
    />
  ) : undefined;
  const currentInterruptedRuns = useMemo(
    () =>
      (interruptedRunsBySession.get(currentId ?? "") ?? []).filter(
        (run) => !dismissedInterruptedRuns.has(run.run_id),
      ),
    [currentId, dismissedInterruptedRuns, interruptedRunsBySession],
  );
  const dismissInterruptedRun = useCallback(
    (runId: string) =>
      setDismissedInterruptedRuns((prev) => {
        const next = new Set(prev);
        next.add(runId);
        return next;
      }),
    [],
  );
  const busy = currentId !== null && runningSessions.has(currentId);
  const refreshGhAccounts = useCallback(async (): Promise<GhAccount[]> => {
    setGhAccountError(undefined);
    setGhInstalled((current) => (current === false ? false : null));
    try {
      const xs = await invoke<GhAccount[]>("gh_accounts");
      const accounts = xs ?? [];
      setGhAccounts(accounts);
      setGhInstalled(true);
      return accounts;
    } catch (e) {
      setGhAccounts([]);
      setGhAccountError(String(e));
      setGhInstalled(String(e).includes("GH_MISSING") ? false : true);
      return [];
    }
  }, []);
  const loadRepoTools = useCallback(async (force = false) => {
    if (repoToolsLoadingRef.current) return;
    if (repoToolsLoadedRef.current && !force) return;
    repoToolsLoadingRef.current = true;
    if (force) {
      setGitInstalled(null);
      setGhInstalled(null);
    }
    setGhAccountError(undefined);
    try {
      const [git, gh] = await Promise.all([
        invoke<DetectResult>("detect_git").catch(() => null),
        invoke<DetectResult>("detect_gh").catch(() => null),
      ]);
      const gitAvailable = Boolean(git?.available);
      const ghAvailable = Boolean(gh?.available);
      let accounts: GhAccount[] = [];
      let accountError: string | undefined;
      let brewAvailable = false;

      if (gitAvailable && ghAvailable) {
        try {
          accounts = (await invoke<GhAccount[]>("gh_accounts")) ?? [];
        } catch (e) {
          accountError = String(e);
        }
      } else if (!ghAvailable) {
        brewAvailable = await invoke<boolean>("detect_brew").catch(() => false);
      }

      setGitInstalled(gitAvailable);
      setGhInstalled(ghAvailable);
      setGhAccounts(accounts);
      setGhAccountError(accountError);
      setCanBrew(brewAvailable);
      repoToolsLoadedRef.current = true;
    } finally {
      repoToolsLoadingRef.current = false;
    }
  }, []);
  const updateRepoCacheByLogin = useCallback(
    (
      updater: (
        prev: Record<string, RepoCacheEntry>,
      ) => Record<string, RepoCacheEntry>,
    ) => {
      const next = updater(repoCacheByLoginRef.current);
      if (next === repoCacheByLoginRef.current) return;
      repoCacheByLoginRef.current = next;
      setRepoCacheByLogin(next);
    },
    [],
  );
  const loadRepoList = useCallback(
    async (login: string, options: LoadRepoListOptions = {}) => {
      if (!login) return;
      const current = repoCacheByLoginRef.current[login];
      if (
        !options.force &&
        (current?.status === "loading" || current?.status === "refreshing")
      ) {
        return;
      }

      const requestId = ++repoListRequestRef.current;
      const fetchStartGen = current?.mutationGen ?? 0;
      updateRepoCacheByLogin((prev) => {
        const previous = prev[login];
        return {
          ...prev,
          [login]: {
            ...previous,
            status: previous?.repos ? "refreshing" : "loading",
            error: undefined,
            requestId,
            mutationGen: previous?.mutationGen ?? fetchStartGen,
          },
        };
      });

      try {
        const repos = await invoke<RemoteRepo[]>("gh_repo_list", { login });
        const latest = repoCacheByLoginRef.current[login];
        if (!latest || latest.requestId !== requestId) return;

        const merged = mergeRefresh(
          latest,
          repos ?? [],
          fetchStartGen,
          cloneProgressRef.current,
        );
        if (!merged) return;

        updateRepoCacheByLogin((prev) => ({ ...prev, [login]: merged }));
        setSelectedByLogin((prev) => {
          const selected = prev[login];
          if (!selected) return prev;
          const pruned = pruneSelection(selected, merged.repos ?? []);
          if (sameRepoSelection(selected, pruned)) return prev;
          return { ...prev, [login]: pruned };
        });
      } catch (e) {
        const latest = repoCacheByLoginRef.current[login];
        if (!latest || latest.requestId !== requestId) return;
        const message = normalizeRepoListError(e, login);
        if (String(e).includes("GH_MISSING")) {
          setGhInstalled(false);
        }
        updateRepoCacheByLogin((prev) => {
          const previous = prev[login];
          if (!previous || previous.requestId !== requestId) return prev;
          return {
            ...prev,
            [login]: {
              ...previous,
              status: "error",
              error: message,
            },
          };
        });
      }
    },
    [updateRepoCacheByLogin],
  );
  const loading = currentId !== null && loadingSessionIds.has(currentId);
  const composerBusy = deriveComposerBusy({
    sessionRunning: busy,
    loading,
    memberRunning,
  });
  const currentRun = currentId ? runningSessions.get(currentId) : undefined;
  // 是否 in-place 由后端按会话真实绑定透出，不再用 namespace.kind 猜。
  const currentSession = currentId
    ? (sessions.find((s) => s.id === currentId) ?? null)
    : null;
  const currentSessionRepoName =
    currentSession?.repo_id != null
      ? (allRepos.find((repo) => repo.id === currentSession.repo_id)?.name ??
        null)
      : null;
  const activeRepoMeta =
    reposInActiveNs.find((repo) => repo.id === activeRepoId) ?? null;
  // session-hover-menu §6.1：活动视图派生（render 期·传子组件用）
  const activeSessions = sessions.filter((s) => !s.archived);
  const currentSessionIsLocal = (() => {
    const nsId = currentSession?.namespace_id ?? "local";
    const ns = namespaces.find((n) => n.id === nsId);
    return ns ? ns.kind === "local" : nsId === "local";
  })();
  function getSessionReadonlyReason(id: string | null | undefined) {
    if (!id) return null;
    const session = sessions.find((s) => s.id === id);
    return session?.continued_to_session_id ||
      sessions.some((s) => s.parent_session_id === id)
      ? t("composer.readonly.continued")
      : null;
  }

  function guardReadonlySession(id: string | null | undefined) {
    const reason = getSessionReadonlyReason(id);
    if (!reason) return false;
    setToast(reason);
    return true;
  }

  const currentSessionReadonlyReason = getSessionReadonlyReason(currentId);
  // 镜像到 ref·供 deps=[] 的 agent-event 监听器按事件所属会话读最新事实。
  sessionsRef.current = sessions;
  const workingTokens =
    currentRun?.workingTokens != null && currentRun.workingTokens > 0
      ? currentRun.workingTokens
      : null;

  function setRun(
    sid: string,
    info: RunInfo | null,
    options?: { preserveStopGate?: boolean },
  ) {
    const current = runningSessionsRef.current.get(sid);
    if (!info && !options?.preserveStopGate) {
      const stopIssuedAt = stopIssuedAtRef.current.get(sid);
      // 后继 run 自己收尾后，停止时间闸已无残余价值；旧 run 的终态则保留闸，
      // 继续防它可能迟到的空标识 closeout。真实 run_id 闸需契约扩展，留后续。
      if (
        stopIssuedAt !== undefined &&
        current !== undefined &&
        current.startedAt > stopIssuedAt
      ) {
        stopIssuedAtRef.current.delete(sid);
      }
    }
    const next = new Map(runningSessionsRef.current);
    if (info) next.set(sid, info);
    else next.delete(sid);
    runningSessionsRef.current = next;
    setRunningSessions(next);
    // 每处「开跑」都落 setRun(sid, {...})——统一在此标 running·别在各调用点重复标。
    // setRun(sid, null) 不在此清态：是否落 attention/done 由具体终态事件决定（见 agent-event 监听器）。
    if (info) setSessionDotStatus(sid, "running");
  }

  function setSessionDotStatus(sid: string, status: SessionDotStatus | null) {
    const next = new Map(sessionStatusRef.current);
    if (status) next.set(sid, status);
    else next.delete(sid);
    sessionStatusRef.current = next;
    setSessionStatusById(next);
  }

  function setSessionMessages<T extends ChatMessage>(sid: string, msgs: T[]) {
    const next = new Map(messagesRef.current);
    next.set(sid, msgs);
    messagesRef.current = next;
    setMessagesBySession(next);
  }

  const clearStaleMemberCards = useCallback(
    (sid: string | null = currentIdRef.current) => {
      if (!sid) return;
      setSessionMessages(
        sid,
        clearStaleRunningDispatchCards(messagesRef.current.get(sid) ?? []),
      );
    },
    [],
  );

  useEffect(() => {
    if (!memberRunning || busy || currentId === null) return;
    const sessionId = currentId;
    return startMemberIdlePoll({
      checkRunning: () =>
        invoke<boolean>("is_team_session_running", { sessionId }),
      onIdle: () => clearStaleMemberCards(sessionId),
    });
  }, [memberRunning, busy, currentId, clearStaleMemberCards]);

  const upsertCodingTaskBlock = (runId: string, blk: CodingTaskBlock) => {
    setCodingBlocksByRun((prev) => {
      const next = new Map(prev);
      next.set(runId, blk);
      return next;
    });
  };

  const removeCodingTaskBlock = (runId: string) => {
    setCodingBlocksByRun((prev) => {
      const next = new Map(prev);
      next.delete(runId);
      return next;
    });
  };

  const detailForCodingState = (
    s: CodingState,
    errDetail?: string,
  ): string | null => {
    if (s.phase === "applied") {
      return s.landedHead
        ? t("app.coding.appliedWithHead", {
            head: s.landedHead.slice(0, 8),
          })
        : t("app.coding.applied");
    }
    // b2b 关自动落地：merge 进 staging 后停在 applying·改动在隔离区·还没落地·等用户点改动条。
    // 措辞绝不能写「已落地」（此刻还没落地）。
    if (s.phase === "applying") return t("app.coding.awaitingDelivery");
    if (s.phase === "shelved") return null;
    // T7：trust-land 下「无验证命令」不再阻断落地（T1/T4）。landing_blocked 现仅来自真实安全拦截
    // （受保护路径 / ff 冲突·见 isLandingBlockedError）·故文案统一为「安全检查未通过」·不再提验证命令。
    if (s.phase === "landing_blocked")
      return errDetail
        ? renderBackendError(errDetail, t)
        : t("app.coding.landingBlocked");
    if (s.phase === "error")
      return errDetail
        ? renderBackendError(errDetail, t)
        : t("app.coding.error");
    if (s.phase === "verify_failed") {
      return s.verifyCmd || null;
    }
    return null;
  };

  const blockFromCodingState = (
    runId: string,
    s: CodingState,
    errDetail?: string,
  ): CodingTaskBlock => {
    const meta = codingLoopDisplayRef.current.get(runId);
    return {
      type: "coding_task",
      run_id: runId,
      assignment_id: s.assignmentId,
      worker_name: meta?.worker_name ?? "worker",
      phase: s.phase,
      step_done: meta?.step_done,
      step_total: meta?.step_total,
      artifact_id: s.artifactId,
      verify_cmd: s.verifyCmd,
      detail: detailForCodingState(s, errDetail),
      lead_rationale: leadRationaleByRunRef.current.get(runId),
    };
  };

  const driveCodingLoop = async (runId: string) => {
    let s = codingLoopsRef.current.get(runId);
    if (!s) return;
    let terminalErrorDetail: string | undefined;
    try {
      while (true) {
        const action = nextCodingAction(s);
        if (action.kind === "wait" || action.kind === "done") break;
        s = await advanceCodingLoop(s, invoke);
        codingLoopsRef.current.set(runId, s);
        upsertCodingTaskBlock(runId, blockFromCodingState(runId, s));
      }
    } catch (e) {
      terminalErrorDetail = String(e);
      s = {
        ...s,
        phase: isLandingBlockedError(e) ? "landing_blocked" : "error",
      };
      codingLoopsRef.current.set(runId, s);
      upsertCodingTaskBlock(
        runId,
        blockFromCodingState(runId, s, terminalErrorDetail),
      );
    }

    // b2b 关自动落地：repo 会话 finalize→merge 后停在 applying（非 terminal）·改动在隔离区·
    // 等用户点改动条才落地。这里固化「待你决定」coding_task 卡 + 发队长停隔离区叙事·
    // 但【保留】run 在 codingLoopsRef（与 terminal 删相反·改动条/后续「提交」要靠它寻址）。
    if (s.phase === "applying") {
      const block = blockFromCodingState(runId, s);
      const stagedLeadId =
        teamRunsRef.current.get(s.sessionId)?.get(runId)?.lead ?? null;
      const stagedLeadName = stagedLeadId
        ? (agentNameSnapshotFor(stagedLeadId) ?? null)
        : null;
      const arr = messagesRef.current.get(s.sessionId) ?? [];
      const narration = t("app.coding.awaitingDeliveryNarration");
      // (d) 同步批次：把「固化卡 + 队长叙事加进持久 messages」与「从 live 容器删 coding_task block」
      // 放在同一同步批次（await 之前）·防 live block 与持久 block 瞬时重复/丢失（对齐 terminal 块纪律）。
      setSessionMessages(s.sessionId, [
        ...arr,
        {
          id: crypto.randomUUID(),
          role: "assistant",
          content: [block],
          engine: "agent-team",
          agent_id: null,
          agent_name_snapshot: null,
        },
        {
          id: crypto.randomUUID(),
          role: "assistant",
          content: [{ type: "text", text: narration }],
          engine: stagedLeadId ?? "agent-team",
          agent_id: stagedLeadId,
          agent_name_snapshot: stagedLeadName,
        },
      ]);
      removeCodingTaskBlock(runId);
      codingLoopDisplayRef.current.delete(runId);
      // (c) 不删 codingLoopsRef.current —— run 留着供改动条/「提交」寻址。
      await invoke("append_message", {
        sessionId: s.sessionId,
        role: "assistant",
        content: [block],
        engine: "agent-team",
        agentId: null,
        agentNameSnapshot: null,
      }).catch((e) =>
        console.error("[coding-loop] 停隔离区 coding_task append 失败", e),
      );
      await invoke("append_message", {
        sessionId: s.sessionId,
        role: "assistant",
        content: [{ type: "text", text: narration }],
        engine: stagedLeadId ?? "agent-team",
        agentId: stagedLeadId,
        agentNameSnapshot: stagedLeadName,
      }).catch((e) =>
        console.error("[coding-loop] 停隔离区叙事 append 失败", e),
      );
      return;
    }

    const terminal =
      s.phase === "applied" ||
      s.phase === "shelved" ||
      s.phase === "landing_blocked" ||
      s.phase === "error";
    if (terminal) {
      const block = blockFromCodingState(runId, s, terminalErrorDetail);
      const arr = messagesRef.current.get(s.sessionId) ?? [];
      setSessionMessages(s.sessionId, [
        ...arr,
        {
          id: crypto.randomUUID(),
          role: "assistant",
          content: [block],
          engine: "agent-team",
          agent_id: null,
          agent_name_snapshot: null,
        },
      ]);
      // 终态：把「加进持久 messages」与「从 live 容器删」放在同一同步批次（await 之前）·
      // 否则 await append_message 让出时 React 先 commit 持久 block·而 live block 尚未删·
      // 同一终态卡片瞬时重复渲染（opus 对抗审 P3 修）。
      removeCodingTaskBlock(runId);
      codingLoopsRef.current.delete(runId);
      codingLoopDisplayRef.current.delete(runId);
      await invoke("append_message", {
        sessionId: s.sessionId,
        role: "assistant",
        content: [block],
        engine: "agent-team",
        agentId: null,
        agentNameSnapshot: null,
      }).catch((e) =>
        console.error("[coding-loop] 终态 coding_task append 失败", e),
      );
      // 块 B（T3·P1-4）：applied/shelved 完成态补发 lead verdict（error 不补·诚实呈现 coding error 卡）。
      if (
        s.phase === "applied" ||
        s.phase === "shelved" ||
        s.phase === "landing_blocked"
      ) {
        const tr = teamRunsRef.current.get(s.sessionId)?.get(runId);
        if (tr && tr.members.length > 0) {
          const verdict: Block = buildCodingVerdictSummary(tr, {
            verifyCmd: s.verifyCmd,
            lastVerdict: s.lastVerdict,
            phase: s.phase,
          });
          const arr2 = messagesRef.current.get(s.sessionId) ?? [];
          setSessionMessages(s.sessionId, [
            ...arr2,
            {
              id: crypto.randomUUID(),
              role: "assistant",
              content: [verdict],
              engine: "agent-team",
              agent_id: null,
              agent_name_snapshot: tr.lead ?? null,
            },
          ]);
          await invoke("append_message", {
            sessionId: s.sessionId,
            role: "assistant",
            content: [verdict],
            engine: "agent-team",
            agentId: null,
            agentNameSnapshot: tr.lead ?? null,
          }).catch(() => {});
        }
      }
    }
  };

  // team 侧 start_team_run 统一错误呈现：对齐 solo 侧既有语义（App.tsx 内多处
  // `if (String(err).startsWith("SESSION_ALREADY_RUNNING:")) return;`）——占槽竞争是正常
  // 收敛态（如用户快速连点/另一路径同时起 run），不该原样把裸串 `SESSION_ALREADY_RUNNING:<sid>`
  // toast 给用户；其余错误照旧走 renderBackendError 人话化。
  function showStartTeamRunError(e: unknown) {
    const msg = String(e);
    if (msg.startsWith("SESSION_ALREADY_RUNNING:")) return;
    setToast(renderBackendError(msg, t));
  }

  function startTeamRunForSession(sid: string, text: string) {
    if (guardReadonlySession(sid)) return;
    const arr = messagesRef.current.get(sid) ?? [];
    if (arr.length === 0) {
      const title = deriveSessionTitle(text) || t("app.session.new");
      invoke("rename_session", { id: sid, title }).then(() =>
        refreshSessions(),
      );
    }
    setDone(null);
    const content = [{ type: "text" as const, text }];
    setSessionMessages(sid, [
      ...arr,
      { id: crypto.randomUUID(), role: "user", content },
    ]);
    invoke("append_message", {
      sessionId: sid,
      role: "user",
      content,
      engine: null,
      agentId: null,
      agentNameSnapshot: null,
    }).catch(() => {});
    let dispatchTargets: { runtime: RuntimeTeamConfig; agentIds: string[] };
    try {
      dispatchTargets = resolveDispatchTargets(sid, null);
    } catch {
      setToast(t("app.dispatch.noAvailableMembers"));
      return;
    }
    const lead =
      agentNameSnapshotFor(dispatchTargets.runtime.effectiveLeadId) ??
      dispatchTargets.runtime.effectiveLeadId;
    // M1b 最小派单：legacy = 当前选中 agent；saved team config = strict member pool 约束下的默认成员。
    // assignmentId 只需 run 内唯一（后端 run_id 组成 MemberKey）→ "a1" 即可·不预生成 runId。
    const members = dispatchTargets.agentIds.map((workerAgentId, index) => ({
      participantId: `worker-${index + 1}`,
      assignmentId: `a${index + 1}`,
      taskId: `task-${index + 1}`,
      agentId: workerAgentId, // 缝4：已配置 agent id → 后端 make_backend
      subtask: text,
    }));
    invoke<string>("start_team_run", {
      sessionId: sid,
      goal: text,
      lead,
      members,
    })
      .then((runId) => {
        leadAgentIdByRunRef.current.set(
          runId,
          dispatchTargets.runtime.effectiveLeadId,
        );
      })
      .catch(showStartTeamRunError);
  }

  // A 子片（spec §5）：tier0 自动派——从 propose 回传组装 N MemberInput + criteria·贯通 runId（spec §3.1）。
  // 前置条件 = 调用方已确认全部子任务有 assignee（allAssigned）并传入已 parse 的 assignments·criteria 与 members 同源 assigned 推导。
  function startTeamRunFromPropose(
    sid: string,
    r: ProposeResult,
    assignments: ParsedAssignment[],
  ) {
    if (guardReadonlySession(sid)) return;
    const assigned = assignments.filter((a) => a.assignee !== null);
    const members = assigned.map((a, i) => ({
      participantId: `worker-${i + 1}`,
      assignmentId: a.subtaskId,
      taskId: a.subtaskId,
      agentId: a.assignee!.agentId,
      subtask: a.subtask,
      // scope_files 暂不传（后端 MemberInput 无此字段·M2 接通）
    }));
    const criteria = assignmentsToCriteria(assigned).map((c) => ({
      id: `${r.runId}-${c.id}`,
      claim: c.claim,
      verifier: c.verifier,
      evidence: null,
      status: "pending" as const,
      scope: c.scope,
    }));
    const runtime = resolveRuntimeTeamConfig(sid);
    const lead =
      agentNameSnapshotFor(runtime.effectiveLeadId) ?? runtime.effectiveLeadId;
    invoke<string>("start_team_run", {
      sessionId: sid,
      goal: r.goal,
      lead,
      members,
      runId: r.runId,
      criteria,
    })
      .then((rid) => {
        leadAgentIdByRunRef.current.set(rid, runtime.effectiveLeadId);
      })
      .catch(showStartTeamRunError);
  }

  // F2b：冻结即派（Tier1「开始执行」/Tier2「确认并开跑」）——复用 A 子片派单路径·runId 贯通（spec §3.1）。
  // criteria 用 DB 回读 rows（含用户编辑后的真版本·id 与 DB 一致）·非 d.criteria。
  function startTeamRunFromDraft(
    sid: string,
    d: GateDraft,
    rows: AcceptanceCriterion[],
  ) {
    if (guardReadonlySession(sid)) return;
    const assigned = d.assignments.filter((a) => a.assignee !== null);
    const members = assigned.map((a, i) => ({
      participantId: `worker-${i + 1}`,
      assignmentId: a.subtaskId,
      taskId: a.subtaskId,
      agentId: a.assignee!.agentId,
      subtask: a.subtask,
      // scope_files 暂不传（后端 MemberInput 无此字段·M2 接通）
    }));
    const criteria = rows.map((c) => ({
      id: c.id,
      claim: c.claim,
      verifier: c.verifier ?? null,
      evidence: c.evidence ?? null,
      status: c.status,
      scope: c.scope,
    }));
    const runtime = resolveRuntimeTeamConfig(sid);
    const lead =
      agentNameSnapshotFor(runtime.effectiveLeadId) ?? runtime.effectiveLeadId;
    invoke<string>("start_team_run", {
      sessionId: sid,
      goal: d.goal,
      lead,
      members,
      runId: d.runId,
      criteria,
    })
      .then((rid) => {
        leadAgentIdByRunRef.current.set(rid, runtime.effectiveLeadId);
      })
      .catch(showStartTeamRunError);
  }

  // 只 propose + 灌 gate 的内核（不存 user 消息·不 rename）。正常送出走外壳·retry/redraft 直接走内核（user 消息已在）。
  function runProposeForSession(sid: string, goal: string) {
    if (guardReadonlySession(sid)) return;
    setDone(null);
    // 新一轮 propose：记 request seq（只 commit 最新·防慢请求覆盖）
    const mySeq = (gateReqSeqRef.current.get(sid) ?? 0) + 1;
    gateReqSeqRef.current.set(sid, mySeq);
    // A 子片（spec §3.1 第 3 点）：propose in-flight 即渲 proposing 卡（GUI #1 技术根因 = 此前无任何态可渲）
    setGateBySession((prev) => {
      const next = new Map(prev);
      next.set(sid, { kind: "proposing" });
      return next;
    });
    const runtime = resolveRuntimeTeamConfig(sid);
    // no saved lead → legacy floor = visible available agents；saved lead → strict saved member pool（[] 有效）。
    invoke<ProposeOutcome>("propose_team_plan", {
      sessionId: sid,
      leadId: runtime.effectiveLeadId,
      goal,
      repoContext: null,
      rosterAgentIds: runtime.memberPoolIds,
    })
      .then((outcome) => {
        if (gateReqSeqRef.current.get(sid) !== mySeq) return; // 已被更新的请求取代·丢弃旧结果
        if (outcome.outcome === "drafted" && outcome.tier === "tier0") {
          const assignments = parseAssignments(outcome.assignmentsJson);
          const allAssigned =
            assignments.length > 0 &&
            assignments.every((a) => a.assignee !== null);
          if (allAssigned) {
            // A 子片：tier0 自动派（draft 一闪即跑·卡清掉·live TeamRun block 接管渲进度）
            setGateBySession((prev) => {
              const next = new Map(prev);
              next.delete(sid);
              return next;
            });
            startTeamRunFromPropose(sid, outcome, assignments);
            return;
          }
          // 有子任务派不到 agent → 不自动派·落 draft 卡（诚实降级·卡上有未派提示·fall-through 到 draft 分支）
        }
        setGateBySession((prev) => {
          const next = new Map(prev);
          if (outcome.outcome === "drafted") {
            next.set(sid, { kind: "draft", draft: draftFromResult(outcome) });
          } else {
            const failure =
              outcome.failure.kind === "parseExhausted"
                ? {
                    ...outcome.failure,
                    lastError: renderBackendError(outcome.failure.lastError, t),
                  }
                : {
                    ...outcome.failure,
                    reason: renderBackendError(outcome.failure.reason, t),
                  };
            next.set(sid, {
              kind: "failed",
              failure,
              runId: "",
              contractId: "",
            });
          }
          return next;
        });
      })
      .catch((e) => {
        if (gateReqSeqRef.current.get(sid) !== mySeq) return;
        setGateBySession((prev) => {
          const next = new Map(prev);
          next.set(sid, {
            kind: "failed",
            failure: {
              kind: "invokeFailed",
              reason: renderBackendError(String(e), t),
            },
            runId: "",
            contractId: "",
          });
          return next;
        });
      });
  }

  function proposeTeamPlanForSession(sid: string, text: string) {
    const arr = messagesRef.current.get(sid) ?? [];
    if (arr.length === 0) {
      const title = deriveSessionTitle(text) || t("app.session.new");
      invoke("rename_session", { id: sid, title }).then(() =>
        refreshSessions(),
      );
    }
    const content = [{ type: "text" as const, text }];
    setSessionMessages(sid, [
      ...arr,
      { id: crypto.randomUUID(), role: "user", content },
    ]);
    invoke("append_message", {
      sessionId: sid,
      role: "user",
      content,
      engine: null,
      agentId: null,
      agentNameSnapshot: null,
    }).catch(() => {});
    runProposeForSession(sid, text);
  }
  void proposeTeamPlanForSession; // 决策7：team 入口已改道 lead_step·旧函数保留模块不删·暂抑制 unused

  function runLeadStepForSession(
    sid: string,
    text: string,
    config?: ComposerRuntimeConfig,
  ) {
    if (guardReadonlySession(sid)) return;
    const arr = messagesRef.current.get(sid) ?? [];
    if (arr.length === 0) {
      const title = deriveSessionTitle(text) || t("app.session.new");
      invoke("rename_session", { id: sid, title }).then(() =>
        refreshSessions(),
      );
    }
    // 用户消息只进 UI·不在此 append_message（避免与 reply 的 send_message 双写）。
    // lead_step 经 userMsg 参数把当前消息喂后端 digest（内存拼·不读 DB）·故无需先落库。
    setDone(null);
    setSessionMessages(sid, [
      ...arr,
      {
        id: crypto.randomUUID(),
        role: "user",
        content: [{ type: "text", text }],
      },
    ]);
    // 一次性算出本回合的 runtime：lead + 前端真正可派的成员池。
    // dispatchableMemberIds 喂后端·让 lead 看到的【可调度 worker】= 前端将要派的池（避免 lead 选到前端派不出去的 worker）。
    const runtime = resolveRuntimeTeamConfig(sid);
    const leadId = runtime.effectiveLeadId;
    // event_cursor = 每次发消息唯一稳定键·用 randomUUID（不可用 reload 会清零的内存序号·否则误判 Duplicate 吞消息）
    const cursor = crypto.randomUUID();
    setRun(sid, {
      startedAt: Date.now(),
      workingTokens: null,
      engine: leadId,
      agent_id: leadId,
      agent_name_snapshot: agentNameSnapshotFor(leadId) ?? null,
    });
    invoke<LeadStepOutcome>("lead_step", {
      sessionId: sid,
      leadAgentId: leadId,
      dispatchableMemberIds: runtime.memberPoolIds,
      lastEvent: "user_msg",
      eventCursor: cursor,
      userMsg: text,
      ...(config?.reasoningTier ? { reasoningTier: config.reasoningTier } : {}),
    })
      .then((outcome) => {
        // 决策7：team 模式不回旧 propose 路径·lead_step 失败走下面 catch（showLeadError）
        if (outcome.status === "duplicate") {
          setRun(sid, null);
          return;
        }
        setRun(sid, null);
        handleLeadOutcome(
          sid,
          leadId,
          text,
          outcome.action,
          outcome.decisionCard,
          config,
        );
      })
      .catch((e) => {
        setRun(sid, null);
        showLeadError(sid, String(e));
      });
  }

  function startLeadSessionForComposer(
    sid: string,
    text: string,
    config?: ComposerRuntimeConfig,
  ) {
    const arr = messagesRef.current.get(sid) ?? [];
    if (arr.length === 0) {
      const title = deriveSessionTitle(text) || t("app.session.new");
      invoke("rename_session", { id: sid, title }).then(() =>
        refreshSessions(),
      );
    }
    setDone(null);
    const runtime = resolveRuntimeTeamConfig(sid);
    const leadId = runtime.effectiveLeadId;
    const leadName = agentNameSnapshotFor(leadId);
    setSessionMessages(sid, [
      ...arr,
      {
        id: crypto.randomUUID(),
        role: "user",
        content: [{ type: "text", text }],
      },
      {
        id: crypto.randomUUID(),
        role: "assistant",
        content: [],
        engine: leadId,
        agent_id: leadId,
        agent_name_snapshot: leadName,
      },
    ]);
    setRun(sid, {
      startedAt: Date.now(),
      workingTokens: null,
      engine: leadId,
      agent_id: leadId,
      agent_name_snapshot: leadName,
    });
    invoke("start_lead_session", {
      sessionId: sid,
      leadAgentId: leadId,
      message: text,
      memberIds: runtime.memberPoolIds,
      ...(config?.reasoningTier ? { reasoningTier: config.reasoningTier } : {}),
    }).catch((e) => {
      setRun(sid, null);
      showLeadError(sid, String(e));
    });
  }

  // 非 reply 动作要手动落用户消息（reply 由 send_message 落·避免双写）
  function persistUserMsg(sid: string, text: string) {
    invoke("append_message", {
      sessionId: sid,
      role: "user",
      content: [{ type: "text", text }],
      engine: null,
      agentId: null,
      agentNameSnapshot: null,
    }).catch(() => {});
  }

  // agent_hint（模糊串如 "codex"）→ 真实 dispatch targets。
  // 有 hint：三路匹配 id/name/provider·必须唯一命中；未命中/多命中 = 失败关闭（throw·绝不 fallback 到首个）。
  // 无 hint：
  //   - lead 派单（leadDispatch=true）且池 > 1 → 失败关闭（throw·要求 lead 带 agent_hint·不默认取首个）；
  //   - 其余（手动/重派 path 或单成员池）→ 沿用池内默认成员，避免把一条笼统任务复制给所有 worker。
  // strict pool 为空时不伪造成员。
  function resolveDispatchTargets(
    sid: string,
    hint: string | null,
    opts?: { leadDispatch?: boolean },
  ): { runtime: RuntimeTeamConfig; agentIds: string[] } {
    const runtime = resolveRuntimeTeamConfig(sid);
    const pool = runtime.memberPoolIds;
    if (pool.length === 0) {
      throw new Error(t("app.dispatch.noAvailableMembers"));
    }
    const normalizedHint = hint?.trim() ?? "";
    if (normalizedHint) {
      const h = normalizedHint.toLowerCase();
      const matches = pool
        .map((agentId) => availableAgents.find((a) => a.id === agentId))
        .filter(
          (a): a is AgentProfile =>
            !!a &&
            (a.id.toLowerCase() === h ||
              a.name.toLowerCase() === h ||
              a.provider.toLowerCase() === h),
        );
      if (matches.length === 0) {
        throw new Error(
          t("app.dispatch.hintNotFound", { hint: normalizedHint }),
        );
      }
      if (matches.length > 1) {
        throw new Error(
          t("app.dispatch.hintAmbiguous", { hint: normalizedHint }),
        );
      }
      return { runtime, agentIds: [matches[0].id] };
    }
    if (opts?.leadDispatch && pool.length > 1) {
      throw new Error(t("app.dispatch.hintRequired"));
    }
    const fallback = defaultWorkerAgentId(runtime);
    if (!fallback) throw new Error(t("app.dispatch.noAvailableMembers"));
    return { runtime, agentIds: [fallback] };
  }

  // 复用单成员派单模式·goal=task·scope 注入 subtask（后端无 scope 字段）。
  function dispatchWorkerFromLead(
    sid: string,
    task: string,
    scopeFiles: string[],
    agentHint: string | null,
    rationale: string,
    goalTitle?: string | null,
  ): Promise<string> {
    if (guardReadonlySession(sid)) {
      return Promise.reject(new Error(t("composer.readonly.continued")));
    }
    let dispatchTargets: { runtime: RuntimeTeamConfig; agentIds: string[] };
    try {
      dispatchTargets = resolveDispatchTargets(sid, agentHint, {
        leadDispatch: true,
      });
    } catch (error) {
      return Promise.reject(error);
    }
    const lead =
      agentNameSnapshotFor(dispatchTargets.runtime.effectiveLeadId) ??
      dispatchTargets.runtime.effectiveLeadId;
    const subtask = scopeFiles.length
      ? t("app.dispatch.scopeSuggestion", {
          task,
          scope: scopeFiles.join(t("app.listSeparator")),
        })
      : task;
    const members = dispatchTargets.agentIds.map((workerAgentId, index) => ({
      participantId: `worker-${index + 1}`,
      assignmentId: `a${index + 1}`,
      taskId: `task-${index + 1}`,
      agentId: workerAgentId,
      subtask,
    }));
    return invoke<string>("start_team_run", {
      sessionId: sid,
      goal: task,
      lead,
      members,
      goalTitle: goalTitle ?? null,
    }).then((runId) => {
      leadAgentIdByRunRef.current.set(
        runId,
        dispatchTargets.runtime.effectiveLeadId,
      );
      leadRationaleByRunRef.current.set(runId, rationale);
      if (goalTitle) {
        setGoalTitleByRun((prev) => {
          const next = new Map(prev);
          next.set(runId, goalTitle);
          return next;
        });
      }
      return runId;
    });
  }

  // T5：lead 选 dispatch_worker → 不直接派·先出本地确认卡（澄清目标 + 子任务 + 派给谁）
  // + 把澄清目标 freeze 进 frozenGoalBySession（确认前即喂 topbar GoalBar·此刻还无 team_run 块）。
  function showDispatchConfirm(
    sid: string,
    action: Extract<LeadAction, { action: "dispatch_worker" }>,
  ) {
    let targetName: string;
    try {
      const targets = resolveDispatchTargets(sid, action.agent_hint, {
        leadDispatch: true,
      });
      const targetId = targets.agentIds[0];
      targetName = agentNameSnapshotFor(targetId) ?? targetId;
    } catch (error) {
      // 派单目标解析失败（无可派成员 / hint 不命中等）→ 直接报错·不出确认卡。
      setToast(String(error));
      return;
    }
    const subtask = action.scope_files.length
      ? t("app.dispatch.scopeSuggestion", {
          task: action.task,
          scope: action.scope_files.join(t("app.listSeparator")),
        })
      : action.task;

    // 澄清目标 freeze → topbar GoalBar（criteria 需 ≥1 条 GoalBar 才渲·这里据子任务合成一条草案验收）。
    // runId 同时充当本卡 source_run_id（per-card 唯一·防 React turn key 撞 + 让取消能精确清掉本卡合成的冻结目标）。
    const runId = `${LOCAL_DISPATCH_PREFIX}-${crypto.randomUUID()}`;
    const goal: GoalContract = {
      goal: action.task,
      // ③ GUI 修：派单确认阶段就喂 lead 产的短标题给 topbar（否则 topbar 退回显长指令·撑乱）。
      goal_title: action.goal_title ?? undefined,
      status: "frozen",
      criteria: [
        {
          id: `${runId}-c1`,
          claim: action.task,
          status: "pending",
          scope: "run",
        },
      ],
    };
    setFrozenGoalBySession((prev) => {
      const next = new Map(prev);
      next.set(sid, { runId, goal });
      return next;
    });

    const card: DecisionCardBlock = {
      type: "decision_card",
      decision_id: `dc-dispatch-${runId}`,
      kind: "dispatch_confirm",
      question: t("app.dispatch.question", { name: targetName, task: subtask }),
      options: [dispatchConfirmOk, dispatchConfirmCancel],
      recommended: dispatchConfirmOk,
      rationale: action.rationale,
      payload: {
        task: action.task,
        scope_files: action.scope_files,
        agent_hint: action.agent_hint,
        rationale: action.rationale,
        goal_title: action.goal_title ?? null,
      },
      // per-card 唯一 id（= 同会话冻结目标 entry 的 runId）·防 turn key 撞 + 让取消精确解冻本卡。
      source_run_id: runId,
      status: "pending",
      chosen_option: null,
      created_at: Date.now(),
    };
    const arr = messagesRef.current.get(sid) ?? [];
    setSessionMessages(sid, [
      ...arr,
      {
        id: crypto.randomUUID(),
        role: "assistant",
        content: [card],
        engine: "agent-team",
        agent_id: null,
        agent_name_snapshot: null,
      },
    ]);
  }

  // T5：本地 dispatch_confirm 卡的确认/取消处理（不走后端·与 onDecisionChoose 的后端路分流）。
  function onLocalDispatchConfirm(
    sid: string,
    card: DecisionCardBlock,
    option: string,
  ) {
    if (guardReadonlySession(sid)) return;
    const payload = (card.payload ?? {}) as {
      task?: string;
      scope_files?: string[];
      agent_hint?: string | null;
      rationale?: string;
      goal_title?: string | null;
    };
    setDecisionStatusInMemory(sid, card.decision_id, "chosen", option);
    if (option !== card.recommended) {
      // 取消 / 其他 → 不派单。清掉本卡合成的冻结目标（否则 topbar 留个没 run 撑着的幽灵目标条）。
      // 仅当冻结 entry 的 runId == 本卡 source_run_id 才清·绝不误伤 gate-freeze 路（其 runId 是真 run id·非 local-dispatch-*）。
      setFrozenGoalBySession((prev) => {
        if (prev.get(sid)?.runId !== card.source_run_id) return prev;
        const next = new Map(prev);
        next.delete(sid);
        return next;
      });
      return;
    }
    dispatchWorkerFromLead(
      sid,
      payload.task ?? "",
      payload.scope_files ?? [],
      payload.agent_hint ?? null,
      payload.rationale ?? "",
      payload.goal_title ?? null,
    ).catch(showStartTeamRunError);
  }

  function handleLeadOutcome(
    sid: string,
    leadId: string,
    userText: string,
    action: LeadAction,
    decisionCard: DecisionCardBlock | null,
    config?: ComposerRuntimeConfig,
  ) {
    // 新一轮决策先清上一轮残留的待确认/收工卡（防陈旧卡残留·opus T8 审 NIT）·各分支按需重设
    setLeadView(sid, null);
    if (action.action === "reply") {
      // 决策5：reply = 转一次真 Normal 流式 send·用 team leadId（lead 既决策也答话）。
      // 用户消息由 send_message 落库（runLeadStepForSession 未预落·避免双写）。
      // team 模式 send_message 终态无「→叫 lead」回路（completed 事件只落消息+清 run）·故 reply 是终结动作·等用户下一句。
      const nameSnap = agentNameSnapshotFor(leadId);
      const arr = messagesRef.current.get(sid) ?? [];
      setSessionMessages(sid, [
        ...arr,
        {
          id: crypto.randomUUID(),
          role: "assistant",
          content: [],
          engine: leadId,
          agent_id: leadId,
          agent_name_snapshot: nameSnap,
        },
      ]);
      setRun(sid, {
        startedAt: Date.now(),
        workingTokens: null,
        engine: leadId,
        agent_id: leadId,
        agent_name_snapshot: nameSnap,
      });
      invoke(
        "send_message",
        sendMessagePayload(sid, leadId, userText, config),
      ).catch((err) => {
        if (String(err).startsWith("SESSION_ALREADY_RUNNING:")) return;
        setRun(sid, null);
        setToast(renderBackendError(String(err), t));
      });
      return;
    }
    persistUserMsg(sid, userText); // 非 reply：补落用户消息
    if (action.action === "dispatch_worker") {
      // 看一眼再派（T5）：lead 选 dispatch_worker 后不直接 start_team_run，
      // 先出一张 dispatch_confirm 确认卡（澄清目标 + 子任务 + 派给谁）+ 把澄清目标 freeze 进 topbar GoalBar；
      // 用户确认才真正派单（confirm 逻辑在 onDecisionChoose 里按 local 标记分流）。
      showDispatchConfirm(sid, action);
      return;
    }
    if (action.action === "ask_user") {
      // T-C3b b1：ask_user → 持久流内决策卡（后端已 append·此处镜像进内存·单一真相=后端）。
      if (decisionCard) {
        const arr = messagesRef.current.get(sid) ?? [];
        setSessionMessages(sid, [
          ...arr,
          {
            id: crypto.randomUUID(),
            role: "assistant",
            content: [decisionCard],
            engine: "agent-team",
            agent_id: null,
            agent_name_snapshot: null,
          },
        ]);
      } else {
        // 兜底：后端未回卡（理论不该发生）→ 退回 ephemeral leadView 保不吞决策
        setLeadView(sid, {
          kind: "ask",
          question: action.question,
          options: action.options,
          recommended: action.recommended,
          rationale: action.rationale,
        } as LeadView);
      }
      return;
    }
    if (action.action === "propose_verifier") {
      // 决策6：有 active coding run → 复用 verify 卡走；无 run → 退化 ask_user（保住验证意图·不吞）
      const activeRun = [...codingLoopsRef.current.values()].find(
        (s) => s.sessionId === sid,
      );
      if (activeRun) {
        const next = {
          ...activeRun,
          verifyCmd: action.cmd,
          phase: "ask_verify" as const,
        };
        codingLoopsRef.current.set(activeRun.runId, next);
        upsertCodingTaskBlock(
          activeRun.runId,
          blockFromCodingState(activeRun.runId, next),
        );
      } else {
        setLeadView(sid, {
          kind: "ask",
          question: t("app.verify.noActiveChanges", { command: action.cmd }),
          options: [t("app.verify.dispatchFirst"), t("app.verify.later")],
          recommended: t("app.verify.dispatchFirst"),
          rationale: action.rationale,
        });
      }
      return;
    }
    if (action.action === "finish") {
      setLeadView(sid, {
        kind: "finish",
        rationale: action.rationale,
        evidenceRefs: action.evidence_refs,
      });
      return;
    }
    // T-C3 b2b 改动条交付动作：commit / push / create_pr / publish。
    // 取当前会话活跃 coding run → invoke 后端 → append assistant 结果 + 持久化；失败 showLeadError。
    if (
      action.action === "commit" ||
      action.action === "push" ||
      action.action === "create_pr" ||
      action.action === "publish"
    ) {
      const runId = runIdForActiveCodingSession(codingLoopsRef.current, sid);
      // append assistant 结果消息 + append_message 持久化
      const appendLeadResult = (text: string) => {
        const leadName = agentNameSnapshotFor(leadId) ?? null;
        const arr = messagesRef.current.get(sid) ?? [];
        setSessionMessages(sid, [
          ...arr,
          {
            id: crypto.randomUUID(),
            role: "assistant" as const,
            content: [{ type: "text" as const, text }],
            engine: leadId,
            agent_id: leadId,
            agent_name_snapshot: leadName,
          },
        ]);
        invoke("append_message", {
          sessionId: sid,
          role: "assistant",
          content: [{ type: "text", text }],
          engine: leadId,
          agentId: leadId,
          agentNameSnapshot: leadName,
        }).catch(() => {});
      };
      if (!runId) {
        appendLeadResult(
          t("app.delivery.noChanges", { rationale: action.rationale }),
        );
        return;
      }
      if (action.action === "commit") {
        invoke<string>("apply_run_to_current_branch", {
          sessionId: sid,
          runId,
        })
          .then((landedHead) => {
            // 更新 codingLoops 里该 run 的 CodingState → phase applied + landedHead（落地完成态）
            const prev = codingLoopsRef.current.get(runId);
            if (prev) {
              codingLoopsRef.current.set(runId, {
                ...prev,
                phase: "applied",
                landedHead,
              });
            }
            appendLeadResult(
              t("app.delivery.applied", {
                rationale: action.rationale,
                head: landedHead,
              }),
            );
            refreshReview(sid);
          })
          .catch((e) => showLeadError(sid, String(e)));
        return;
      }
      if (action.action === "push") {
        if (!window.confirm(t("app.delivery.confirmPush"))) return;
        invoke<string>("push_run", { sessionId: sid, runId, confirmed: true })
          .then((brief) =>
            appendLeadResult(
              `${action.rationale} · ${renderBackendError(brief, t)}`,
            ),
          )
          .catch((e) => showLeadError(sid, String(e)));
        return;
      }
      if (action.action === "create_pr") {
        if (!window.confirm(t("app.delivery.confirmCreatePr"))) return;
        invoke<string>("create_pr_run", {
          sessionId: sid,
          runId,
          title: action.title,
          body: action.body,
          confirmed: true,
        })
          .then((url) =>
            appendLeadResult(
              t("app.delivery.prCreated", {
                rationale: action.rationale,
                url,
              }),
            ),
          )
          .catch((e) => showLeadError(sid, String(e)));
        return;
      }
      // action.action === "publish"
      if (!window.confirm(t("app.delivery.confirmPublish"))) return;
      invoke<string>("publish_local_run", {
        sessionId: sid,
        runId,
        repoName: action.repo_name,
        private: action.private,
        confirmed: true,
      })
        .then((url) =>
          appendLeadResult(
            t("app.delivery.published", {
              rationale: action.rationale,
              url,
            }),
          ),
        )
        .catch((e) => showLeadError(sid, String(e)));
      return;
    }
  }

  function showLeadError(sid: string, msg: string) {
    if (
      msg.startsWith("SESSION_BUSY") ||
      msg.startsWith("SESSION_ALREADY_RUNNING")
    )
      return;
    // 失败态对齐原型：保留临时失败的柔性文案，但不要盖住队长硬闸的真实原因。
    const classification = classifyLeadError(msg);
    const claudeOnly = classification === "claudeOnly";
    const transient = classification === "transient";
    const renderedBackendDetail = renderBackendError(msg, t);
    setLeadView(sid, {
      kind: "ask",
      question: claudeOnly
        ? t("lead.error.claudeOnly")
        : transient
          ? t("app.lead.transientQuestion")
          : t("app.lead.genericQuestion"),
      options: [t("app.lead.retry")],
      recommended: t("app.lead.retry"),
      rationale: claudeOnly
        ? t("lead.error.claudeOnly")
        : transient
          ? t("app.lead.transientRationale")
          : renderedBackendDetail === msg
            ? t("app.lead.genericRationale")
            : renderedBackendDetail,
    });
  }

  function membersForRun(runId: string): MemberUnit[] | null {
    const live = teamRunsBySession.get(currentId ?? "")?.get(runId);
    if (live) return live.members;
    const msgs = messagesRef.current.get(currentId ?? "") ?? [];
    for (const message of msgs) {
      for (const block of message.content) {
        if (block.type === "team_run" && block.run_id === runId) {
          return block.members;
        }
      }
    }
    return null;
  }

  const handleOpenMember = (runId: string, assignmentId: string) => {
    setInspectorTarget(null);
    setUndoTarget(null);
    tabBeforeDrillRef.current = rightPanelTab;
    setDrillRun({ runId, assignmentId });
    setRightPanelOpen(true);
  };

  const handleStopMember = (runId: string, assignmentId: string) => {
    const sid = currentIdRef.current;
    if (!sid) return;
    invoke("stop_team_member", {
      sessionId: sid,
      runId,
      assignmentId,
    }).catch((e) => setToast(String(e)));
  };

  const handleBackFromDrill = () => {
    setDrillRun(null);
    if (tabBeforeDrillRef.current !== null) {
      setRightPanelTab(tabBeforeDrillRef.current);
    }
  };

  // 点右面板 tab 时先退出 drill（否则 drill 压过 tab·点了菜单没反应·GUI 验收 #3）。
  const handleSelectTab = (t: RightPanelTab | null) => {
    if (t === "preview" && rightPanelTab !== "preview") {
      tabBeforePreviewRef.current = rightPanelTab;
    }
    setInspectorTarget(null);
    setDrillRun(null);
    setShowTaskList(false);
    setUndoTarget(null);
    setRightPanelTab(t);
  };

  const drillMembers = drillRun ? membersForRun(drillRun.runId) : null;
  const drill =
    drillRun && drillMembers
      ? {
          members: drillMembers,
          selectedId: drillRun.assignmentId,
          onSelect: (assignmentId: string) =>
            setDrillRun({ runId: drillRun.runId, assignmentId }),
          onBack: handleBackFromDrill,
          onStop: (assignmentId: string) =>
            handleStopMember(drillRun.runId, assignmentId),
          goal:
            teamRunsBySession.get(currentId ?? "")?.get(drillRun.runId)?.goal ??
            null,
          criteria: acceptanceByRun.get(drillRun.runId) ?? [],
        }
      : null;

  const openInspector = (aid: string) => {
    setInspectorTarget(aid);
    setDrillRun(null);
    setUndoTarget(null);
    setRightPanelOpen(true);
  };

  const openPreview = useCallback(
    (path: string) => {
      if (rightPanelTabRef.current !== "preview") {
        tabBeforePreviewRef.current = rightPanelTabRef.current;
      }
      setPreviewPath(path);
      setPreviewSessionId(currentId ?? null);
      setInspectorTarget(null);
      setDrillRun(null);
      setShowTaskList(false);
      setUndoTarget(null);
      setRightPanelTab("preview");
      setRightPanelOpen(true);
    },
    [currentId],
  );

  const closePreview = () => {
    setPreviewPath(null);
    setPreviewSessionId(null);
    if (rightPanelTab === "preview") {
      setRightPanelTab(tabBeforePreviewRef.current ?? "files");
    }
    tabBeforePreviewRef.current = null;
  };

  const openLightbox = useCallback(
    (path: string) => {
      setLightbox({ path, sessionId: currentId });
    },
    [currentId],
  );
  const closeLightbox = useCallback(() => setLightbox(null), []);

  const openTaskList = () => {
    setInspectorTarget(null);
    setDrillRun(null);
    setUndoTarget(null);
    setRightPanelTab(null);
    setShowTaskList(true);
    setRightPanelOpen(true);
  };

  const toggleTaskList = () => {
    if (rightPanelOpen && showTaskList) {
      setInspectorTarget(null);
      setShowTaskList(false);
      setDrillRun(null);
      setRightPanelOpen(false);
      setRightPanelExpanded(false);
    } else {
      openTaskList();
    }
  };

  const openRightPanelHome = () => {
    setInspectorTarget(null);
    setDrillRun(null);
    setShowTaskList(false);
    setUndoTarget(null);
    setRightPanelTab(null);
    setRightPanelOpen(true);
  };

  const inspectorMember = inspectorTarget
    ? memberByAssignment(
        messagesRef.current.get(currentId ?? "") ?? [],
        inspectorTarget,
      )
    : null;

  function scheduleRender() {
    if (flushRafRef.current !== null) return;
    flushRafRef.current = requestAnimationFrame(() => {
      flushRafRef.current = null;
      setMessagesBySession(messagesRef.current);
    });
  }

  function mutateSession(
    sid: string,
    fn: (msgs: ChatMessage[]) => ChatMessage[],
  ) {
    const cur = messagesRef.current.get(sid) ?? [];
    const next = new Map(messagesRef.current);
    next.set(sid, fn(cur));
    messagesRef.current = next;
    scheduleRender();
  }

  // 块②a-1：决策卡后另起续写消息时·新消息带队长身份（与 lead 流式消息一致·渲成「Claude 队长」而非空/Lead）。
  function leadStreamIdentity(sid: string) {
    const run = runningSessionsRef.current.get(sid);
    return {
      engine: run?.engine,
      agent_id: run?.agent_id,
      agent_name_snapshot: run?.agent_name_snapshot,
    };
  }

  function sweepSession(sid: string) {
    return sweepRunning(messagesRef.current.get(sid) ?? []);
  }

  function setSessionLoading(sid: string, loading: boolean) {
    const next = new Set(loadingSessionsRef.current);
    if (loading) next.add(sid);
    else next.delete(sid);
    loadingSessionsRef.current = next;
    setLoadingSessionIds(next);
  }

  // 刀 R R3：lastAssistantIndex/Engine/AgentId/AgentNameSnapshot 四个 helper 已随
  // 终态分支的前端 append_message 补写一并删除（过程持久化已后端化·display_reduce 归约器）。

  function agentNameSnapshotFor(id: string): string | null {
    return agents.find((agent) => agent.id === id)?.name ?? null;
  }

  const refreshRuntimeDetect = useCallback(() => {
    invoke<{
      claude?: { available?: boolean };
      codex?: { available?: boolean };
    }>("detect_runtime")
      .then((r) =>
        setRuntimeDetect({
          claude: Boolean(r?.claude?.available),
          codex: Boolean(r?.codex?.available),
        }),
      )
      .catch(() => setRuntimeDetect(undefined)); // 失败保持 undefined = 乐观（不误隐原生）
  }, []);

  const refetchAgents = useCallback(async () => {
    // 表单内「重新检测」可能已发现新装的 CLI——agents 变化时同步刷新可用性，防 composer 用旧检测过滤（T6 F5）
    refreshRuntimeDetect();
    await invoke<AgentProfile[]>("list_agents")
      .then((xs) => {
        const nextAgents = xs ?? [];
        const enabledAgents = nextAgents
          .filter((agent) => agent.enabled)
          .sort(
            (a, b) => a.sort_order - b.sort_order || a.id.localeCompare(b.id),
          );
        setAgents(nextAgents);
        setAgentId((current) => {
          if (current && enabledAgents.some((agent) => agent.id === current)) {
            return current;
          }
          return (
            resolveFallbackAgentId(loadLastAgentId(), enabledAgents) ??
            current ??
            "claude"
          );
        });
        setAgentsReady(true);
      })
      .catch(() => {
        setAgents([]);
        setAgentsReady(true);
      });
  }, [refreshRuntimeDetect]);

  const availableAgents = useMemo(
    () => agents.filter((agent) => isAgentAvailable(agent, runtimeDetect)),
    [agents, runtimeDetect],
  );

  const inIntroComposer = view === "intro";
  const composerTeamCfg = useMemo<TeamConfig>(
    () =>
      !inIntroComposer && currentId
        ? { leadId: teamCfg.leadId, rosterIds: teamCfg.rosterIds }
        : draftTeamCfg,
    [
      currentId,
      draftTeamCfg,
      inIntroComposer,
      teamCfg.leadId,
      teamCfg.rosterIds,
    ],
  );
  const composerTeamActive = composerTeamCfg.leadId !== null;

  function setComposerLeadId(id: string | null, memberIds?: string[]) {
    if (!inIntroComposer && currentId) {
      teamCfg.setLeadId(id, memberIds);
      return;
    }

    const leadId = typeof id === "string" ? id : null;
    setDraftTeamCfg((prev) => ({
      leadId,
      rosterIds: leadId === null ? [] : (memberIds ?? prev.rosterIds),
    }));
  }

  function toggleComposerRoster(id: string, _allEnabledIds: string[]) {
    if (!inIntroComposer && currentId) {
      teamCfg.toggleRoster(id, _allEnabledIds);
      return;
    }

    setDraftTeamCfg((prev) => ({
      leadId: prev.leadId,
      rosterIds: prev.rosterIds.includes(id)
        ? prev.rosterIds.filter((existingId) => existingId !== id)
        : [...prev.rosterIds, id],
    }));
  }

  function resolveRuntimeTeamConfig(sid: string): RuntimeTeamConfig {
    const cfg = loadTeamConfig(sid);
    const hasSavedLead = cfg.leadId !== null;
    const effectiveLeadId = cfg.leadId ?? agentId;
    const visibleIds = availableAgents.map((agent) => agent.id);
    const visible = new Set(visibleIds);
    const configuredPool = hasSavedLead ? cfg.rosterIds : visibleIds;
    return {
      hasSavedLead,
      effectiveLeadId,
      memberPoolIds: configuredPool.filter((id) => visible.has(id)),
    };
  }
  // 常青 ref：resolveRuntimeTeamConfig 闭包读 agentId/availableAgents（组件 state），若被
  // deps=[] 的 agent-event(-batch) 监听 effect 直接捕获会拿到挂载时的陈旧值（同 tRef 手法，
  // 每次渲染同步刷新，effect 里只经 ref 调用取当前值）。
  const resolveRuntimeTeamConfigRef = useRef(resolveRuntimeTeamConfig);
  resolveRuntimeTeamConfigRef.current = resolveRuntimeTeamConfig;

  function defaultWorkerAgentId(runtime: RuntimeTeamConfig): string | null {
    if (runtime.hasSavedLead) return runtime.memberPoolIds[0] ?? null;
    return runtime.memberPoolIds.includes(agentId)
      ? agentId
      : (runtime.memberPoolIds[0] ?? null);
  }

  const sendGate = useMemo(
    () =>
      deriveSendGate({
        messagesLoaded: currentId !== null && messagesBySession.has(currentId),
        loading,
        agentsReady,
        availableAgents,
        selectedAgentId: agentId,
        memberRunning,
      }),
    [
      currentId,
      messagesBySession,
      loading,
      agentsReady,
      availableAgents,
      agentId,
      memberRunning,
    ],
  );
  const teamConfigPending =
    composerTeamActive &&
    !inIntroComposer &&
    currentId !== null &&
    teamCfg.loading;
  const teamConfigBlocked =
    teamConfigPending ||
    (composerTeamActive &&
      !inIntroComposer &&
      currentId !== null &&
      teamCfg.error !== null);

  useLayoutEffect(() => {
    const id = currentId;
    if (!id) return;
    if (stickyDoneRef.current === id) return;
    if (!agentsReady) return;
    if (!messagesBySession.has(id)) return;
    const msgs = messagesRef.current.get(id) ?? [];
    const sticky = deriveStickyAgentId(msgs, availableAgents);
    if (sticky) {
      setAgentId(sticky);
      stickyDoneRef.current = id;
    }
  }, [currentId, agentsReady, messagesBySession, availableAgents]);

  useEffect(() => {
    setAgentId((current) => {
      if (availableAgents.some((agent) => agent.id === current)) {
        return current;
      }
      return (
        resolveFallbackAgentId(loadLastAgentId(), availableAgents) ?? current
      );
    });
  }, [availableAgents]);

  // 启动：取/建会话，从 sqlite 载入历史
  useEffect(() => {
    if (initedRef.current) return; // StrictMode 下 effect 双调，防重复初始化
    initedRef.current = true;
    (async () => {
      let list = await refreshSessions();
      if (list.length === 0) {
        const sid = crypto.randomUUID();
        if (activeRepoId !== null) {
          await invoke("create_session", {
            id: sid,
            title: t("app.session.new"),
            repoId: activeRepoId,
            namespaceId: activeNamespaceId,
          });
          list = await refreshSessions();
        }
      }
      const active = list.filter((s) => !s.archived);
      if (active.length > 0) {
        await openSession(active[0].id, list);
      }
    })().catch((e) => setToast(renderBackendError(String(e), t)));
    invoke<AppContext>("app_context")
      .then((ctx) => {
        if (!ctx) return;
        setNamespaces(ctx.namespaces ?? []);
        setActiveNamespaceId(ctx.active_namespace_id ?? "local");
        setActiveRepoId(ctx.active_repo_id ?? null);
        setReposInActiveNs(ctx.repos ?? []);
      })
      .catch((e) => {
        setNamespaces([]);
        setReposInActiveNs([]);
        setToast(renderBackendError(String(e), t));
      });
    void refetchAgents();
    // B1 NamespaceDropdown 需 allRepos 算 count · 启动同步拉一次
    invoke<RepoMeta[]>("list_repos")
      .then((rs) => setAllRepos(rs ?? []))
      .catch(() => setAllRepos([]));
    // detect_runtime 已随上方 refetchAgents() 刷新（F5），此处不再重复调。
    // Git/gh/账户只在打开「设置 > 仓库」时检测，避免拖慢每次启动。
  }, [refetchAgents, refreshRuntimeDetect]);

  useEffect(() => {
    if (!(settingsOpen && settingsPage === "repos")) return;
    void loadRepoTools();
  }, [loadRepoTools, settingsOpen, settingsPage]);

  useEffect(() => {
    if (ghAccounts.length === 0) {
      setSelectedLogin("");
      return;
    }
    setSelectedLogin((current) => {
      if (current && ghAccounts.some((account) => account.login === current)) {
        return current;
      }
      return (
        ghAccounts.find((account) => account.active)?.login ??
        ghAccounts[0].login
      );
    });
  }, [ghAccounts]);

  useEffect(() => {
    try {
      localStorage.setItem(
        REPO_CACHE_STORAGE_KEY,
        serializeRepoCache(repoCacheByLogin),
      );
    } catch {
      // 隐私模式 / quota 超限等 — 不影响主流程，缓存只是退化回冷加载。
    }
  }, [repoCacheByLogin]);

  useEffect(() => {
    if (
      !(settingsOpen && settingsPage === "repos") ||
      !selectedLogin ||
      gitInstalled !== true ||
      ghInstalled !== true ||
      ghAccountError
    )
      return;
    const entry = repoCacheByLoginRef.current[selectedLogin];
    const nowMs = Date.now();
    const lastAutoRefresh = autoRepoRefreshAtRef.current[selectedLogin] ?? 0;
    const stale =
      !entry?.updatedAt || nowMs - entry.updatedAt > REPO_CACHE_STALE_MS;
    const alreadyLoading =
      entry?.status === "loading" || entry?.status === "refreshing";
    if (!stale || alreadyLoading) return;
    if (nowMs - lastAutoRefresh < REPO_CACHE_AUTO_DEBOUNCE_MS) return;
    autoRepoRefreshAtRef.current[selectedLogin] = nowMs;
    void loadRepoList(selectedLogin);
  }, [
    ghAccountError,
    ghInstalled,
    gitInstalled,
    loadRepoList,
    selectedLogin,
    settingsOpen,
    settingsPage,
  ]);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if ((e.metaKey || e.ctrlKey) && e.key === ",") {
        e.preventDefault();
        setSettingsOpen(true);
      } else if (
        e.key === "Escape" &&
        settingsOpen &&
        !deleteTarget &&
        !groupDeleteTarget &&
        removeProjectTarget === null &&
        !invalidDialog
      ) {
        e.stopPropagation();
        setSettingsOpen(false);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [
    settingsOpen,
    deleteTarget,
    groupDeleteTarget,
    removeProjectTarget,
    invalidDialog,
  ]);

  useEffect(() => {
    const timers = cloneSettleTimersRef.current;
    const stateByLogin = new Map<
      string,
      { hasCloning: boolean; hasDone: boolean }
    >();

    for (const entry of Object.values(cloneProgress)) {
      const current = stateByLogin.get(entry.login) ?? {
        hasCloning: false,
        hasDone: false,
      };
      if (entry.phase === "cloning") current.hasCloning = true;
      if (entry.phase === "done") current.hasDone = true;
      stateByLogin.set(entry.login, current);
    }

    for (const login of Object.keys(timers)) {
      const state = stateByLogin.get(login);
      if (!state || state.hasCloning || !state.hasDone) {
        clearTimeout(timers[login]);
        delete timers[login];
      }
    }

    for (const [login, state] of stateByLogin) {
      if (state.hasCloning || !state.hasDone || timers[login]) continue;

      timers[login] = setTimeout(() => {
        delete cloneSettleTimersRef.current[login];
        setCloneProgressEntries((prev) => {
          let changed = false;
          const next: Record<RepoKey, CloneProgressEntry> = {};

          for (const [key, entry] of Object.entries(prev) as Array<
            [RepoKey, CloneProgressEntry]
          >) {
            if (entry.login === login && entry.phase === "done") {
              changed = true;
              continue;
            }
            next[key] = entry;
          }

          return changed ? next : prev;
        });
      }, CLONE_SETTLE_LINGER_MS);
    }
  }, [cloneProgress, setCloneProgressEntries]);

  useEffect(
    () => () => {
      for (const timer of Object.values(cloneSettleTimersRef.current)) {
        clearTimeout(timer);
      }
      cloneSettleTimersRef.current = {};
    },
    [],
  );

  useEffect(() => {
    const applyAgentEvent = (
      ev: AppAgentEventEnvelope,
      mutate: SessionMutator,
    ) => {
      const sid = ev.session_id;
      if (!sid) return;

      if (ev.kind === "session_started") {
        return;
      }

      if (ev.kind === "usage_delta") {
        const run = runningSessionsRef.current.get(sid);
        if (!run) return;
        setRun(sid, {
          ...run,
          workingTokens: accumulateWorkingTokens(
            run.workingTokens ?? 0,
            ev.input_tokens,
            ev.output_tokens,
          ),
        });
        return;
      }

      if (isDispatchEnvelope(ev)) {
        // orchestrated（队长 lead-session 派的 worker）= lead 的后台执行资源·默认不外露
        // （原型真相源 agent-team-runtime-lead-centric：主区只留 lead 一个声音·worker 默认不外露）。
        // 整个不进主区 team-run 渲染/持久化——队长在道 A 自然叙述 + dispatch_worker 工具卡已表征派单。
        if (isOrchestratedDispatch(ev)) {
          // 块①.5：worker 事件折进当前队长消息的 dispatch_card·短路·绝不落普通 completed/error
          mutate(sid, (msgs) =>
            upsertDispatchCard(msgs, ev, tRef.current("teamRun.errorPrefix")),
          );
          const rid = ev.dispatch?.run_id;
          if (rid && !goalTitleFetchedRef.current.has(rid)) {
            goalTitleFetchedRef.current.add(rid);
            invoke<string | null>("get_run_goal_title", {
              sessionId: sid,
              runId: rid,
            })
              .then((title) => {
                if (title)
                  setGoalTitleByRun((prev) => new Map(prev).set(rid, title));
              })
              .catch(() => {});
          }
          // 自动唤醒刀：worker 终态到达且 lead 已空闲 → 替用户按「继续」（同形先例
          // App.tsx:5261-5288·G3 停摆修复 T3）。终态判定复用本 effect 下方既有 isTerminalEvent
          // （3507 行左右）——worker 自己的 run 无论 done/failed/stopped 都统一走
          // kind==="completed"（member_terminal_event 恒发 AgentEvent::Completed，
          // status_transition 才区分成败，见 src-tauri/src/member_runner.rs:1514-1557），
          // isTerminalEvent 的 kind 判据天然适用、不需要另判 status_transition。
          {
            const aid = ev.dispatch?.assignment_id;
            if (aid && isTerminalEvent(ev)) {
              const dedupKey = `${sid}:${aid}`;
              // 按 assignment 去重：防事件重放/重复批次导致多次 invoke。
              if (
                !autoResumeTriggeredRef.current.has(dedupKey) &&
                !autoResumeInFlightRef.current.has(dedupKey)
              ) {
                // lead 空闲才触发（与 App.tsx:5257 先例同判据）；lead 在跑就什么都不做——
                // 跟 streak 达上限一样，是「跳过」而非「失败」：立即永久记入、不补触发
                // （不排队，lead 自己下轮能看到 report）。
                if (!runningSessionsRef.current.has(sid)) {
                  const streak = autoResumeStreakRef.current.get(sid) ?? 0;
                  if (streak < AUTO_RESUME_MAX_STREAK) {
                    autoResumeStreakRef.current.set(sid, streak + 1);
                    // 同步占位：只挡 invoke 结果落定前的同 tick 重放（下面成功/终态失败都会摘掉）——
                    // 不是永久记入，失败（含竞速重试后仍失败）时特意不转成 autoResumeTriggeredRef，
                    // 好让这单自动续喂不被永久放弃（真实事件重放/重触发条件到来还有机会）。
                    autoResumeInFlightRef.current.add(dedupKey);
                    const runtime = resolveRuntimeTeamConfigRef.current(sid);
                    // 中性原则：只是替用户按「继续」，不往对话注入任何指令文本——worker
                    // report 已在会话史里，lead 自己会看到。
                    const attemptResume = (isRetry: boolean) => {
                      invoke("resume_lead_session", {
                        sessionId: sid,
                        leadAgentId: runtime.effectiveLeadId,
                        memberIds: runtime.memberPoolIds,
                      })
                        .then(() => {
                          autoResumeInFlightRef.current.delete(dedupKey);
                          autoResumeTriggeredRef.current.add(dedupKey);
                        })
                        .catch((e) => {
                          const envelope = parseBackendError(String(e));
                          const isIntentRaceRejection =
                            envelope?.code === "run.teamMembersActive";
                          if (!isRetry && isIntentRaceRejection) {
                            // 竞速：reader 摘 member（member_runner.rs:2272）早于 dispatch
                            // intent guard 释放（要等 run_single_worker 整体返回，含
                            // persist/finalize/Stage① 收尾）——前端可能抢在 guard drop 前
                            // invoke、撞 AL_ERR:run.teamMembersActive 被拒。暂时性竞态，
                            // 不是真失败：短延迟后重试一次，仍失败才罢休。
                            window.setTimeout(
                              () => attemptResume(true),
                              AUTO_RESUME_RACE_RETRY_DELAY_MS,
                            );
                            return;
                          }
                          // 占槽被抢（如另一路径同时唤醒）或竞速重试仍失败 = 正常收敛，
                          // 不扰民；不烧 triggered 键，只摘掉 inFlight——留给后续真实事件
                          // 重放/重触发条件自然补上，不是永久放弃。
                          autoResumeInFlightRef.current.delete(dedupKey);
                          console.debug(
                            "[auto-resume] resume_lead_session skipped",
                            e,
                          );
                        });
                    };
                    attemptResume(false);
                  } else {
                    // 达连续上限：静默停，把控制权还给用户；用户发消息（onSend）时清零。
                    autoResumeTriggeredRef.current.add(dedupKey);
                  }
                } else {
                  // lead 在跑：跳过，不补触发（同原语义永久记入）。
                  autoResumeTriggeredRef.current.add(dedupKey);
                }
              }
            }
          }
          return;
        }
        const rid = ev.dispatch!.run_id!;
        const prevRuns = teamRunsRef.current;
        const next = new Map(prevRuns);
        const runMap = new Map(next.get(sid) ?? []);
        const updated = applyTeamEvent(
          runMap.get(rid) ?? null,
          ev,
          tRef.current("teamRun.errorPrefix"),
        );
        runMap.set(rid, updated);
        next.set(sid, runMap);
        teamRunsRef.current = next;
        setTeamRunsBySession(next);

        const pkey = `${sid}:${rid}`;
        if (isTeamRunComplete(updated) && !persistedRunsRef.current.has(pkey)) {
          persistedRunsRef.current.add(pkey);
          const block = teamRunToBlock(updated);
          const arr = messagesRef.current.get(sid) ?? [];
          setSessionMessages(sid, [
            ...arr,
            {
              id: crypto.randomUUID(),
              role: "assistant",
              content: [block],
              engine: "agent-team",
              agent_id: null,
              agent_name_snapshot: null,
            },
          ]);
          invoke("append_message", {
            sessionId: sid,
            role: "assistant",
            content: [block],
            engine: "agent-team",
            agentId: null,
            agentNameSnapshot: null,
          }).catch(() => {});
          if (sid === currentIdRef.current) refreshReview(sid);
          void (async () => {
            try {
              const criteria = await invoke<AcceptanceCriterion[]>(
                "list_acceptance",
                {
                  sessionId: sid,
                  runId: rid,
                },
              ).catch(() => []);
              setAcceptanceByRun((prev) => {
                const next = new Map(prev);
                next.set(rid, criteria);
                return next;
              });
              invoke<string | null>("get_run_goal_title", {
                sessionId: sid,
                runId: rid,
              })
                .then((title) => {
                  if (title) {
                    setGoalTitleByRun((prev) => {
                      const next = new Map(prev);
                      next.set(rid, title);
                      return next;
                    });
                  }
                })
                .catch(() => {});
              if (updated.members.length === 1) {
                if (shouldEnterCodingLoop(updated)) {
                  const member = updated.members[0];
                  const result = member.result as any;
                  const baseSha = result.anchor.base_sha as string;
                  const verifyCmd = selectCodingVerifier(
                    criteria,
                    member.task_id,
                    rid,
                  );
                  const state: CodingState = {
                    runId: rid,
                    sessionId: sid,
                    assignmentId: member.assignment_id,
                    taskId: member.task_id,
                    baseSha,
                    phase: "finalizing",
                    artifactId: null,
                    verifyCmd,
                    landedHead: null,
                    isInPlace:
                      sessionsRef.current.find((s) => s.id === sid)
                        ?.in_place === true,
                  };
                  codingLoopsRef.current.set(rid, state);
                  codingLoopDisplayRef.current.set(rid, {
                    worker_name: member.name,
                    step_done: member.steps_done,
                    step_total: member.steps_total,
                  });
                  upsertCodingTaskBlock(rid, blockFromCodingState(rid, state));
                  await driveCodingLoop(rid);
                } else {
                  const summary = buildSinglePassthroughSummary(updated);
                  if (summaryStatusOf(updated).kind !== "all_succeeded")
                    summary.findings = buildFailureFindings(updated);
                  const sblock: Block = summary;
                  const leadName = updated.lead ?? null;
                  const arr2 = messagesRef.current.get(sid) ?? [];
                  setSessionMessages(sid, [
                    ...arr2,
                    {
                      id: crypto.randomUUID(),
                      role: "assistant",
                      content: [sblock],
                      engine: "agent-team",
                      agent_id: null,
                      agent_name_snapshot: leadName,
                    },
                  ]);
                  await invoke("append_message", {
                    sessionId: sid,
                    role: "assistant",
                    content: [sblock],
                    engine: "agent-team",
                    agentId: null,
                    agentNameSnapshot: leadName,
                  }).catch(() => {});
                }
              } else {
                const workers: [string, string][] = updated.members.map((m) => [
                  m.name,
                  memberFinalText(m),
                ]);
                const leadAgentId = leadAgentIdByRunRef.current.get(rid);
                const goal = updated.goal?.goal;
                const leadName = updated.lead ?? null;
                const pendingBlock: Block = buildPendingSummary(updated);
                const summaryMessageId = crypto.randomUUID();
                const pendingMessage: ChatMessage & { id: string } = {
                  id: summaryMessageId,
                  role: "assistant",
                  content: [pendingBlock],
                  engine: "agent-team",
                  agent_id: null,
                  agent_name_snapshot: leadName,
                };
                const arr2 = messagesRef.current.get(sid) ?? [];
                setSessionMessages(sid, [...arr2, pendingMessage]);
                let summary: Block;
                try {
                  const markdown = await invoke<string>("lead_summarize", {
                    sessionId: sid,
                    leadAgentId,
                    goal,
                    workers,
                  });
                  summary = buildSynthesisSummary(updated, markdown);
                } catch {
                  summary = buildFallbackRawSummary(updated);
                }
                const sblock: Block = summary;
                const finalMessage: ChatMessage & { id: string } = {
                  id: summaryMessageId,
                  role: "assistant",
                  content: [sblock],
                  engine: "agent-team",
                  agent_id: null,
                  agent_name_snapshot: leadName,
                };
                const latest = messagesRef.current.get(sid) ?? [];
                setSessionMessages(
                  sid,
                  latest.map((msg) => {
                    const first = msg.content[0];
                    if (
                      first?.type === "lead_summary" &&
                      first.summary_source === "pending" &&
                      first.run_id === rid
                    ) {
                      return finalMessage;
                    }
                    return msg;
                  }),
                );
                await invoke("append_message", {
                  sessionId: sid,
                  role: "assistant",
                  content: [sblock],
                  engine: "agent-team",
                  agentId: null,
                  agentNameSnapshot: leadName,
                }).catch(() => {});
              }
            } catch {
              /* 汇总失败不应中断事件流·吞掉 */
            }
          })();
        }
        return;
      }

      if (ev.kind === "text_delta") {
        mutate(sid, (m) =>
          appendTextDelta(
            ensureStreamTail(m, leadStreamIdentity(sid)),
            ev.text,
          ),
        );
      } else if (ev.kind === "thinking_delta") {
        mutate(sid, (m) =>
          appendThinkingDelta(
            ensureStreamTail(m, leadStreamIdentity(sid)),
            ev.text,
          ),
        );
      } else if (ev.kind === "tool_started") {
        // isHiddenTool（编排/交互工具，前缀语义）不建裸卡——决策卡 / 任务条走各自路径渲（块②a-1 bug#3）·记 id 供 completion 静默跳过。
        if (isHiddenTool(ev.tool)) {
          hiddenToolIdsRef.current.add(ev.id);
        } else {
          // ensureStreamTail：决策卡后队长的工具调用另起新消息·不灌进被 consume 的卡消息（块②a-1 narration）。
          mutate(sid, (m) =>
            appendToolStarted(ensureStreamTail(m, leadStreamIdentity(sid)), {
              id: ev.id,
              tool: ev.tool,
              summary: ev.summary,
              card: ev.card,
            }),
          );
        }
      } else if (ev.kind === "approval_requested") {
        mutate(sid, (m) =>
          appendApprovalRequested(
            ensureStreamTail(m, leadStreamIdentity(sid)),
            {
              approval_id: ev.approval_id,
              run_id: ev.run_id,
              tool: ev.tool,
              command: ev.command,
              summary: ev.summary,
              cwd: ev.cwd,
              request_kind: ev.request_kind,
            },
          ),
        );
      } else if (ev.kind === "approval_resolved") {
        mutate(sid, (m) =>
          applyApprovalResolved(m, {
            approval_id: ev.approval_id,
            decision: ev.decision,
          }),
        );
      } else if (ev.kind === "tool_completed") {
        if (hiddenToolIdsRef.current.has(ev.id)) {
          // 隐藏工具无裸卡可完成·静默跳过（不 warn / 不 applyToolCompleted）。
          hiddenToolIdsRef.current.delete(ev.id);
        } else {
          if (!hasRunningTool(messagesRef.current.get(sid) ?? [], ev.id)) {
            console.warn(
              `[execution-state] tool_completed 无匹配 running 卡 sid=${sid} id=${ev.id}`,
            );
          }
          mutate(sid, (m) =>
            applyToolCompleted(m, {
              id: ev.id,
              status: ev.status,
              exit_code: ev.exit_code,
              output: ev.output,
            }),
          );
        }
      } else if (ev.kind === "completed") {
        const swept = sweepSession(sid);
        const streamed = assistantText(swept);
        let finalMsgs = swept;
        if (streamed === "" && (ev.final_text ?? "") !== "") {
          // 决策卡结尾时 streamed==="" 必命中·末条是被 consume 的卡 → ensureStreamTail 另起新消息·final_text 不被吞（块②a-1）。
          finalMsgs = appendTextDelta(
            ensureStreamTail(swept, leadStreamIdentity(sid)),
            ev.final_text ?? "",
          );
        }
        // plan B3：非空轮（后端带 commit 字段）→ 末尾 append 持久 run_card block
        if (ev.files_changed != null && ev.run_id) {
          finalMsgs = appendRunCard(
            ensureStreamTail(finalMsgs, leadStreamIdentity(sid)),
            {
              type: "run_card",
              run_id: ev.run_id,
              commit_sha: ev.commit_sha ?? null,
              files_changed: ev.files_changed,
              insertions: ev.insertions ?? 0,
              deletions: ev.deletions ?? 0,
              interrupted: ev.interrupted ?? false,
            },
          );
        }
        mutate(sid, () => finalMsgs);

        const run = runningSessionsRef.current.get(sid);
        // 刀 R R3：过程持久化已后端化（display_reduce 归约器 flush·dedup_key 防重）——前端不再补写，消双写。
        setRun(sid, null);
        setSessionDotStatus(sid, "done");
        setSessions((prev) =>
          prev.map((s) =>
            s.id === sid
              ? {
                  ...s,
                  total_input_tokens:
                    s.total_input_tokens + (ev.input_tokens ?? 0),
                  total_output_tokens:
                    s.total_output_tokens + (ev.output_tokens ?? 0),
                }
              : s,
          ),
        );

        if (sid === currentIdRef.current) {
          setSessionUsage((prev) =>
            accumulateSessionUsage(prev, ev.input_tokens, ev.output_tokens),
          );
          const elapsedSec = run
            ? Math.max(0, Math.floor((Date.now() - run.startedAt) / 1000))
            : null;
          setDone({
            cost_usd: ev.cost_usd,
            output_tokens: ev.output_tokens,
            elapsed_sec: elapsedSec,
          });
          // 三引擎都写隔离 worktree、session_review 引擎无关 → 都刷新（Part A）
          refreshReview(sid);
          // 同 run_closeout 分支：这轮刚追加的 RunCard 的撤销按钮要看 undo_total，
          // 不刷新 ledger 的话它会一直读到 0（Solo run 完成用的正是这条 completed 分支，
          // 不是下面的 run_closeout）。
          if (ev.files_changed != null && ev.run_id) {
            void refreshRunStates(sid);
          }
        }
      } else if (ev.kind === "run_closeout") {
        const stopIssuedAt = stopIssuedAtRef.current.get(sid);
        const run = runningSessionsRef.current.get(sid);
        if (
          ev.run_id === "" &&
          stopIssuedAt !== undefined &&
          run !== undefined &&
          run.startedAt > stopIssuedAt
        ) {
          // slot 已空时补发的空标识 closeout 可能迟到；只保护停止后启动的新 run。
          // 事件到达即消费本地闸。真实 run_id 闸需契约扩展，留后续。
          stopIssuedAtRef.current.delete(sid);
          return;
        }
        // 空标识兜底已到达，或正常 closeout 已明确收尾：都不再保留停止时间闸。
        stopIssuedAtRef.current.delete(sid);
        const filesChanged = ev.files_changed;
        if (filesChanged != null) {
          mutate(sid, (m) =>
            appendRunCard(ensureStreamTail(m, leadStreamIdentity(sid)), {
              type: "run_card",
              run_id: ev.run_id,
              commit_sha: ev.commit_sha,
              files_changed: filesChanged,
              insertions: ev.insertions ?? 0,
              deletions: ev.deletions ?? 0,
              interrupted: ev.interrupted ?? false,
            }),
          );
          if (sid === currentIdRef.current) {
            refreshReview(sid);
            // RunCard 的撤销按钮现在要看 undo_total 才决定显不显示（commit 2：没有可撤销
            // 记录就不显示，不留死胡同）；这轮刚收尾，ledger 里还没有这个 run_id 的快照——
            // 不刷新的话 undo_total 会一直读到 0，把刚做完、真有得撤销的这一轮也藏起来。
            void refreshRunStates(sid);
          }
        }
        if (ev.interrupted === true) {
          setSessionDotStatus(sid, "attention");
        }
        setRun(sid, null);
      } else if (ev.kind === "error") {
        const swept = sweepSession(sid);
        const withErr = appendTextDelta(
          ensureStreamTail(swept, leadStreamIdentity(sid)),
          tRef.current("app.run.error", {
            message: renderBackendError(ev.message, tRef.current),
          }),
        );
        mutate(sid, () => withErr);

        setRun(sid, null);
        setSessionDotStatus(sid, "attention");
      } else if (ev.kind === "needs_decision") {
        const swept = sweepSession(sid);
        const withCard = appendScopeChangeCard(
          ensureStreamTail(swept, leadStreamIdentity(sid)),
          {
            type: "scope_change",
            changes: ev.changes,
          },
        );
        mutate(sid, () => withCard);

        setRun(sid, null);
        setSessionDotStatus(sid, "attention");
      } else if (ev.kind === "blocked") {
        const swept = sweepSession(sid);
        // G3 停摆点破：还有 pending 的 MCP 队长决策卡在等答案时，收工文案后追一句
        // 提示——不然用户看不出「点上方选项即可继续」是当下唯一的出路。
        const hasPendingMcpQuestion = swept.some((m) =>
          m.content.some(
            (b) =>
              b.type === "decision_card" &&
              b.status === "pending" &&
              b.source_run_id.startsWith(`${MCP_LEAD_PREFIX}-`),
          ),
        );
        const stopText =
          tRef.current("app.run.stopped", {
            message: humanizeStopReason(ev.message, tRef.current),
          }) +
          (hasPendingMcpQuestion
            ? tRef.current("app.run.stoppedPendingQuestion")
            : "");
        const withErr = appendTextDelta(
          ensureStreamTail(swept, leadStreamIdentity(sid)),
          stopText,
        );
        mutate(sid, () => withErr);

        setRun(sid, null);
        setSessionDotStatus(sid, "attention");
      }
    };

    const isTerminalEvent = (ev: AppAgentEventEnvelope) =>
      ev.kind === "completed" ||
      ev.kind === "run_closeout" ||
      ev.kind === "error" ||
      ev.kind === "needs_decision" ||
      ev.kind === "blocked";

    const flushMessagesImmediately = () => {
      if (flushRafRef.current !== null) {
        cancelAnimationFrame(flushRafRef.current);
        flushRafRef.current = null;
      }
      setMessagesBySession(messagesRef.current);
    };

    const unlisten = listen<AppAgentEventEnvelope>("agent-event", (e) => {
      applyAgentEvent(e.payload, mutateSession);
      if (isTerminalEvent(e.payload)) flushMessagesImmediately();
    });
    const unlistenBatch = listen<AppAgentEventBatchPayload>(
      "agent-event-batch",
      (e) => {
        const { messagesChanged, hasTerminal } = applyEventTransportBatch(
          e.payload,
          () => messagesRef.current,
          (event, mutate) =>
            applyAgentEvent(event as AppAgentEventEnvelope, mutate),
          (event) => isTerminalEvent(event as AppAgentEventEnvelope),
          (messages) => {
            messagesRef.current = messages;
          },
        );

        if (hasTerminal) flushMessagesImmediately();
        else if (messagesChanged) scheduleRender();
      },
    );
    return () => {
      if (flushRafRef.current !== null) {
        cancelAnimationFrame(flushRafRef.current);
        flushRafRef.current = null;
      }
      unlisten.then((f) => f());
      unlistenBatch.then((f) => f());
    };
  }, []);

  useEffect(() => {
    const unlistenGoalUpdated = listen<{
      session_id: string;
      title: string | null;
    }>("session-goal-updated", async (e) => {
      const { session_id } = e.payload;
      try {
        const sg = await invoke<SessionGoal | null>("get_session_goal", {
          sessionId: session_id,
        });
        if (sg) {
          setSessionGoalBySession((prev) => {
            const next = new Map(prev);
            next.set(session_id, sg);
            return next;
          });
        }
      } catch {
        // ignore
      }
    });
    return () => {
      unlistenGoalUpdated.then((f) => f());
    };
  }, []);

  useEffect(() => {
    const unlisten = listen<{
      session_id: string;
      block: DecisionCardBlock;
      agent_id?: string | null;
      agent_name_snapshot?: string | null;
    }>("lead-decision-card", (e) => {
      const {
        session_id: sid,
        block,
        agent_id = null,
        agent_name_snapshot = null,
      } = e.payload;
      const arr = messagesRef.current.get(sid) ?? [];
      const alreadyHas = arr.some((m) =>
        m.content.some(
          (b) =>
            b.type === "decision_card" &&
            (b as DecisionCardBlock).decision_id === block.decision_id,
        ),
      );
      if (!alreadyHas) {
        setSessionMessages(sid, [
          ...arr,
          {
            id: crypto.randomUUID(),
            role: "assistant" as const,
            content: [block],
            engine: "agent-team",
            agent_id,
            agent_name_snapshot,
          },
        ]);
      }
    });
    return () => {
      unlisten.then((f) => f());
    };
  }, []);

  // 决策打扰收敛刀 T1·症状 B 根修：后端 append_decision_echo 写库成功后 emit 这个事件——
  // 原来点击 MCP ask_user 卡后的回显消息只在下次打开会话 `get_messages` 全量拉取时才出现，
  // 当场停留在同一进程里几乎永远看不到。payload.message 与 `get_messages` 单条消息形状一致
  // （含后端 DB 自增 id）；按 id 去重防未来重拉双份（e.g. reload 后 get_messages 又把它带回来）。
  useEffect(() => {
    const unlisten = listen<{
      session_id: string;
      message: ChatMessage & { id: number };
    }>("lead-message-appended", (e) => {
      const { session_id: sid, message } = e.payload;
      // T3 顺手加固：该会话在 messagesRef 里还没有缓存条目（Map 没这个 key，不是「有 key
      // 但空数组」）时直接忽略——`?? []` 兜底会当场用「只有这一条回显」种下缓存，之后别处
      // 靠 `messagesBySession.has(sid)` 判「已加载」的地方会误判成「已加载」，挡掉后续
      // `get_messages` 全量拉取（真实历史就此丢失，只剩这一条回显）。真没缓存时让全量拉取
      // 自己把这条回显带回来即可，不必在这里抢跑。
      if (!messagesRef.current.has(sid)) return;
      const arr = messagesRef.current.get(sid) ?? [];
      const newId = String(message.id);
      const alreadyHas = arr.some(
        (m) => (m as ChatMessage & { id?: string }).id === newId,
      );
      if (!alreadyHas) {
        let insertAt = arr.length;
        while (
          insertAt > 0 &&
          (arr[insertAt - 1] as ChatMessage & { id?: string }).id == null
        ) {
          insertAt--;
        }
        const firstLargerId = arr.findIndex((m) => {
          const id = (m as ChatMessage & { id?: string }).id;
          return id != null && Number(id) > Number(newId);
        });
        if (firstLargerId >= 0) {
          insertAt = Math.min(insertAt, firstLargerId);
        }
        const newMessage = {
          ...message,
          id: newId,
        } as ChatMessage & { id: string };
        setSessionMessages(sid, [
          ...arr.slice(0, insertAt),
          newMessage,
          ...arr.slice(insertAt),
        ]);
      }
    });
    return () => {
      unlisten.then((f) => f());
    };
  }, []);

  useEffect(() => {
    const unlisten = listen("menu-open-about", () => setAboutOpen(true));
    return () => {
      unlisten.then((f) => f());
    };
  }, []);

  /**
   * cluster L plan 2a · 统一错误码分发中心
   * 按 plan 1 错误码契约前缀 split · 返 true = 已处理 / false = 未识别（上层走 fallback）
   * - PROJECT_INVALID:<id> → 弹 InvalidProjectDialog kind=invalid
   * - PROJECT_ARCHIVED:<id> → 弹 InvalidProjectDialog kind=archived
   * - ALREADY_ADDED:<id> → toast + 自动切到已有 repo
   */
  function handleProjectError(err: unknown): boolean {
    const msg = String(err);
    if (msg.startsWith("PROJECT_INVALID:")) {
      setInvalidDialog({
        repoId: msg.slice("PROJECT_INVALID:".length),
        kind: "invalid",
      });
      return true;
    }
    if (msg.startsWith("PROJECT_ARCHIVED:")) {
      setInvalidDialog({
        repoId: msg.slice("PROJECT_ARCHIVED:".length),
        kind: "archived",
      });
      return true;
    }
    if (msg.startsWith("ALREADY_ADDED:")) {
      const existingId = msg.slice("ALREADY_ADDED:".length);
      setActiveRepoId(existingId);
      setView("intro");
      setToast(t("app.repo.alreadyAdded"));
      return true;
    }
    return false;
  }

  async function refreshReview(sid: string) {
    if (currentIdRef.current !== sid) return;
    const requestGeneration = ++reviewRequestGenerationRef.current;
    const isLatestRequest = () =>
      currentIdRef.current === sid &&
      reviewRequestGenerationRef.current === requestGeneration;
    try {
      const r = await invoke<ReviewResult>("session_review", {
        sessionId: sid,
      });
      if (!isLatestRequest()) return;
      reviewCacheRef.current.set(sid, r);
      setReview(r);
      // plan B3：移除自动弹闸——右面板开合纯手动（不再 has → 自动 open/切 tab）。
      // review state 仍刷新（喂角标 + 面板内容），只是不主动 open。
    } catch (e) {
      if (!isLatestRequest()) return;
      if (!handleProjectError(e)) setReview(null);
    }
  }

  useEffect(() => {
    if (
      currentId &&
      isReviewPanelVisible(view, rightPanelOpen, rightPanelTab)
    ) {
      refreshReview(currentId);
    }
  }, [currentId, view, rightPanelOpen, rightPanelTab]);

  const refreshRunStates = useCallback(async (sid: string) => {
    try {
      const rows =
        (await invoke<RunCommitState[] | null>("list_run_commits", {
          sessionId: sid,
        })) ?? [];
      const states = new Map<string, RunCommitState>();
      for (const row of rows) {
        if (row.run_id) states.set(row.run_id, row);
      }
      setRunStatesBySession((prev) => {
        const next = new Map(prev);
        next.set(sid, states);
        return next;
      });
    } catch {
      // Keep the last backend-confirmed ledger. A failed refresh must not
      // replace known partial progress with a fabricated empty/active state.
    }
  }, []);

  async function openSession(id: string, list?: Session[]) {
    setSettingsOpen(false);
    setContinuationReadySessionIds((previous) => {
      if (!previous.has(id)) return previous;
      const next = new Set(previous);
      next.delete(id);
      return next;
    });
    /**
     * cluster L Phase 3 plan C2-B Task 1 · 打开 session
     * C2-B：refreshSessions 后立即 open 时 React state 尚未 commit，不能读闭包 sessions。
     * B4：sess.namespace_id !== activeNamespaceId 时同步 activeNamespaceId + 刷新 reposInActiveNs（保 sidebar/crumb 同步）。
     */
    const source = list ?? sessions;
    const sess = source.find((s) => s.id === id);
    if (sess?.unread) {
      invoke("set_session_unread", { id, unread: false }).catch(() => {});
      setSessions((prev) =>
        prev.map((s) => (s.id === id ? { ...s, unread: false } : s)),
      );
    }
    const sessNamespaceId = sess?.namespace_id ?? null;
    if (sessNamespaceId && sessNamespaceId !== activeNamespaceId) {
      setActiveNamespaceId(sessNamespaceId);
      try {
        const all = await invoke<RepoMeta[]>("list_repos");
        setReposInActiveNs(
          all.filter((r) => r.namespace_id === sessNamespaceId),
        );
        setAllRepos(all);
      } catch (e) {
        console.error("list_repos failed during openSession cross-ns", e);
      }
    }

    setView("session");
    if (sess) {
      setActiveRepoId(sess.repo_id ?? null);
    }
    stickyDoneRef.current = null;
    currentIdRef.current = id;
    setCurrentId(id);
    // 切进该会话即清左栏行的 done/attention（running 不清·仍在跑就仍显）。
    const priorDotStatus = sessionStatusRef.current.get(id);
    if (priorDotStatus === "done" || priorDotStatus === "attention") {
      setSessionDotStatus(id, null);
    }
    setDone(null);
    setSessionUsage(
      sess
        ? sessionUsageFromSession(sess)
        : {
            input: 0,
            output: 0,
          },
    );
    setReview(reviewCacheRef.current.get(id) ?? null);
    const shouldLoadMessages = !messagesRef.current.has(id);
    if (shouldLoadMessages) {
      setSessionLoading(id, true);
    }
    try {
      const [msgs] = await Promise.all([
        shouldLoadMessages
          ? invoke<ChatMessage[]>("get_messages", { sessionId: id })
          : Promise.resolve(null),
        refreshRunStates(id),
      ]);
      if (msgs) {
        const hydratedMsgs = hydrateWorkerReportCards(msgs);
        if (!messagesRef.current.has(id)) setSessionMessages(id, hydratedMsgs);
        const { runIds: runIdsArr, hasTeamHistory } =
          collectReloadRunInfo(hydratedMsgs);
        const runIds = runIdsArr;
        // 恢复协作模式：会话有 team 历史（team_run/lead_summary/dispatch_card 块）→ 团队会话·reload 后恢复 Agent Team 模式
        // dispatch_card（worker run）也算团队历史·且其 run_id 现进 runIds 循环：取 goal_title 回填 topbar 短标题（无验收 criteria·list_acceptance 返空无害）。
        // currentIdRef.current === id 守卫：openSession 是 fire-and-forget 可并发·防慢返回的旧会话把已切走的当前会话 mode 改错（codex 重审逮的竞态·同 C2-B stale 闭包）。
        if (hasTeamHistory && currentIdRef.current === id) setMode("team");
        // run 元数据只用于回填视图，不阻塞会话切换主链；所有 run / 两类 IPC 同时发出。
        // 两张 state map 仅按 runId 索引，故旧会话迟到时必须丢弃，避免覆盖当前会话的同名 run。
        void Promise.all(
          runIds.map(async (rid) => {
            const [criteria, goalTitle] = await Promise.all([
              invoke<AcceptanceCriterion[]>("list_acceptance", {
                sessionId: id,
                runId: rid,
              }).catch(() => []),
              invoke<string | null>("get_run_goal_title", {
                sessionId: id,
                runId: rid,
              }).catch(() => null),
            ]);
            if (currentIdRef.current !== id) return;
            if (criteria.length)
              setAcceptanceByRun((prev) => {
                const next = new Map(prev);
                next.set(rid, criteria);
                return next;
              });
            if (goalTitle) {
              setGoalTitleByRun((prev) => {
                const next = new Map(prev);
                next.set(rid, goalTitle);
                return next;
              });
            }
          }),
        );
        // 会话级 goal 回填（reload 时从后端取会话记忆 goal）
        const sg = await invoke<SessionGoal | null>("get_session_goal", {
          sessionId: id,
        }).catch(() => null);
        if (sg) {
          setSessionGoalBySession((prev) => {
            const next = new Map(prev);
            next.set(id, sg);
            return next;
          });
        }
      }
      const interrupted = await invoke<TeamRunPendingRow[]>(
        "list_interrupted_team_runs",
        { sessionId: id },
      ).catch(() => [] as TeamRunPendingRow[]);
      setInterruptedRunsBySession((prev) => {
        const next = new Map(prev);
        next.set(id, interrupted);
        return next;
      });
    } finally {
      if (shouldLoadMessages) {
        setSessionLoading(id, false);
      }
    }
    // crash 续最小档（决策8）：从后端恢复 autonomy 档（仅此·active 指针本刀不写）
    invoke<{ autonomy: string }>("get_lead_loop_state", { sessionId: id })
      .then((st) => {
        if (!st) return;
        if (currentIdRef.current === id) {
          autonomyRef.current.set(id, st.autonomy);
        }
      })
      .catch(() => {});
    if (
      shouldFetchOnSwitch(
        isReviewPanelVisible(view, rightPanelOpen, rightPanelTab),
        reviewCacheRef.current.has(id),
      )
    ) {
      refreshReview(id);
    }
  }

  async function applyNamespaceRepoSwitch(nsId: string, repoId: string | null) {
    const all = await invoke<RepoMeta[]>("list_repos");
    const nss = await invoke<NamespaceMeta[]>("list_namespaces");
    setNamespaces(nss);
    setAllRepos(all);
    setActiveNamespaceId(nsId);
    setActiveRepoId(repoId);
    setReposInActiveNs(all.filter((r) => r.namespace_id === nsId));
    setRepoGroupExpanded(repoId ? { [repoId]: true } : {});
    currentIdRef.current = null;
    setCurrentId(null);
    setReview(null);
    setView("intro");
  }

  /**
   * 导航 IA（spec §2.A.2·codex BLOCK 契约）· 跨 namespace 一步切 repo·原子持久化。
   * 禁顺序调 onSelectNamespace+onSelectRepo（clobber + 闭包旧 ns）。
   * 持久化 set_active_namespace(nsId) → set_last_active_repo({nsId 显式}) → 刷新 → 一次性 set state。
   * = applyNamespaceRepoSwitch 形状 + 补持久化 IPC（前者只 set 前端 state 不持久化）。
   */
  async function onSelectRepoInNamespace(nsId: string, repoId: string) {
    // 持久化 + 刷新全包进同一 try（对齐既有 onSelectNamespace pattern·codex T3 审 BLOCK-1）：
    // refresh IPC 若 throw 也走 catch return·不留未捕获 promise rejection。state set 仍只在全部 await 成功后一次性执行（原子）。
    // 注：set_active_namespace 成功后 set_last_active_repo 失败仍会留后端半切（ns last_used bump·repo 未写）——与既有
    // onSelectNamespace/onSelectRepo 同档的两调非原子性·真原子需后端单事务命令（超本 plan「不动后端」scope·deferred）。
    try {
      await invoke("set_active_namespace", { id: nsId });
      await invoke("set_last_active_repo", {
        namespaceId: nsId,
        repoId,
      });
      const all = await invoke<RepoMeta[]>("list_repos");
      const nss = await invoke<NamespaceMeta[]>("list_namespaces");
      setNamespaces(nss);
      setAllRepos(all);
      setActiveNamespaceId(nsId);
      setActiveRepoId(repoId);
      setReposInActiveNs(all.filter((r) => r.namespace_id === nsId));
      setRepoGroupExpanded({ [repoId]: true });
      currentIdRef.current = null;
      setCurrentId(null);
      setReview(null);
      setView("intro");
    } catch (e) {
      console.error("onSelectRepoInNamespace failed", e);
      setToast(renderBackendError(String(e), t));
      return;
    }
  }

  async function onConnectGithub() {
    setConnectError(null);
    let picked: string | null = null;
    try {
      const sel = await openDialog({ directory: true, multiple: false });
      picked = typeof sel === "string" ? sel : null;
    } catch {
      return;
    }
    if (!picked) return;
    try {
      const res = await invoke<{ namespace_id: string; repo_id: string }>(
        "connect_github_repo",
        { path: picked },
      );
      await applyNamespaceRepoSwitch(res.namespace_id, res.repo_id);
    } catch (e) {
      const msg = String(e);
      const m = msg.match(/ALREADY_ADDED:(.+)$/);
      if (m) {
        const all = await invoke<RepoMeta[]>("list_repos");
        const existing = all.find((r) => r.id === m[1].trim());
        if (existing) {
          await applyNamespaceRepoSwitch(existing.namespace_id, existing.id);
          return;
        }
      }
      setConnectError(renderBackendError(msg, t));
    }
  }

  async function handleCreateProject(args: NewProjectArgs) {
    try {
      const newId = await invoke<string>("create_local_project", {
        name: args.name,
        newUnderDefault: args.newUnderDefault,
        existingPath: args.existingPath,
        icon: args.icon,
      });
      await invoke("set_active_namespace", { id: "local" });
      await invoke("set_last_active_repo", {
        namespaceId: "local",
        repoId: newId,
      });
      await applyNamespaceRepoSwitch("local", newId);
    } catch (error) {
      setToast(renderBackendError(String(error), t));
      throw error;
    }
  }

  async function handleEditProject(args: {
    name: string;
    icon: string | null;
  }) {
    if (!editingRepo) return;
    try {
      if (args.name !== editingRepo.name) {
        await invoke("rename_repo", { id: editingRepo.id, name: args.name });
      }
      await invoke("set_repo_icon", {
        id: editingRepo.id,
        icon: args.icon,
      });
      const all = await invoke<RepoMeta[]>("list_repos");
      setAllRepos(all);
      setReposInActiveNs(
        all.filter((repo) => repo.namespace_id === activeNamespaceId),
      );
      setEditingRepo(null);
    } catch (error) {
      setToast(renderBackendError(String(error), t));
      throw error;
    }
  }

  async function handleRemoveProject(removedRepoId: string) {
    try {
      await invoke("archive_repo", { id: removedRepoId });
      if (removedRepoId === activeRepoId) {
        await invoke("set_active_namespace", { id: "local" });
        await invoke("set_last_active_repo", {
          namespaceId: "local",
          repoId: "local-default",
        });
        await applyNamespaceRepoSwitch("local", "local-default");
      } else {
        const all = await invoke<RepoMeta[]>("list_repos");
        setAllRepos(all);
        setReposInActiveNs(
          all.filter((repo) => repo.namespace_id === activeNamespaceId),
        );
      }
    } catch (error) {
      setToast(renderBackendError(String(error), t));
    }
  }

  async function handleArchivedChanged() {
    try {
      const all = await invoke<RepoMeta[]>("list_repos");
      setAllRepos(all);
      setReposInActiveNs(
        all.filter((repo) => repo.namespace_id === activeNamespaceId),
      );
      await refreshSessions();
    } catch (error) {
      setToast(renderBackendError(String(error), t));
    }
  }

  async function refreshGroups(repoId: string | null) {
    if (!repoId) {
      setGroups([]);
      return;
    }
    const g = await invoke<GroupMeta[]>("list_groups", { repoId });
    setGroups(g);
  }

  useEffect(() => {
    if (!activeRepoId) {
      setGroups([]);
      return;
    }
    invoke<GroupMeta[]>("list_groups", { repoId: activeRepoId })
      .then(setGroups)
      .catch(() => {});
  }, [activeRepoId]);

  async function onDeleteGroup(id: string) {
    await invoke("delete_group", { id });
    await refreshGroups(activeRepoId);
    await refreshSessions();
  }

  async function onCreateGroup(name: string): Promise<string> {
    const id = crypto.randomUUID();
    await invoke("create_group", { id, repoId: activeRepoId, name });
    await refreshGroups(activeRepoId);
    return id;
  }

  async function onMoveSessionToGroup(
    sessionId: string,
    groupId: string | null,
  ) {
    try {
      await invoke("move_session_to_group", { sessionId, groupId });
      await refreshSessions();
    } catch (e) {
      setToast(
        String(e).includes("GROUP_REPO_MISMATCH")
          ? t("app.session.moveRepoMismatch")
          : String(e),
      );
    }
  }

  async function onRenameGroup(id: string, name: string) {
    await invoke("rename_group", { id, name });
    await refreshGroups(activeRepoId);
  }

  function onRequestDeleteGroup(g: GroupMeta) {
    setGroupDeleteTarget({ id: g.id, name: g.name });
  }

  async function refreshSessions() {
    const list = await invoke<Session[]>("list_sessions");
    setSessions(list);
    return list;
  }

  function setContinuationDraft(
    sessionId: string,
    state: ContinuationDraftState,
  ) {
    const next = new Map(continuationDraftsRef.current);
    next.set(sessionId, state);
    continuationDraftsRef.current = next;
    setContinuationDrafts(next);
  }

  function clearContinuationDraft(sessionId: string) {
    const next = new Map(continuationDraftsRef.current);
    next.delete(sessionId);
    continuationDraftsRef.current = next;
    setContinuationDrafts(next);
  }

  async function generateContinuationDraft(sessionId: string, retry = false) {
    const current = continuationDraftsRef.current.get(sessionId);
    if (
      current?.status === "loading" ||
      (!retry && current?.status === "ready")
    ) {
      return;
    }
    const generation =
      (continuationDraftGenerationRef.current.get(sessionId) ?? 0) + 1;
    continuationDraftGenerationRef.current.set(sessionId, generation);
    const requestId = `handoff-${Date.now()}-${++continuationRequestSeqRef.current}`;
    continuationDraftRequestIdRef.current.set(sessionId, requestId);
    setContinuationDraft(sessionId, { status: "loading" });
    try {
      const pendingCancellation =
        continuationCancellationRef.current.get(sessionId);
      if (pendingCancellation) {
        await pendingCancellation;
        if (
          continuationDraftGenerationRef.current.get(sessionId) !== generation
        ) {
          return;
        }
      }
      let result: ContinuationHandoffDraft;
      for (let busyAttempt = 0; ; busyAttempt += 1) {
        try {
          result = await invoke<ContinuationHandoffDraft>(
            "generate_handoff_doc",
            { sessionId, requestId },
          );
          break;
        } catch (error) {
          const busy = String(error).includes(
            "SESSION_BUSY:generate_handoff_doc",
          );
          if (!busy || busyAttempt >= HANDOFF_BUSY_RETRY_LIMIT) throw error;
          await new Promise((resolve) =>
            window.setTimeout(resolve, HANDOFF_BUSY_RETRY_DELAY_MS),
          );
          if (
            continuationDraftGenerationRef.current.get(sessionId) !== generation
          ) {
            return;
          }
        }
      }
      if (
        continuationDraftGenerationRef.current.get(sessionId) !== generation
      ) {
        return;
      }
      setContinuationDraft(sessionId, {
        status: "ready",
        draft: result.doc_markdown,
        suggestedTitle: result.suggested_title,
        warnings: result.warnings,
      });
      if (currentIdRef.current !== sessionId || viewRef.current !== "session") {
        setContinuationReadySessionIds((previous) => {
          const next = new Set(previous);
          next.add(sessionId);
          return next;
        });
        const title =
          sessions.find((session) => session.id === sessionId)?.title ??
          sessionId;
        setToast(t("continuation.notice.ready", { title }));
      }
    } catch (error) {
      if (
        continuationDraftGenerationRef.current.get(sessionId) !== generation
      ) {
        return;
      }
      const raw = String(error);
      setContinuationDraft(sessionId, {
        status: "error",
        error: renderBackendError(raw, t),
      });
    } finally {
      if (continuationDraftRequestIdRef.current.get(sessionId) === requestId) {
        continuationDraftRequestIdRef.current.delete(sessionId);
      }
    }
  }

  function cancelContinuationDraft(sessionId: string) {
    const generation =
      (continuationDraftGenerationRef.current.get(sessionId) ?? 0) + 1;
    continuationDraftGenerationRef.current.set(sessionId, generation);
    clearContinuationDraft(sessionId);
    setContinuationParentId(null);
    const requestId = continuationDraftRequestIdRef.current.get(sessionId);
    if (!requestId) return;
    const cancellation = invoke("cancel_handoff_generation", {
      sessionId,
      requestId,
    })
      .catch((error) => {
        console.warn("cancel_handoff_generation failed", error);
      })
      .then(() => undefined);
    continuationCancellationRef.current.set(sessionId, cancellation);
    void cancellation.finally(() => {
      if (continuationCancellationRef.current.get(sessionId) === cancellation) {
        continuationCancellationRef.current.delete(sessionId);
      }
    });
  }

  async function onHandoverSession(id: string) {
    const target = sessions.find((s) => s.id === id);
    if (
      !target ||
      target.archived ||
      getSessionReadonlyReason(id) ||
      runningSessionsRef.current.has(id) ||
      continuationAssemblingId === id
    ) {
      return;
    }
    const requestSeq = continuationAssembleSeqRef.current + 1;
    continuationAssembleSeqRef.current = requestSeq;
    setContinuationAssemblingId(id);
    setContinuationParentId(null);
    void generateContinuationDraft(id);
    try {
      if (continuationAssembleSeqRef.current !== requestSeq) return;
      setView("session");
      if (currentIdRef.current !== id) {
        await openSession(id);
      }
      if (continuationAssembleSeqRef.current !== requestSeq) return;
      setContinuationParentId(id);
    } catch (e) {
      if (continuationAssembleSeqRef.current !== requestSeq) return;
      setContinuationParentId(null);
      setToast(String(e));
    } finally {
      if (continuationAssembleSeqRef.current === requestSeq) {
        setContinuationAssemblingId(null);
      }
    }
  }

  async function onStartContinuation({
    parentSessionId,
    handoffDoc,
    suggestedTitle,
  }: {
    parentSessionId: string;
    handoffDoc: string;
    suggestedTitle?: string;
  }) {
    if (guardReadonlySession(parentSessionId)) return;
    setContinuationStarting(true);
    try {
      const result = await invoke<
        string | { session_id?: string; id?: string }
      >("start_continuation_session", {
        parentSessionId,
        handoffDoc,
        suggestedTitle,
      });
      const childId =
        typeof result === "string" ? result : (result.session_id ?? result.id);
      if (!childId)
        throw new Error("start_continuation_session returned no id");
      setContinuationParentId(null);
      const list = await refreshSessions();
      await openSession(childId, list);
    } catch (e) {
      const raw = String(e);
      const envelope = parseBackendError(raw);
      setToast(
        envelope?.code === "continuation.startCleanupFailed"
          ? t("backend.continuation.startCleanupFailed", {
              ...envelope.params,
              original: renderBackendError(envelope.params.original, t),
            })
          : renderBackendError(raw, t),
      );
    } finally {
      setContinuationStarting(false);
    }
  }

  // session-hover-menu §3.6：同 scope 活动列表（list 已按 pinned/created_at/id 排好）
  function activeScopedSorted(src: Session[]): Session[] {
    return src.filter(
      (s) =>
        !s.archived &&
        (activeRepoId === null ? true : (s.repo_id ?? null) === activeRepoId),
    );
  }

  async function onNewSession() {
    if (activeRepoId === null) {
      // B3：0 repo namespace 禁建 · Sidebar 已 disable 按钮兜底
      return;
    }
    const activeRepo = allRepos.find((r) => r.id === activeRepoId);
    const namespaceId = activeRepo?.namespace_id ?? activeNamespaceId;
    const sid = crypto.randomUUID();
    try {
      await invoke("create_session", {
        id: sid,
        title: t("app.session.new"),
        repoId: activeRepoId,
        namespaceId,
      });
      const list = await refreshSessions();
      await openSession(sid, list);
    } catch (e) {
      setToast(renderBackendError(String(e), t));
    }
  }

  async function createSessionAndSend(
    text: string,
    mode: Mode,
    config?: ComposerRuntimeConfig,
  ) {
    if (activeRepoId === null) return;
    if (mode === "team" && teamConfigBlocked) return;
    // 零可用 agent → 不建会话/不发（review：前移到 create_session 之前·避免残留孤儿空会话）。
    // UI 层 ProjectIntroPage canSend 已挡按钮·此为防御纵深。
    if (!availableAgents.some((a) => a.id === agentId)) return;
    const teamConfigSnapshot =
      mode === "team" && composerTeamCfg.leadId !== null
        ? {
            leadId: composerTeamCfg.leadId,
            rosterIds: [...composerTeamCfg.rosterIds],
          }
        : null;

    const activeRepo = allRepos.find((r) => r.id === activeRepoId);
    const namespaceId = activeRepo?.namespace_id ?? activeNamespaceId;
    const sid = crypto.randomUUID();
    try {
      await invoke("create_session", {
        id: sid,
        title: t("app.session.new"),
        repoId: activeRepoId,
        namespaceId,
      });
      if (teamConfigSnapshot) {
        await saveSessionTeamConfig(sid, teamConfigSnapshot);
      }
      const list = await refreshSessions();
      await openSession(sid, list);

      if (mode === "team") {
        startLeadSessionForComposer(sid, text, config);
        return;
      }

      const arr = messagesRef.current.get(sid) ?? [];
      if (arr.length === 0) {
        const title = deriveSessionTitle(text) || t("app.session.new");
        invoke("rename_session", { id: sid, title }).then(() =>
          refreshSessions(),
        );
      }
      setDone(null);
      const selectedAgentId = agentId;
      const agentNameSnapshot = agentNameSnapshotFor(selectedAgentId);
      setSessionMessages(sid, [
        ...arr,
        {
          id: crypto.randomUUID(),
          role: "user",
          content: [{ type: "text", text }],
        },
        {
          id: crypto.randomUUID(),
          role: "assistant",
          content: [],
          engine: selectedAgentId,
          agent_id: selectedAgentId,
          agent_name_snapshot: agentNameSnapshot,
        },
      ]);
      setRun(sid, {
        startedAt: Date.now(),
        workingTokens: null,
        engine: selectedAgentId,
        agent_id: selectedAgentId,
        agent_name_snapshot: agentNameSnapshot,
      });
      invoke(
        "send_message",
        sendMessagePayload(sid, selectedAgentId, text, config),
      ).catch((err) => {
        if (String(err).startsWith("SESSION_ALREADY_RUNNING:")) return;
        setRun(sid, null);
        if (handleProjectError(err)) {
          setSessionMessages(sid, arr);
          return;
        }
        setSessionMessages(sid, [
          ...arr,
          {
            id: crypto.randomUUID(),
            role: "user",
            content: [{ type: "text", text }],
          },
          {
            id: crypto.randomUUID(),
            role: "assistant",
            content: [
              {
                type: "text",
                text: t("app.run.startFailed", {
                  error: renderBackendError(String(err), t),
                }),
              },
            ],
            engine: selectedAgentId,
            agent_id: selectedAgentId,
            agent_name_snapshot: agentNameSnapshot,
          },
        ]);
      });
    } catch (e) {
      if (!handleProjectError(e)) setToast(renderBackendError(String(e), t));
    }
  }

  async function onDeleteSession(id: string) {
    const i = activeScopedSorted(sessions).findIndex((s) => s.id === id);
    await invoke("delete_session", { id });
    setSessionDotStatus(id, null);
    const list = await refreshSessions();

    // 剪枝导航历史：移除所有该会话的条目
    const pruned = pruneNavHistory(
      navHistoryRef.current,
      navIndexRef.current,
      id,
    );
    navHistoryRef.current = pruned.history;
    navIndexRef.current = pruned.index;
    syncNavState();

    if (id === currentId) {
      const rest = activeScopedSorted(list).filter((s) => s.id !== id);
      if (rest.length === 0) {
        setCurrentId(null);
        setView("intro");
        return;
      }
      const target = rest[Math.min(Math.max(i, 0), rest.length - 1)];
      await openSession(target.id, list);
    }
  }

  function activeContinuationDescendantCount(parentId: string): number {
    const childrenByParent = new Map<string, Session[]>();
    for (const session of sessions) {
      if (session.archived || session.parent_session_id == null) continue;
      const children = childrenByParent.get(session.parent_session_id) ?? [];
      children.push(session);
      childrenByParent.set(session.parent_session_id, children);
    }
    const stack = [...(childrenByParent.get(parentId) ?? [])];
    const seen = new Set<string>();
    let count = 0;
    while (stack.length > 0) {
      const child = stack.pop()!;
      if (seen.has(child.id)) continue;
      seen.add(child.id);
      count += 1;
      stack.push(...(childrenByParent.get(child.id) ?? []));
    }
    return count;
  }

  function deleteSessionConfirmBody(target: { id: string } | null): string {
    if (!target) return t("app.session.deleteBody");
    const liveChildren = activeContinuationDescendantCount(target.id);
    if (liveChildren <= 0) return t("app.session.deleteBody");
    return t("app.session.deleteBodyWithContinuations", {
      count: liveChildren,
    });
  }

  function removeProjectConfirmBody(target: {
    id: string;
    name: string;
  }): string {
    const count = sessions.filter(
      (session) => session.repo_id === target.id && !session.archived,
    ).length;
    return t("removeProject.confirm.body", {
      name: target.name,
      count: String(count),
    });
  }

  async function onRenameSession(id: string, title: string) {
    await invoke("rename_session", { id, title });
    await refreshSessions();
  }

  async function onTogglePin(id: string, pinned: boolean) {
    await invoke("set_session_pinned", { id, pinned });
    await refreshSessions();
  }

  async function onToggleUnread(id: string, unread: boolean) {
    await invoke("set_session_unread", { id, unread });
    await refreshSessions();
  }

  async function onToggleArchive(id: string, archived: boolean) {
    // pre-index（mutation 前·用当前 state 的同 scope 活动序）
    const i = activeScopedSorted(sessions).findIndex((s) => s.id === id);
    await invoke("set_session_archived", { id, archived });
    setSessionDotStatus(id, null);
    const list = await refreshSessions();

    // 归档时剪枝导航历史，解归档不回填
    if (archived) {
      const pruned = pruneNavHistory(
        navHistoryRef.current,
        navIndexRef.current,
        id,
      );
      navHistoryRef.current = pruned.history;
      navIndexRef.current = pruned.index;
      syncNavState();
    }

    if (id === currentIdRef.current) {
      const rest = activeScopedSorted(list).filter((s) => s.id !== id);
      if (rest.length === 0) {
        setCurrentId(null);
        setView("intro");
        return;
      }
      const target = rest[Math.min(Math.max(i, 0), rest.length - 1)];
      await openSession(target.id, list);
    }
  }

  function onStop() {
    const sid = currentIdRef.current;
    if (!sid) return;
    stopIssuedAtRef.current.set(sid, Date.now());
    invoke("stop_session", { sessionId: sid })
      .catch(() => {})
      .finally(() => {
        // 后端 slot 已空时可能没有可等待的终态；调用结束仍 running 就由前端兜底释放。
        // 保留停止时间闸，防随后迟到的空标识 closeout 清掉用户紧接着启动的新 run。
        if (runningSessionsRef.current.has(sid)) {
          setRun(sid, null, { preserveStopGate: true });
        }
      });
    const swept = sweepSession(sid);
    const next = new Map(messagesRef.current);
    next.set(sid, swept);
    messagesRef.current = next;
    setMessagesBySession(next);
  }

  function onSend(text: string, mode: Mode, config?: ComposerRuntimeConfig) {
    const sid = currentIdRef.current;
    if (!sid) return;
    if (loadingSessionsRef.current.has(sid)) return;
    if (runningSessionsRef.current.has(sid)) return;
    if (getSessionReadonlyReason(sid)) return;
    // 用户手动发消息 = 明确接管，清零自动续喂连续计数（见 autoResumeStreakRef 旁注）。
    resetAutoResumeStreak(autoResumeStreakRef, sid);
    if (mode === "team") {
      if (teamConfigBlocked) return;
      startLeadSessionForComposer(sid, text, config);
      return;
    }
    const arr = messagesRef.current.get(sid) ?? [];
    // 首条消息自动命名：本会话还没有消息时，用消息截断作标题（替代恒为「新会话」）
    if (arr.length === 0) {
      const title = deriveSessionTitle(text) || t("app.session.new");
      invoke("rename_session", { id: sid, title }).then(() =>
        refreshSessions(),
      );
    }
    setDone(null);
    if (!sendGate.effectiveAgentId) return;
    const selectedAgentId = sendGate.effectiveAgentId;
    const agentNameSnapshot = agentNameSnapshotFor(selectedAgentId);
    setSessionMessages(sid, [
      ...arr,
      {
        id: crypto.randomUUID(),
        role: "user",
        content: [{ type: "text", text }],
      },
      {
        id: crypto.randomUUID(),
        role: "assistant",
        content: [],
        engine: selectedAgentId,
        agent_id: selectedAgentId,
        agent_name_snapshot: agentNameSnapshot,
      },
    ]);
    setRun(sid, {
      startedAt: Date.now(),
      workingTokens: null,
      engine: selectedAgentId,
      agent_id: selectedAgentId,
      agent_name_snapshot: agentNameSnapshot,
    });
    // user 由后端 send_message 落库；assistant 由前端 completed 落库
    invoke(
      "send_message",
      sendMessagePayload(sid, selectedAgentId, text, config),
    ).catch((err) => {
      if (String(err).startsWith("SESSION_ALREADY_RUNNING:")) return;
      setRun(sid, null);
      // PROJECT_INVALID / PROJECT_ARCHIVED / ALREADY_ADDED 弹 dialog / 切 repo · 不污染消息流
      if (handleProjectError(err)) {
        setSessionMessages(sid, arr);
        return;
      }
      setSessionMessages(sid, [
        ...arr,
        {
          id: crypto.randomUUID(),
          role: "user",
          content: [{ type: "text", text }],
        },
        {
          id: crypto.randomUUID(),
          role: "assistant",
          content: [
            {
              type: "text",
              text: t("app.run.startFailed", {
                error: renderBackendError(String(err), t),
              }),
            },
          ],
          engine: selectedAgentId,
          agent_id: selectedAgentId,
          agent_name_snapshot: agentNameSnapshot,
        },
      ]);
    });
  }

  // run_card「查看」走 Review；Agent Team「查看过程」带 runId，二次点击同一 run 收回右侧过程。
  const onViewRun = useCallback(
    (runId?: string) => {
      if (!runId) {
        setDrillRun(null);
        setUndoTarget(null);
        setRightPanelOpen(true);
        setInspectorTarget(null);
        setRightPanelTab("review");
        return;
      }
      if (rightPanelOpen && drillRun?.runId === runId) {
        setDrillRun(null);
        setRightPanelOpen(false);
        setRightPanelExpanded(false);
        return;
      }
      const members = membersForRun(runId);
      const first = members?.[0];
      if (!first) return;
      tabBeforeDrillRef.current = rightPanelTab;
      setInspectorTarget(null);
      setUndoTarget(null);
      setDrillRun({ runId, assignmentId: first.assignment_id });
      setRightPanelOpen(true);
    },
    [
      currentId,
      drillRun?.runId,
      rightPanelOpen,
      rightPanelTab,
      teamRunsBySession,
    ],
  );
  const onUndoRun = useCallback((runId: string) => {
    const sessionId = currentIdRef.current;
    if (!sessionId) return;
    setInspectorTarget(null);
    setDrillRun(null);
    setShowTaskList(false);
    setUndoTarget({ sessionId, runId });
    setRightPanelTab("review");
    setRightPanelOpen(true);
  }, []);

  const handleUndoComplete = useCallback(
    (result: UndoResultRecord) => {
      setUndoFeedback((previous) => {
        const next = new Map(previous);
        next.set(undoFeedbackKey(result.session_id, result.run_id), result);
        return next;
      });
      void refreshRunStates(result.session_id);
    },
    [refreshRunStates],
  );
  const handleExitUndo = useCallback(() => {
    setUndoTarget(null);
    if (!undoTarget) return;
    setUndoFeedback((previous) => {
      const next = new Map(previous);
      next.delete(undoFeedbackKey(undoTarget.sessionId, undoTarget.runId));
      return next;
    });
  }, [undoTarget]);
  const fallbackGhLogin =
    ghAccounts.find((a) => a.active)?.login ?? ghAccounts[0]?.login ?? "";
  const selectedGhLogin = selectedLogin || fallbackGhLogin;
  const ghGate = computeGate(
    gitInstalled,
    ghInstalled,
    ghAccounts.length,
    {
      canBrew,
      installing: installState.installing,
      error: installState.error,
    },
    ghAccountError,
  );
  const repoManageAccounts = useMemo(
    () =>
      ghAccounts.map((account) => ({
        ...account,
        count: repoCacheByLogin[account.login]?.repos?.length,
      })),
    [ghAccounts, repoCacheByLogin],
  );
  const repoListView = useMemo(
    () => deriveView(repoCacheByLogin[selectedGhLogin]),
    [repoCacheByLogin, selectedGhLogin],
  );
  const currentSelectedRepoKeys = useMemo(
    () => selectedByLogin[selectedGhLogin] ?? new Set<RepoKey>(),
    [selectedByLogin, selectedGhLogin],
  );
  const markCloneBatchStarted = useCallback(
    (keys: RepoKey[], login: string, orderByKey: Map<RepoKey, number>) => {
      setCloneProgressEntries((prev) => {
        const next = { ...prev };
        keys.forEach((key, index) => {
          const { repoOwner, name } = splitRepoKey(key);
          next[key] = {
            login,
            ["ow" + "ner"]: repoOwner,
            name,
            order: orderByKey.get(key) ?? index,
            phase: "cloning",
          } as CloneProgressEntry;
        });
        return next;
      });
    },
    [setCloneProgressEntries],
  );
  const updateCloneProgressEntry = useCallback(
    (
      key: RepoKey,
      st: CloneRowState,
      login: string,
      orderByKey: Map<RepoKey, number>,
    ) => {
      const { repoOwner, name } = splitRepoKey(key);
      setCloneProgressEntries((prev) => {
        const existing = prev[key];
        const base = {
          login: existing?.login ?? login,
          ["ow" + "ner"]: cloneEntryRepoOwner(existing) ?? repoOwner,
          name: existing?.name ?? name,
          order: existing?.order ?? orderByKey.get(key) ?? 0,
        };
        const nextEntry = (
          st.phase === "done"
            ? {
                ...base,
                phase: "done",
                repoId: st.repoId,
                settledAt: Date.now(),
              }
            : st.phase === "fail"
              ? {
                  ...base,
                  phase: "fail",
                  message: renderBackendError(st.message, t),
                  settledAt: Date.now(),
                }
              : st.phase === "occupied"
                ? {
                    ...base,
                    phase: "occupied",
                    settledAt: Date.now(),
                  }
                : { ...base, phase: "cloning" }
        ) as CloneProgressEntry;

        return { ...prev, [key]: nextEntry };
      });
      if (st.phase === "occupied") {
        setSelectedByLogin((prev) => {
          const selected = prev[login];
          if (!selected?.has(key)) return prev;
          const next = new Set(selected);
          next.delete(key);
          return { ...prev, [login]: next };
        });
      }
    },
    [setCloneProgressEntries, t],
  );
  const recordRepoCloneSuccess = useCallback(
    (login: string, key: RepoKey, result: ClonedRepoResult) => {
      updateRepoCacheByLogin((prev) => {
        const previous: RepoCacheEntry = prev[login] ?? {
          status: "idle" as const,
          requestId: 0,
          mutationGen: 0,
        };
        const repos = previous.repos?.map((repo) =>
          repoKey(repo) === key
            ? {
                ...repo,
                cloned: true,
                repo_id: result.repo_id,
                local_path: result.dest,
              }
            : repo,
        );

        return {
          ...prev,
          [login]: {
            ...previous,
            repos,
            status: previous.repos ? "ready" : previous.status,
            error: previous.repos ? undefined : previous.error,
            mutationGen: previous.mutationGen + 1,
          },
        };
      });
      setSelectedByLogin((prev) => {
        const selected = prev[login];
        if (!selected?.has(key)) return prev;
        const next = new Set(selected);
        next.delete(key);
        return { ...prev, [login]: next };
      });
    },
    [updateRepoCacheByLogin],
  );
  const cloneRepoKey = useCallback(
    async (key: RepoKey, login: string): Promise<{ repoId: string }> => {
      const { repoOwner, name } = splitRepoKey(key);
      const result = await invoke<ClonedRepoResult>("gh_clone_repo", {
        login,
        ["ow" + "ner"]: repoOwner,
        name,
      });
      recordRepoCloneSuccess(login, key, result);
      return { repoId: result.repo_id };
    },
    [recordRepoCloneSuccess],
  );
  const runCloneBatch = useCallback(
    async (
      login: string,
      keys: RepoKey[],
      orderByKey: Map<RepoKey, number>,
    ) => {
      await runClones(
        keys,
        (key) => cloneRepoKey(key, login),
        (key, st) => updateCloneProgressEntry(key, st, login, orderByKey),
        CLONE_CONCURRENCY,
      );
    },
    [cloneRepoKey, updateCloneProgressEntry],
  );
  const refreshReposAfterClone = useCallback(
    async (login: string) => {
      try {
        const repos = await invoke<RepoMeta[]>("list_repos");
        setAllRepos(repos);
      } catch (e) {
        console.error("list_repos failed after clone batch", e);
      }
      if (login) await loadRepoList(login, { force: true });
    },
    [loadRepoList],
  );
  const onToggleSelectedRepo = useCallback(
    (key: RepoKey) => {
      if (!selectedGhLogin) return;
      setSelectedByLogin((prev) => {
        const next = new Set(prev[selectedGhLogin] ?? []);
        if (next.has(key)) next.delete(key);
        else next.add(key);
        return { ...prev, [selectedGhLogin]: next };
      });
    },
    [selectedGhLogin],
  );
  const onCloneSelectedRepos = useCallback(async () => {
    const login = selectedGhLogin;
    if (!login) return;
    const cacheEntry = repoCacheByLoginRef.current[login];
    const pruned = pruneSelection(
      currentSelectedRepoKeys,
      cacheEntry?.repos ?? [],
    );
    const keys = Array.from(pruned).filter((key) => {
      const existing = cloneProgressRef.current[key];
      return !(
        existing?.phase === "cloning" ||
        existing?.phase === "done" ||
        existing?.phase === "occupied"
      );
    });
    if (keys.length === 0) return;
    const orderByKey = new Map<RepoKey, number>(
      keys.map((key, index) => [key, index]),
    );
    markCloneBatchStarted(keys, login, orderByKey);
    await runCloneBatch(login, keys, orderByKey);
    await refreshReposAfterClone(login);
  }, [
    currentSelectedRepoKeys,
    markCloneBatchStarted,
    refreshReposAfterClone,
    runCloneBatch,
    selectedGhLogin,
  ]);
  const onRetryClone = useCallback(
    async (key: RepoKey) => {
      const existing = cloneProgressRef.current[key];
      if (existing?.phase !== "fail") return;
      const login = existing?.login ?? selectedGhLogin;
      if (!login) return;
      const orderByKey = new Map<RepoKey, number>([
        [key, existing?.order ?? 0],
      ]);
      markCloneBatchStarted([key], login, orderByKey);
      await runCloneBatch(login, [key], orderByKey);
      await refreshReposAfterClone(login);
    },
    [
      markCloneBatchStarted,
      refreshReposAfterClone,
      runCloneBatch,
      selectedGhLogin,
    ],
  );
  const onRetryFailedClones = useCallback(async () => {
    const login = selectedGhLogin;
    if (!login) return;
    const failedEntries = (
      Object.entries(cloneProgressRef.current) as Array<
        [RepoKey, CloneProgressEntry]
      >
    )
      .filter(([, entry]) => entry.login === login && entry.phase === "fail")
      .sort((a, b) => a[1].order - b[1].order);
    const keys = failedEntries.map(([key]) => key);
    if (keys.length === 0) return;
    const orderByKey = new Map<RepoKey, number>(
      failedEntries.map(([key, entry]) => [key, entry.order]),
    );
    markCloneBatchStarted(keys, login, orderByKey);
    await runCloneBatch(login, keys, orderByKey);
    await refreshReposAfterClone(login);
  }, [
    markCloneBatchStarted,
    refreshReposAfterClone,
    runCloneBatch,
    selectedGhLogin,
  ]);
  const onOpenClonedSession = useCallback(
    async (target: RepoOpenSessionTarget) => {
      try {
        const repoId = target.repo_id;
        if (!repoId && target.local_path) {
          const connected = await invoke<{
            namespace_id: string;
            repo_id: string;
          }>("connect_github_repo", { path: target.local_path });
          await applyNamespaceRepoSwitch(
            connected.namespace_id,
            connected.repo_id,
          );
          setSettingsOpen(false);
          return;
        }
        if (!repoId) return;

        let repo = allRepos.find((r) => r.id === repoId);
        if (!repo) {
          const repos = await invoke<RepoMeta[]>("list_repos");
          setAllRepos(repos);
          repo = repos.find((r) => r.id === repoId);
        }
        if (!repo) return;
        await applyNamespaceRepoSwitch(repo.namespace_id, repoId);
        setSettingsOpen(false);
      } catch (e) {
        setToast(renderBackendError(String(e), t));
      }
    },
    [allRepos, t],
  );
  const onInstallGh = useCallback(async () => {
    setInstallState({ installing: true });
    try {
      await invoke("install_gh");
      const result = await invoke<DetectResult>("detect_gh").catch(() => null);
      setGhInstalled(Boolean(result?.available));
      repoToolsLoadedRef.current = true;
      await refreshGhAccounts();
      setInstallState({ installing: false });
    } catch (e) {
      const message = String(e);
      if (
        message.includes("NO_BREW") ||
        message.includes("UNSUPPORTED_PLATFORM")
      ) {
        setCanBrew(false);
      }
      setInstallState({ installing: false, error: message });
    }
  }, [refreshGhAccounts]);
  const onRetryList = useCallback(() => {
    if (ghGate.kind !== "ready") {
      void loadRepoTools(true);
      return;
    }
    if (selectedGhLogin) void loadRepoList(selectedGhLogin, { force: true });
  }, [ghGate.kind, loadRepoList, loadRepoTools, selectedGhLogin]);
  const onSelectRepoAccount = useCallback(
    (login: string) => {
      if (login === selectedGhLogin) return;
      setSelectedLogin(login);
    },
    [selectedGhLogin],
  );

  const gateView = gateBySession.get(currentId ?? "") ?? null;
  const onGateAction = (a: GateAction) => {
    const sid = currentId;
    if (!sid) return;
    if (guardReadonlySession(sid)) return;
    setGateBySession((prev) => {
      const v = prev.get(sid);
      if (v?.kind !== "draft") return prev;
      const next = new Map(prev);
      next.set(sid, { kind: "draft", draft: gateReducer(v.draft, a) });
      return next;
    });
  };

  function lastUserText(sid: string): string | null {
    const msgs = messagesRef.current.get(sid) ?? [];
    for (let i = msgs.length - 1; i >= 0; i--) {
      if (msgs[i].role === "user") {
        const t = msgs[i].content.find((b) => b.type === "text");
        if (t && t.type === "text") return t.text;
      }
    }
    return null;
  }

  const clearGate = (sid: string) =>
    setGateBySession((prev) => {
      const next = new Map(prev);
      next.delete(sid);
      return next;
    });

  // 让队长重拟 / 重试拟：丢 draft 再 propose（无 note·折入 #9 砍带批注）
  const onGateRedraft = () => {
    if (composerTeamActive) return;
    const sid = currentId;
    if (!sid) return;
    if (guardReadonlySession(sid)) return;
    const goal = lastUserText(sid);
    if (goal) runProposeForSession(sid, goal);
  };
  const onGateRetry = onGateRedraft;

  // 手动填 gate：渲空 GateCard（emptyDraft·manual:true·新生成唯一 runId 避免 UNIQUE 撞）
  const onGateManual = () => {
    if (composerTeamActive) return;
    const sid = currentId;
    if (!sid) return;
    if (guardReadonlySession(sid)) return;
    const runId = `${sid}-manual-${crypto.randomUUID()}`;
    setGateBySession((prev) => {
      const next = new Map(prev);
      next.set(sid, { kind: "draft", draft: emptyDraft(runId, `${runId}-gc`) });
      return next;
    });
  };

  const onGateBackToNormal = () => {
    const sid = currentId;
    if (!sid) return;
    if (guardReadonlySession(sid)) return;
    clearGate(sid);
    setMode("normal");
  };

  const onCodingConfirmVerify = async (runId: string, cmd: string) => {
    const s = codingLoopsRef.current.get(runId);
    if (!s) return;
    if (guardReadonlySession(s.sessionId)) return;
    const next = { ...s, verifyCmd: cmd, phase: "verifying" as const };
    codingLoopsRef.current.set(runId, next);
    upsertCodingTaskBlock(runId, blockFromCodingState(runId, next));
    await driveCodingLoop(runId);
  };

  const onCodingRetryVerify = (runId: string) => {
    const s = codingLoopsRef.current.get(runId);
    if (!s) return;
    if (guardReadonlySession(s.sessionId)) return;
    const next = { ...s, phase: "ask_verify" as const };
    codingLoopsRef.current.set(runId, next);
    upsertCodingTaskBlock(runId, blockFromCodingState(runId, next));
  };

  // 内存同步翻某 decision_card 块 status（镜像 b0 CAS·让卡即时反映 submitting/chosen/failed）
  function setDecisionStatusInMemory(
    sid: string,
    decisionId: string,
    // "pending"：决策打扰收敛刀 T1·症状 B 根修新增——MCP ask_user 卡点击后先乐观置
    // submitting，若 answer_lead_question 失败（非 NO_PENDING_QUESTION）须回滚回 pending
    // 让用户能重新点选，而不是卡死在灰态。
    status: "pending" | "submitting" | "chosen" | "failed",
    chosenOption?: string,
  ) {
    const arr = messagesRef.current.get(sid) ?? [];
    const next = arr.map((m) => ({
      ...m,
      content: m.content.map((b) =>
        b.type === "decision_card" && b.decision_id === decisionId
          ? {
              ...b,
              status,
              ...(chosenOption !== undefined
                ? { chosen_option: chosenOption }
                : {}),
            }
          : b,
      ),
    }));
    setSessionMessages(sid, next);
  }

  function findDecisionCard(
    sid: string,
    decisionId: string,
  ): DecisionCardBlock | null {
    for (const m of messagesRef.current.get(sid) ?? [])
      for (const b of m.content)
        if (b.type === "decision_card" && b.decision_id === decisionId)
          return b;
    return null;
  }

  const onDecisionChoose = async (
    sid: string,
    decisionId: string,
    option: string,
  ) => {
    if (guardReadonlySession(sid)) return;
    if (leadChoosingRef.current.has(sid)) return; // 前端防重入
    const card = findDecisionCard(sid, decisionId);
    // 仅 pending（首次）或 failed（重试·双路 review BLOCK：failed 态重试按钮必须能走通）可点；
    // submitting/chosen 不重入。
    if (!card || (card.status !== "pending" && card.status !== "failed"))
      return;
    // T5：本地「看一眼再派」确认卡 → 前端直接派/取消·不走后端 choose_decision_card·不喂回 lead。
    // 据 source_run_id 前缀判（每张本地卡都是 `local-dispatch-${uuid}`·唯一）；后端持久化卡
    // 带真 per-run source_run_id·不会命中此前缀。
    if (card.source_run_id.startsWith(`${LOCAL_DISPATCH_PREFIX}-`)) {
      onLocalDispatchConfirm(sid, card, option);
      return;
    }
    // MCP 队长决策卡（ask_user / propose_verifier）：据 source_run_id 前缀按【身份】路由——
    // handler 阻塞在 channel 上·answer_lead_question 解阻塞。**MCP 卡绝不回退 legacy lead_step**。
    // （旧实现靠探测 NO_PENDING_QUESTION 当 legacy 判据·但「已取消/已消费的 MCP 卡」也返
    // NO_PENDING_QUESTION → 会在 stopped 会话上误触发 lead_step LLM 跑·整支终审 opus Important。）
    // 决策打扰收敛刀 T1：后端 answer_lead_question 现在需要 sessionId 才能在「迟到答案」
    // （ask_user 有界等待 240 秒超时后 handler 已体面退出）时落库——卡置 chosen + 转一条真实
    // 用户消息喂给 lead 下一轮，这条路径与「handler 还活着、当场应答」路径一样返回 Ok，
    // 下面 try 分支统一处理，不需要按「准点/迟到」分叉。
    if (card.source_run_id.startsWith(`${MCP_LEAD_PREFIX}-`)) {
      leadChoosingRef.current.add(sid);
      // 决策打扰收敛刀 T1·症状 B 根修：await 前先乐观置 submitting——原来这里在 invoke
      // 落定前不改任何状态，点击后按钮不置灰、没有任何"已收到点击"的反馈。
      setDecisionStatusInMemory(sid, decisionId, "submitting");
      try {
        await invoke("answer_lead_question", {
          sessionId: sid,
          decisionId,
          answer: option,
        });
        setDecisionStatusInMemory(sid, decisionId, "chosen", option);
        // 答完且队长仍在跑 → 立刻另起续写消息·busy 时显示「工作中」·填队长思考空窗（块②a-1 体验 b·选完到首字之间不留空白）。
        if (runningSessionsRef.current.has(sid)) {
          mutateSession(sid, (m) =>
            ensureStreamTail(m, leadStreamIdentity(sid)),
          );
        } else {
          const runtime = resolveRuntimeTeamConfig(sid);
          // G3 停摆修复（T3）只适用于有持久化 lead 配置的 team 会话：solo 的迟到答案
          // 已由 commit_late_answer 落成真实 user 消息，留给下一轮普通 run 自然消费，不能把
          // effectiveLeadId 对 agentId 的回退误当成 lead 身份去调用 resume_lead_session。
          if (runtime.hasSavedLead) {
            // resume_lead_session 不落新消息（避免答案在 transcript 里重复），直接以现有历史
            // （已含刚落的答案）起新 run；并发安全复用它内部同一套
            // reserve_new_session_run 互斥闸，不新造锁。
            const leadId = runtime.effectiveLeadId;
            const identity = {
              engine: leadId,
              agent_id: leadId,
              agent_name_snapshot: agentNameSnapshotFor(leadId),
            };
            setRun(sid, {
              startedAt: Date.now(),
              workingTokens: null,
              ...identity,
            });
            mutateSession(sid, (m) => ensureStreamTail(m, identity));
            invoke("resume_lead_session", {
              sessionId: sid,
              leadAgentId: leadId,
              memberIds: runtime.memberPoolIds,
            }).catch((e) => {
              setRun(sid, null);
              showLeadError(sid, String(e));
            });
          }
        }
      } catch (e) {
        if (String(e).startsWith("NO_PENDING_QUESTION")) {
          // 卡已经被答过（真双击 / 另一处已经把迟到答案落库）——后端只在 DB 里的卡确已
          // chosen 时才返回这个错误，落库早已一致，这里只是让前端内存跟上已知事实，
          // 不是「只改内存不落库」的旧坑（旧坑是这条分支以前对所有陈旧卡都不落库）。
          setDecisionStatusInMemory(sid, decisionId, "chosen", option);
        } else {
          // 真失败（非双击）：回滚 submitting → pending，让用户能重新点选，不卡死在灰态。
          setDecisionStatusInMemory(sid, decisionId, "pending");
          showLeadError(sid, String(e));
        }
      } finally {
        leadChoosingRef.current.delete(sid);
      }
      return;
    }
    const fromStatus = card.status; // "pending" | "failed"
    leadChoosingRef.current.add(sid);
    try {
      // b0 CAS fromStatus→submitting（赢 race 才继续·防双击；failed 重试同走 CAS·expect=failed）
      const won = await invoke<boolean>("choose_decision_card", {
        sessionId: sid,
        decisionId,
        expectStatus: fromStatus,
        nextStatus: "submitting",
        chosenOption: null,
      }).catch(() => false);
      if (!won) return; // 别人赢了 race / 状态已变·不重复执行
      setDecisionStatusInMemory(sid, decisionId, "submitting");

      // 任意 option 都当作用户回复喂回 lead，派 worker 由下一轮 lead_step 自己决定。
      // 有意 fire-and-forget：runLeadStepForSession 自带错误 UI；chosen 表示选择已记录并已交给 lead。
      runLeadStepForSession(sid, option);
      await invoke("choose_decision_card", {
        sessionId: sid,
        decisionId,
        expectStatus: "submitting",
        nextStatus: "chosen",
        chosenOption: option,
      }).catch(() => {});
      setDecisionStatusInMemory(sid, decisionId, "chosen", option);
    } finally {
      leadChoosingRef.current.delete(sid);
    }
  };

  const onLeadChoose = async (sid: string, option: string) => {
    if (guardReadonlySession(sid)) return;
    if (leadChoosingRef.current.has(sid)) return;
    leadChoosingRef.current.add(sid);
    const view = leadViewBySession.get(sid);
    setLeadView(sid, null);
    try {
      if (view) runLeadStepForSession(sid, option);
    } finally {
      leadChoosingRef.current.delete(sid);
    }
  };

  const onCodingShelve = async (runId: string) => {
    const s = codingLoopsRef.current.get(runId);
    if (!s) return;
    if (guardReadonlySession(s.sessionId)) return;
    const next = { ...s, phase: "shelved" as const };
    codingLoopsRef.current.set(runId, next);
    upsertCodingTaskBlock(runId, blockFromCodingState(runId, next));
    await driveCodingLoop(runId);
  };

  function gateCriteriaToDbRows(
    d: GateDraft,
    sid: string,
  ): AcceptanceCriterion[] {
    const now = Math.floor(Date.now() / 1000);
    return d.criteria.map((c, i) => ({
      id: `${d.runId}-${c.id}`,
      session_id: sid,
      run_id: d.runId,
      task_id: c.taskId,
      contract_id: d.contractId,
      scope: c.scope,
      claim: c.claim,
      verifier: c.verifier,
      evidence: null,
      status: "pending",
      waiver: null,
      created_at: now + i,
    }));
  }

  const onGateFreeze = () => {
    const sid = currentId;
    if (!sid) return;
    if (guardReadonlySession(sid)) return;
    const view = gateBySession.get(sid);
    if (view?.kind !== "draft") return;
    // 折入双审 P3：手动填态目标不能空（避免无意义空冻结）
    if (view.draft.manual && view.draft.goal.trim() === "") {
      setToast(t("app.gate.goalRequired"));
      return;
    }
    // F2b 前置守卫（派单前·冻结前）：未派齐则不冻结·卡留着让用户补派。
    const allAssigned =
      view.draft.assignments.length > 0 &&
      view.draft.assignments.every((a) => a.assignee !== null);
    if (!allAssigned) {
      setToast(t("app.gate.unassigned"));
      return;
    }
    // 守卫都过了·发起链之前 → 进 freezing 态（禁用主按钮防双击）
    setGateFreezing(true);
    const d = gateReducer(view.draft, { type: "freeze" });
    const assignmentsJson = JSON.stringify(
      d.assignments.map((a) => ({
        subtask_id: a.subtaskId,
        subtask: a.subtask,
        assignee: a.assignee
          ? {
              agent_id: a.assignee.agentId,
              provider: a.assignee.provider,
              model: a.assignee.model,
            }
          : null,
        scope_files: a.scopeFiles,
        // 折入 P1-1：用编辑后的 criteria 回填（按 taskId 匹配 subtask）·非 assignment 自带旧 acceptance
        acceptance: d.criteria
          .filter((c) => c.taskId === a.subtaskId)
          .map((c) => ({ claim: c.claim, verifier: c.verifier })),
      })),
    );
    const criteria = gateCriteriaToDbRows(d, sid);
    // 折入 #4：手动填 gate 的 contract DB 不存在 → 冻结前先 insert（保 freeze 纯 UPDATE）
    const ensure = d.manual
      ? invoke("insert_goal_contract_row", {
          contractId: d.contractId,
          sessionId: sid,
          runId: d.runId,
          goal: d.goal,
          leadId: agentId,
        })
      : Promise.resolve();
    ensure
      .then(() =>
        invoke("freeze_team_plan", {
          sessionId: sid,
          runId: d.runId,
          goal: d.goal,
          assignmentsJson,
          criteria,
        }),
      )
      // 折入 #6：冻结后从 DB 回读 → criteria id 一致 + DB 写入真有读者
      .then(() =>
        invoke<AcceptanceCriterion[]>("list_acceptance", {
          sessionId: sid,
          runId: d.runId,
        }),
      )
      .then((rows) => {
        setAcceptanceByRun((prev) => {
          const next = new Map(prev);
          next.set(d.runId, rows);
          return next;
        });
        const goal: GoalContract = {
          goal: d.goal,
          status: "frozen",
          criteria: rows.map((r) => ({
            id: r.id,
            claim: r.claim,
            verifier: r.verifier,
            evidence: r.evidence,
            status: r.status,
            scope: r.scope,
          })),
        };
        setFrozenGoalBySession((prev) => {
          const next = new Map(prev);
          next.set(sid, { runId: d.runId, goal });
          return next;
        });
        setGateBySession((prev) => {
          const v = prev.get(sid);
          // 折入 P1-2：stale freeze completion 不清掉用户后续发起的新 gate（runId 不同 = 已被取代）
          if (v?.kind !== "draft" || v.draft.runId !== d.runId) return prev;
          const next = new Map(prev);
          next.delete(sid);
          return next;
        });
        // F2b：冻结落库成功 → 复用派单路径真派队员（criteria 用 DB 回读 rows·死路消除）。
        startTeamRunFromDraft(sid, d, rows);
        setGateFreezing(false);
      })
      .catch((e) => {
        setGateFreezing(false);
        setToast(String(e));
      });
  };

  const rightPanelMax = rightPanelOpen && rightPanelExpanded;
  const runningSessionIds = useMemo(
    () => new Set(runningSessions.keys()),
    [runningSessions],
  );
  const sidebarActiveNamespace = useMemo(
    () => namespaces.find((n) => n.id === activeNamespaceId) ?? null,
    [activeNamespaceId, namespaces],
  );
  const sidebarHandlersRef = useRef({
    navigateHistory,
    onCreateGroup,
    onHandoverSession,
    onMoveSessionToGroup,
    onNewSession,
    onRenameGroup,
    onRenameSession,
    onRequestDeleteGroup,
    onSelectRepoInNamespace,
    onToggleArchive,
    onToggleGroup,
    onTogglePin,
    onToggleUnread,
    openSession,
  });
  sidebarHandlersRef.current = {
    navigateHistory,
    onCreateGroup,
    onHandoverSession,
    onMoveSessionToGroup,
    onNewSession,
    onRenameGroup,
    onRenameSession,
    onRequestDeleteGroup,
    onSelectRepoInNamespace,
    onToggleArchive,
    onToggleGroup,
    onTogglePin,
    onToggleUnread,
    openSession,
  };
  const handleSidebarToggleRepoGroup = useCallback((repoId: string) => {
    setRepoGroupExpanded((m) => ({ ...m, [repoId]: !m[repoId] }));
  }, []);
  const handleSidebarToggleGroup = useCallback((id: string) => {
    sidebarHandlersRef.current.onToggleGroup(id);
  }, []);
  const handleSidebarCreateGroup = useCallback((name: string) => {
    return sidebarHandlersRef.current.onCreateGroup(name);
  }, []);
  const handleSidebarMoveSessionToGroup = useCallback(
    (sessionId: string, groupId: string | null) => {
      return sidebarHandlersRef.current.onMoveSessionToGroup(
        sessionId,
        groupId,
      );
    },
    [],
  );
  const handleSidebarRenameGroup = useCallback((id: string, name: string) => {
    return sidebarHandlersRef.current.onRenameGroup(id, name);
  }, []);
  const handleSidebarRequestDeleteGroup = useCallback((group: GroupMeta) => {
    sidebarHandlersRef.current.onRequestDeleteGroup(group);
  }, []);
  const handleSidebarSelect = useCallback((id: string) => {
    setView("session");
    void sidebarHandlersRef.current.openSession(id);
  }, []);
  const handleSidebarNew = useCallback(() => {
    void sidebarHandlersRef.current.onNewSession();
  }, []);
  const handleSidebarRequestDelete = useCallback((session: Session) => {
    setDeleteTarget({ id: session.id, title: session.title });
  }, []);
  const handleSidebarRename = useCallback((id: string, title: string) => {
    return sidebarHandlersRef.current.onRenameSession(id, title);
  }, []);
  const handleSidebarTogglePin = useCallback((id: string, next: boolean) => {
    return sidebarHandlersRef.current.onTogglePin(id, next);
  }, []);
  const handleSidebarToggleUnread = useCallback((id: string, next: boolean) => {
    return sidebarHandlersRef.current.onToggleUnread(id, next);
  }, []);
  const handleSidebarToggleArchive = useCallback(
    (id: string, next: boolean) => {
      return sidebarHandlersRef.current.onToggleArchive(id, next);
    },
    [],
  );
  const handleSidebarHandover = useCallback((id: string) => {
    void sidebarHandlersRef.current.onHandoverSession(id);
  }, []);
  const handleSidebarMenuIntro = useCallback(() => setView("intro"), []);
  const handleMenuAgents = useCallback(
    () => openSettings("agents"),
    [openSettings],
  );
  const handleSidebarSelectRepo = useCallback(
    (namespaceId: string, repoId: string) => {
      void sidebarHandlersRef.current.onSelectRepoInNamespace(
        namespaceId,
        repoId,
      );
    },
    [],
  );
  const handleSidebarNewProject = useCallback(
    () => setNewProjectOpen(true),
    [],
  );
  const handleManageRepos = useCallback(
    () => openSettings("repos"),
    [openSettings],
  );
  const handleBack = useCallback(
    () => sidebarHandlersRef.current.navigateHistory(-1),
    [],
  );
  const handleForward = useCallback(
    () => sidebarHandlersRef.current.navigateHistory(1),
    [],
  );
  const handleToggleSidebar = useCallback(
    () => setSidebarOpen((value) => !value),
    [],
  );
  const handleHome = useCallback(() => setView("overview"), []);
  const sessionMainHandlersRef = useRef({
    cancelContinuationDraft,
    dismissInterruptedRun,
    generateContinuationDraft,
    handleOpenMember,
    onCodingConfirmVerify,
    onCodingRetryVerify,
    onCodingShelve,
    onDecisionChoose,
    onGateAction,
    onGateBackToNormal,
    onGateFreeze,
    onGateManual,
    onGateRedraft,
    onGateRetry,
    onLeadChoose,
    onSend,
    onStartContinuation,
    onStop,
    onViewRun,
    openInspector,
    setComposerLeadId,
    startTeamRunForSession,
    toggleComposerRoster,
  });
  sessionMainHandlersRef.current = {
    cancelContinuationDraft,
    dismissInterruptedRun,
    generateContinuationDraft,
    handleOpenMember,
    onCodingConfirmVerify,
    onCodingRetryVerify,
    onCodingShelve,
    onDecisionChoose,
    onGateAction,
    onGateBackToNormal,
    onGateFreeze,
    onGateManual,
    onGateRedraft,
    onGateRetry,
    onLeadChoose,
    onSend,
    onStartContinuation,
    onStop,
    onViewRun,
    openInspector,
    setComposerLeadId,
    startTeamRunForSession,
    toggleComposerRoster,
  };
  const handleSessionSend = useCallback(
    (text: string, nextMode: Mode, config?: ComposerRuntimeConfig) =>
      sessionMainHandlersRef.current.onSend(text, nextMode, config),
    [],
  );
  const handleSessionStop = useCallback(
    () => sessionMainHandlersRef.current.onStop(),
    [],
  );
  const handleSessionViewRun = useCallback(
    (runId?: string) => sessionMainHandlersRef.current.onViewRun(runId),
    [],
  );
  const handleSessionOpenMember = useCallback(
    (runId: string, assignmentId: string) =>
      sessionMainHandlersRef.current.handleOpenMember(runId, assignmentId),
    [],
  );
  const handleSessionOpenInspector = useCallback(
    (assignmentId: string) =>
      sessionMainHandlersRef.current.openInspector(assignmentId),
    [],
  );
  const handleSessionGateAction = useCallback(
    (action: GateAction) => sessionMainHandlersRef.current.onGateAction(action),
    [],
  );
  const handleSessionGateFreeze = useCallback(
    () => sessionMainHandlersRef.current.onGateFreeze(),
    [],
  );
  const handleSessionGateRedraft = useCallback(
    () => sessionMainHandlersRef.current.onGateRedraft(),
    [],
  );
  const handleSessionGateRetry = useCallback(
    () => sessionMainHandlersRef.current.onGateRetry(),
    [],
  );
  const handleSessionGateManual = useCallback(
    () => sessionMainHandlersRef.current.onGateManual(),
    [],
  );
  const handleSessionGateBackToNormal = useCallback(
    () => sessionMainHandlersRef.current.onGateBackToNormal(),
    [],
  );
  const handleSessionConfirmVerify = useCallback(
    (runId: string, command: string) =>
      sessionMainHandlersRef.current.onCodingConfirmVerify(runId, command),
    [],
  );
  const handleSessionRetryVerify = useCallback(
    (runId: string) =>
      sessionMainHandlersRef.current.onCodingRetryVerify(runId),
    [],
  );
  const handleSessionShelve = useCallback(
    (runId: string) => sessionMainHandlersRef.current.onCodingShelve(runId),
    [],
  );
  const handleSessionTakeOver = useCallback(() => setMode("normal"), []);
  const handleSessionCleanRedispatch = useCallback((runId: string) => {
    const sid = currentIdRef.current;
    if (!sid) return;
    const sessionMessages = messagesRef.current.get(sid) ?? [];
    let goalText: string | null = null;
    for (const message of sessionMessages) {
      for (const block of message.content) {
        if (block.type === "team_run" && block.run_id === runId) {
          goalText = block.goal?.goal ?? null;
        }
      }
    }
    if (goalText) {
      sessionMainHandlersRef.current.startTeamRunForSession(sid, goalText);
    }
  }, []);
  const handleSessionLeadChoose = useCallback((option: string) => {
    const sid = currentIdRef.current;
    if (sid) void sessionMainHandlersRef.current.onLeadChoose(sid, option);
  }, []);
  const handleSessionDecisionChoose = useCallback(
    (decisionId: string, option: string) => {
      const sid = currentIdRef.current;
      if (sid) {
        void sessionMainHandlersRef.current.onDecisionChoose(
          sid,
          decisionId,
          option,
        );
      }
    },
    [],
  );
  const handleSessionRetryContinuation = useCallback(() => {
    const sid = currentIdRef.current;
    if (sid) {
      void sessionMainHandlersRef.current.generateContinuationDraft(sid, true);
    }
  }, []);
  const handleSessionCancelContinuation = useCallback(() => {
    const sid = currentIdRef.current;
    if (sid) sessionMainHandlersRef.current.cancelContinuationDraft(sid);
  }, []);
  const handleSessionStartContinuation = useCallback(
    (payload: Parameters<typeof onStartContinuation>[0]) =>
      sessionMainHandlersRef.current.onStartContinuation(payload),
    [],
  );
  const handleSessionSetLead = useCallback(
    (id: string | null, memberIds?: string[]) =>
      sessionMainHandlersRef.current.setComposerLeadId(id, memberIds),
    [],
  );
  const handleSessionToggleRoster = useCallback(
    (id: string, allEnabledIds: string[]) =>
      sessionMainHandlersRef.current.toggleComposerRoster(id, allEnabledIds),
    [],
  );
  const handleSessionRedispatchInterrupted = useCallback(
    (runId: string, goal: string | null) => {
      const sid = currentIdRef.current;
      if (sid && goal) {
        sessionMainHandlersRef.current.startTeamRunForSession(sid, goal);
      }
      sessionMainHandlersRef.current.dismissInterruptedRun(runId);
    },
    [],
  );
  const handleSessionDismissInterrupted = useCallback((runId: string) => {
    sessionMainHandlersRef.current.dismissInterruptedRun(runId);
  }, []);
  const interruptedBanner = useMemo(
    () =>
      currentInterruptedRuns.length > 0 ? (
        <div className="interrupt-banner">
          {currentInterruptedRuns.map((run) => (
            <div className="interrupt-banner__row" key={run.run_id}>
              <span className="interrupt-banner__label">
                {t("app.interrupt.label")}
              </span>
              {run.goal && (
                <span className="interrupt-banner__goal">{run.goal}</span>
              )}
              <div className="interrupt-banner__actions">
                <button
                  type="button"
                  className="interrupt-banner__redispatch"
                  onClick={() =>
                    handleSessionRedispatchInterrupted(run.run_id, run.goal)
                  }
                >
                  {t("app.interrupt.redispatch")}
                </button>
                <button
                  type="button"
                  className="interrupt-banner__dismiss"
                  onClick={() => handleSessionDismissInterrupted(run.run_id)}
                >
                  {t("app.interrupt.dismiss")}
                </button>
              </div>
            </div>
          ))}
        </div>
      ) : undefined,
    [
      currentInterruptedRuns,
      handleSessionDismissInterrupted,
      handleSessionRedispatchInterrupted,
      t,
    ],
  );
  const showInstallGuide =
    !settingsOpen &&
    shouldShowInstallGuide({
      agentsReady,
      runtimeDetectResolved: runtimeDetect !== undefined,
      availableAgentsCount: availableAgents.length,
      dismissed: installGuideDismissed,
    });

  return (
    <div className="app-shell">
      <div
        className="shell-bg"
        inert={
          showInstallGuide ||
          settingsOpen ||
          newProjectOpen ||
          editingRepo !== null
        }
      >
        {sidebarOpen && (
          <Sidebar
            sessions={sessions}
            currentId={currentId}
            busy={busy}
            runningSessionIds={runningSessionIds}
            sessionStatusById={sessionStatusById}
            continuationReadySessionIds={continuationReadySessionIds}
            activeMenu={view === "intro" ? "intro" : "session"}
            settingsActive={settingsOpen}
            activeNamespace={sidebarActiveNamespace}
            activeRepo={activeRepoMeta}
            namespaces={namespaces}
            allRepos={allRepos}
            activeRepoId={activeRepoId}
            activeNamespaceId={activeNamespaceId}
            reposInActiveNs={reposInActiveNs}
            repoGroupExpanded={repoGroupExpanded}
            onToggleRepoGroup={handleSidebarToggleRepoGroup}
            newDisabled={activeRepoId === null}
            groups={groups}
            groupExpanded={groupExpanded}
            onToggleGroup={handleSidebarToggleGroup}
            onCreateGroup={handleSidebarCreateGroup}
            onMoveSessionToGroup={handleSidebarMoveSessionToGroup}
            onRenameGroup={handleSidebarRenameGroup}
            onRequestDeleteGroup={handleSidebarRequestDeleteGroup}
            onSelect={handleSidebarSelect}
            onNew={handleSidebarNew}
            onRequestDelete={handleSidebarRequestDelete}
            onRename={handleSidebarRename}
            onTogglePin={handleSidebarTogglePin}
            onToggleUnread={handleSidebarToggleUnread}
            onToggleArchive={handleSidebarToggleArchive}
            onHandover={handleSidebarHandover}
            handoverAssemblingId={continuationAssemblingId}
            onMenuIntro={handleSidebarMenuIntro}
            onMenuAgents={handleMenuAgents}
            onSelectRepoInNamespace={handleSidebarSelectRepo}
            onNewProject={handleSidebarNewProject}
            onEditRepo={setEditingRepo}
            onManageRepos={handleManageRepos}
            canGoBack={navState.canGoBack}
            canGoForward={navState.canGoForward}
            onBack={handleBack}
            onForward={handleForward}
            onToggleSidebar={handleToggleSidebar}
            onHome={handleHome}
          />
        )}
        <div
          className={`surface${sidebarOpen ? "" : " full"}${rightPanelOpen ? " rpopen" : ""}`}
        >
          <SurfaceHeader
            view={view}
            sidebarCollapsed={!sidebarOpen}
            onToggleSidebar={handleToggleSidebar}
            onHome={handleHome}
            canGoBack={navState.canGoBack}
            canGoForward={navState.canGoForward}
            onBack={handleBack}
            onForward={handleForward}
            sessionTitle={
              currentSession?.title ?? t("app.header.sessionFallback")
            }
            repoName={activeRepoMeta?.name}
            status={busy ? "working" : "idle"}
            contextLabel={
              view === "overview"
                ? t("app.header.overviewContext", {
                    name:
                      namespaces.find((n) => n.id === activeNamespaceId)
                        ?.name ?? "Local",
                  })
                : view === "intro"
                  ? t("app.header.introContext", {
                      name: activeRepoMeta?.name ?? "",
                    })
                  : ""
            }
            rightPanelOpen={rightPanelOpen}
            rightPanelTab={rightPanelTab}
            previewPath={previewPath}
            tabBeforePreview={tabBeforePreviewRef.current}
            rightPanelExpanded={rightPanelMax}
            reviewBadge={
              view === "session"
                ? currentSessionIsLocal
                  ? 0
                  : review?.has_changes
                    ? review.files_changed
                    : 0
                : 0
            }
            onTab={handleSelectTab}
            onExpand={openRightPanelHome}
            onUserCollapse={() => {
              setInspectorTarget(null);
              setShowTaskList(false);
              setDrillRun(null);
              setRightPanelOpen(false);
              setRightPanelExpanded(false);
            }}
            onExpandPanel={() => setRightPanelExpanded(true)}
            onRestorePanel={() => setRightPanelExpanded(false)}
            goal={view === "session" ? currentDisplayGoal : null}
            goalExpanded={goalExpanded}
            onToggleGoal={() => setGoalExpanded((v) => !v)}
            goalPanel={goalPanel}
            goalRunComplete={goalRunComplete}
            goalRunHasMemberFailure={goalRunHasMemberFailure}
            goalRunning={goalRunActive}
            orchestratedTaskCount={isOrchestratedRun ? goalMembers.length : 0}
            orchestratedAnyRunning={isOrchestratedRun && memberRunning}
            onOpenTaskList={toggleTaskList}
          />
          <div className="sf-body">
            <div className={`session-pane${rightPanelMax ? " hidden" : ""}`}>
              {view === "overview" ? (
                <OverviewHome
                  sessions={activeSessions}
                  repos={allRepos}
                  runningSessionIds={runningSessionIds}
                  onOpen={(id) => {
                    setView("session");
                    openSession(id);
                  }}
                  onSelectRepo={handleSidebarSelectRepo}
                />
              ) : view === "intro" ? (
                <ProjectIntroPage
                  activeRepo={activeRepoMeta}
                  composerBusy={activeRepoId === null || composerBusy}
                  running={false}
                  agents={availableAgents}
                  agentId={agentId}
                  canSend={
                    availableAgents.some((a) => a.id === agentId) &&
                    !teamConfigBlocked
                  }
                  onAgentChange={handleUserSelectAgent}
                  onMenuAgents={handleMenuAgents}
                  mode={mode}
                  onModeChange={setMode}
                  onSend={createSessionAndSend}
                  onStop={onStop}
                  teamLeadId={composerTeamCfg.leadId}
                  rosterIds={composerTeamCfg.rosterIds}
                  onSetLead={setComposerLeadId}
                  onToggleRoster={toggleComposerRoster}
                />
              ) : (
                <SessionMain
                  messages={displayMessages}
                  busy={busy}
                  composerBusy={composerBusy}
                  memberRunning={memberRunning}
                  runStartedAt={currentRun?.startedAt ?? null}
                  workingTokens={workingTokens}
                  agents={availableAgents}
                  agentId={agentId}
                  canSend={
                    sendGate.canSend &&
                    !teamConfigBlocked &&
                    !currentSessionReadonlyReason
                  }
                  readonlyReason={currentSessionReadonlyReason}
                  loading={sendGate.pending}
                  teamSaving={teamConfigPending}
                  done={done}
                  sessionUsage={sessionUsage}
                  sessionId={currentId}
                  onAgentChange={handleUserSelectAgent}
                  onMenuAgents={handleMenuAgents}
                  mode={mode}
                  onModeChange={setMode}
                  onSend={handleSessionSend}
                  onMemberIdle={clearStaleMemberCards}
                  onStop={handleSessionStop}
                  onViewRun={handleSessionViewRun}
                  onUndoRun={onUndoRun}
                  onOpenPreview={openPreview}
                  onOpenLightbox={openLightbox}
                  onOpenMember={handleSessionOpenMember}
                  onOpenInspector={handleSessionOpenInspector}
                  gateView={composerTeamActive ? null : gateView}
                  leadName={agentNameSnapshotFor(agentId) ?? agentId}
                  enabledAgents={availableAgents}
                  onGateAction={handleSessionGateAction}
                  onGateFreeze={handleSessionGateFreeze}
                  onGateRedraft={handleSessionGateRedraft}
                  onGateRetry={handleSessionGateRetry}
                  onGateManual={handleSessionGateManual}
                  onGateBackToNormal={handleSessionGateBackToNormal}
                  onConfirmVerify={handleSessionConfirmVerify}
                  onRetryVerify={handleSessionRetryVerify}
                  onShelve={handleSessionShelve}
                  gateFreezing={gateFreezing}
                  onTakeOver={handleSessionTakeOver}
                  onCleanRedispatch={handleSessionCleanRedispatch}
                  interruptedBanner={interruptedBanner}
                  leadView={leadViewBySession.get(currentId ?? "") ?? null}
                  onLeadChoose={handleSessionLeadChoose}
                  onDecisionChoose={handleSessionDecisionChoose}
                  pendingDecision={pendingDecision}
                  liveRunsByRun={liveRunsByRun}
                  liveCodingByRun={liveCodingByRun}
                  continuationParentId={
                    continuationParentId === currentId ? currentId : null
                  }
                  continuationParentTitle={
                    sessions.find((s) => s.id === currentId)?.title
                  }
                  continuationDraftState={
                    currentId ? continuationDrafts.get(currentId) : undefined
                  }
                  continuationStarting={continuationStarting}
                  onRetryContinuation={handleSessionRetryContinuation}
                  onCancelContinuation={handleSessionCancelContinuation}
                  onStartContinuation={handleSessionStartContinuation}
                  teamLeadId={composerTeamCfg.leadId}
                  rosterIds={composerTeamCfg.rosterIds}
                  onSetLead={handleSessionSetLead}
                  onToggleRoster={handleSessionToggleRoster}
                />
              )}
            </div>
            {rightPanelOpen && (
              <>
                <div className="sf-div" />
                <div className={`tools-pane${rightPanelMax ? " full" : ""}`}>
                  <RightPanel
                    open={rightPanelOpen}
                    tab={rightPanelTab}
                    review={view === "session" ? review : null}
                    reviewContext={view === "session" ? "session" : "none"}
                    sessionId={view === "session" ? currentId : null}
                    repoId={
                      view === "session"
                        ? (currentSession?.repo_id ?? null)
                        : activeRepoId
                    }
                    repoName={
                      view === "session"
                        ? currentSessionRepoName
                        : activeRepoMeta?.name
                    }
                    previewPath={previewPath}
                    previewSessionId={previewSessionId}
                    onClosePreview={closePreview}
                    onTab={handleSelectTab}
                    drill={drill}
                    inspectorMember={inspectorMember}
                    onCloseInspector={() => {
                      setInspectorTarget(null);
                      setRightPanelOpen(false);
                    }}
                    showTaskList={showTaskList}
                    taskListWorkers={isOrchestratedRun ? goalMembers : []}
                    onSelectTask={openInspector}
                    onStopTask={(assignmentId) => {
                      const runId = runIdByAssignment(messages, assignmentId);
                      if (runId) handleStopMember(runId, assignmentId);
                    }}
                    onBackToList={() => {
                      setInspectorTarget(null);
                      setShowTaskList(true);
                    }}
                    undoTarget={
                      undoTarget && undoTarget.sessionId === currentId
                        ? {
                            ...undoTarget,
                            result:
                              undoFeedback.get(
                                undoFeedbackKey(
                                  undoTarget.sessionId,
                                  undoTarget.runId,
                                ),
                              ) ?? null,
                          }
                        : null
                    }
                    onExitUndo={handleExitUndo}
                    onUndoComplete={handleUndoComplete}
                  />
                </div>
              </>
            )}
          </div>
        </div>
      </div>
      {showInstallGuide && (
        <AgentInstallGuideDialog
          onClose={() => setInstallGuideDismissed(true)}
          onOpenSettings={() => openSettings("agents")}
        />
      )}
      {lightbox && (
        <Lightbox
          key={`${lightbox.sessionId ?? ""}::${lightbox.path}`}
          path={lightbox.path}
          sessionId={lightbox.sessionId}
          onClose={closeLightbox}
        />
      )}
      <NewProjectSheet
        open={newProjectOpen}
        onClose={() => setNewProjectOpen(false)}
        onCreate={handleCreateProject}
      />
      <NewProjectSheet
        mode="edit"
        open={editingRepo !== null}
        initial={
          editingRepo
            ? { name: editingRepo.name, icon: editingRepo.icon ?? null }
            : undefined
        }
        onSave={handleEditProject}
        onRemove={
          editingRepo?.id === DEFAULT_LOCAL_PROJECT_ID
            ? undefined
            : () => {
                if (editingRepo) {
                  setRemoveProjectTarget({
                    id: editingRepo.id,
                    name: editingRepo.name,
                  });
                  setEditingRepo(null);
                }
              }
        }
        onClose={() => setEditingRepo(null)}
      />
      <SettingsSheet
        open={settingsOpen}
        page={settingsPage}
        onPageChange={setSettingsPage}
        onClose={() => setSettingsOpen(false)}
        agentsContent={
          <SettingsAgents
            onAgentsChanged={refetchAgents}
            runtimeDetect={runtimeDetect}
          />
        }
        searchContent={<SettingsSearch />}
        languageContent={<SettingsLanguage />}
        archivedProjectsContent={
          <ArchivedProjectsPanel onArchivedChanged={handleArchivedChanged} />
        }
        reposContent={
          <RepoManagePanel
            accounts={repoManageAccounts}
            selectedLogin={selectedGhLogin}
            onSelectAccount={onSelectRepoAccount}
            onConnectAccount={refreshGhAccounts}
            onConnectLocal={onConnectGithub}
            connectError={connectError}
            gate={ghGate}
            onInstallGh={onInstallGh}
            onRefreshAccounts={refreshGhAccounts}
            view={repoListView}
            onRetryList={onRetryList}
            search={search}
            onSearchChange={setSearch}
            filter={filter}
            onFilterChange={setFilter}
            selected={currentSelectedRepoKeys}
            onToggleSelect={onToggleSelectedRepo}
            baseFolderLabel="~/code/"
            onClone={onCloneSelectedRepos}
            cloneProgress={cloneProgress}
            onRetry={onRetryClone}
            onRetryFailed={onRetryFailedClones}
            onOpenSession={onOpenClonedSession}
          />
        }
      />
      {invalidDialog && (
        <InvalidProjectDialog
          state={invalidDialog}
          onResolved={async (action) => {
            setInvalidDialog(null);
            const ctx = await invoke<AppContext>("app_context").catch(
              () => null as AppContext | null,
            );
            setReposInActiveNs(ctx?.repos ?? []);
            setActiveRepoId(null);
            setView("session");
            if (action === "archived") setToast(t("app.project.archived"));
            else if (action === "restored") setToast(t("app.project.restored"));
            else setToast(t("app.project.switchedDefault"));
          }}
          onClose={() => setInvalidDialog(null)}
        />
      )}
      <ConfirmDialog
        open={deleteTarget !== null}
        title={t("app.session.deleteTitle", {
          title: deleteTarget?.title ?? "",
        })}
        body={deleteSessionConfirmBody(deleteTarget)}
        confirmLabel={t("app.dialog.delete")}
        onConfirm={async () => {
          const target = deleteTarget;
          setDeleteTarget(null);
          if (target) {
            try {
              await onDeleteSession(target.id);
            } catch (e) {
              setToast(renderBackendError(String(e), t));
            }
          }
        }}
        onCancel={() => setDeleteTarget(null)}
      />
      <ConfirmDialog
        open={groupDeleteTarget !== null}
        title={t("app.group.deleteTitle", {
          name: groupDeleteTarget?.name ?? "",
        })}
        body={t("app.group.deleteBody")}
        confirmLabel={t("app.dialog.delete")}
        onConfirm={async () => {
          const t = groupDeleteTarget;
          setGroupDeleteTarget(null);
          if (t) await onDeleteGroup(t.id);
        }}
        onCancel={() => setGroupDeleteTarget(null)}
      />
      <ConfirmDialog
        open={removeProjectTarget !== null}
        title={t("removeProject.confirm.title")}
        body={
          removeProjectTarget
            ? removeProjectConfirmBody(removeProjectTarget)
            : undefined
        }
        confirmLabel={t("removeProject.confirm.confirm")}
        cancelLabel={t("removeProject.confirm.cancel")}
        tone="danger"
        onConfirm={() => {
          const target = removeProjectTarget;
          setRemoveProjectTarget(null);
          if (target) void handleRemoveProject(target.id);
        }}
        onCancel={() => setRemoveProjectTarget(null)}
      />
      <AboutDialog open={aboutOpen} onClose={() => setAboutOpen(false)} />
      {toast && (
        <div
          className="toast"
          role="status"
          onAnimationEnd={() => setToast(null)}
        >
          {toast}
        </div>
      )}
    </div>
  );
}

function App() {
  return (
    <RepoDocumentProvider>
      <AppContent />
    </RepoDocumentProvider>
  );
}

export default App;

import type { UndoResultRecord } from "./undo";

export type AgentEvent =
  | { kind: "session_started"; conversation_id: string }
  | { kind: "text_delta"; text: string }
  | {
      kind: "tool_started";
      id: string;
      tool: string;
      summary: string;
      card: "command" | "compact";
    }
  | {
      kind: "tool_completed";
      id: string;
      status: "ok" | "failed";
      exit_code: number | null;
      output: string | null;
    }
  | {
      kind: "approval_requested";
      approval_id: string;
      run_id: string;
      tool: string;
      command: string;
      summary: string;
      cwd: string;
      request_kind?: string | null;
      proposal_id?: string | null;
    }
  | {
      kind: "approval_resolved";
      approval_id: string;
      decision: string;
      reason?: string | null;
    }
  | { kind: "thinking_delta"; text: string }
  | {
      kind: "completed";
      cost_usd: number | null;
      input_tokens: number | null;
      output_tokens: number | null;
      final_text: string | null;
      // plan B3：B1 后端已 emit 的 commit 结构化字段（空轮全 null）
      run_id?: string | null;
      commit_sha?: string | null;
      files_changed?: number | null;
      insertions?: number | null;
      deletions?: number | null;
      interrupted?: boolean | null;
      // §4.1：后端 Completed.result 已发·终态结构化结果（空轮/Normal 单线为 null）
      result?: MemberResult | null;
    }
  | {
      kind: "run_closeout";
      run_id: string;
      commit_sha: string | null;
      files_changed: number | null;
      insertions: number | null;
      deletions: number | null;
      interrupted: boolean | null;
    }
  | { kind: "error"; message: string }
  | { kind: "blocked"; message: string }
  | {
      kind: "goal_declared";
      goal: string;
      status: "draft" | "frozen";
      lead: string | null;
      criteria: Criterion[];
    }
  | {
      kind: "criteria_updated";
      criteria: {
        id: string;
        status: Criterion["status"];
        evidence: string | null;
      }[];
    }
  | {
      kind: "goal_updated";
      criteria: Criterion[];
    }
  | {
      kind: "needs_decision";
      run_id: string;
      reason: string;
      changes: ScopeChangeItem[];
    };

export type StatusTransition =
  | "dispatched"
  | "needs_input"
  | "done"
  | "failed"
  | "stopped"
  | "reassigned";

/** 缝1 派单维度（Normal 单线时整体缺省）。run_id 是派单 run，与 completed.run_id（git run）两层。 */
export type DispatchMeta = {
  run_id?: string;
  task_id?: string;
  assignment_id?: string;
  segment_id?: string;
  origin_participant_id?: string;
  parent_event_id?: string;
  status_transition?: StatusTransition;
  /** #3：开场派单事件携带的 TaskPack 冷 brief 全文（后端 DispatchMeta.task_pack·drill 查看派单 brief 用）。 */
  task_pack?: string;
  /** lead-session 编排派的 worker 标记，前端据此跳过旧 team-run 收尾全套。 */
  orchestrated?: boolean;
  /** 队员名快照·派单事件携带 */
  member_name?: string;
};

/** R1：dispatch 是**嵌套**字段（与后端 envelope 镜像），不再把派单字段铺平到顶层。 */
export type AgentEventEnvelope = {
  session_id: string;
  dispatch?: DispatchMeta;
} & AgentEvent;

// failed（§三.3）：零成本字面量，免 M3 改类型牵动徽章/reducer/isTeamRunComplete 全链路
export type ParticipantStatus =
  | "running"
  | "needs_input"
  | "done"
  | "failed"
  | "stopped";

/** 一条验收标准（与后端 GoalCriterion 镜像）。 */
export type Criterion = {
  id: string;
  claim: string;
  verifier?: string | null;
  evidence?: string | null;
  status: "pending" | "passed" | "failed" | "waived" | "uncertain";
  scope: "run" | "task";
};

export type ScopeChangeItem = {
  proposal_id: string;
  kind: string;
  detail_text: string;
  detail_summary: string | null;
};

/** run 级目标契约（与后端 TeamGoal 镜像）。M1a status 恒 frozen。 */
export type GoalContract = {
  goal: string;
  status: "draft" | "frozen";
  criteria: Criterion[];
  goal_title?: string;
};

/** 一个队员在一次派单 run 里的运行单元（按 assignment 唯一）。 */
export type MemberUnit = {
  participant_id: string;
  assignment_id: string;
  task_id: string;
  name: string;
  status: ParticipantStatus;
  /** 子任务一句话（取自开场文本） */
  sub: string;
  /** 进度 = derived（非 narrated）：见过的 tool 数 / 完成的 tool 数 */
  steps_total: number;
  steps_done: number;
  /** §三.4：token/cost 由 completed 累加（drill 头部 + 目标浮层底显） */
  cost_usd: number | null;
  input_tokens: number;
  output_tokens: number;
  /** §二.4：失败态（队员卡变红 + M3 改派入口预留） */
  failed: boolean;
  /** 该队员的事件 reduce 出的块（右面板 drill-in 渲染用） */
  blocks: Block[];
  /** §4.1：运行时合成的结构化 MemberResult（终态填·老快照无此字段不破）。 */
  result?: MemberResult;
  /** #3：队长派给该队员的 TaskPack 全文（取自开场派单事件 dispatch.task_pack·drill 折叠展示）。 */
  taskPack?: string;
  /** 该 member 首次创建（派单）时间·epoch ms·TaskList 显相对时间用 */
  started_at?: number;
};

export type TeamRun = {
  run_id: string;
  /** 方案 A：goal_declared 事件收进来；持久化时顺带进 team_run Block（reload 复活） */
  goal: GoalContract | null;
  lead: string | null;
  members: MemberUnit[];
};

export type ReasoningTier =
  | "auto"
  | "none"
  | "minimal"
  | "low"
  | "medium"
  | "high"
  | "xhigh"
  | "max";

export type ComposerRuntimeConfig = {
  reasoningTier?: ReasoningTier;
};

export interface AgentProfile {
  id: string;
  name: string;
  access: string;
  provider: string;
  primary_model: string | null;
  endpoint: string | null;
  auth_mode: string | null;
  model_opus: string | null;
  model_sonnet: string | null;
  model_haiku: string | null;
  model_subagent: string | null;
  reasoning_default: string;
  max_output_tokens: number | null;
  api_timeout_ms: number | null;
  compat_disable_betas: boolean;
  compat_disable_nonessential: boolean;
  compat_disable_thinking: boolean;
  compat_proxy: string | null;
  custom_headers: string | null;
  extra_body: string | null;
  cap_reasoning: string | null;
  cap_computer_use: string | null;
  cap_lead: string | null;
  has_key: boolean;
  is_builtin: boolean;
  enabled: boolean;
  sort_order: number;
  created_at: number;
  updated_at: number;
}

export type ConnectionTestResult = {
  ok: boolean;
  category: string | null;
  raw_error: string | null;
};

export type SummaryStatus = {
  kind: "all_succeeded" | "partial" | "failed";
  succeeded_count: number;
  total: number;
};
export type Finding = {
  status: "done" | "miss";
  text: string;
  assignment_id: string;
};
export type TraceRef = { run_id: string; assignment_ids: string[] };
export type SourceLoc = {
  run_id: string;
  assignment_id: string;
  block_index: number;
};
export type SourceSpan = {
  ref_no: number;
  text_span: [number, number];
  sources: SourceLoc[];
  conflict: boolean;
};
export type SummarySection = {
  heading: string;
  body_richtext?: string | null;
  findings?: Finding[];
  attribution: string[];
  trace_ref: TraceRef;
  source_spans?: SourceSpan[];
};
export type ArtifactRef = {
  kind: "code_diff" | "file" | "doc" | "pr" | "deploy";
  label: string;
};
export type ChangedFile = {
  path: string;
  insertions: number;
  deletions: number;
};
export type ResultAnchor = {
  base_sha: string;
  head_sha?: string | null;
  diff_ref?: string | null;
  generated_from: string;
};
export type CommandEvidence = {
  cmd: string;
  exit_code?: number | null;
  status: string;
  source_provider: string;
  output_ref?: string | null;
};
export type RiskInputs = {
  files_changed: number;
  cmd_danger: string;
  reversibility: string;
};
export type Decision = {
  id: string;
  text: string;
  source_refs?: SourceLoc[];
  supersedes?: string[];
  confidence?: string | null;
  source_kind?: string | null;
};
export type Risk = {
  id: string;
  text: string;
  source_refs?: SourceLoc[];
  confidence?: string | null;
  source_kind?: string | null;
};
/** 镜像后端 agent_event.rs::MemberResult（spec §4.1·软字段全 optional）。 */
export type MemberResult = {
  schema_version: number;
  assignment_id: string;
  participant_id: string;
  status: string;
  failure_reason?: string | null;
  changed_files: ChangedFile[];
  anchor: ResultAnchor;
  command_evidence: CommandEvidence[];
  risk_inputs: RiskInputs;
  decisions?: Decision[];
  risks?: Risk[];
  final_text_ref?: string | null;
  artifact_refs?: ArtifactRef[];
  result_source: string;
  /** P1（member 失败原因透出）：进程真退出码——诊断素材，非契约判定用。旧快照无此字段→undefined。 */
  exit_code?: number | null;
  /** stderr 尾部（后端截断 4096B）；空则不给字段。只在 Failed/Stopped 时才有值。 */
  stderr_tail?: string | null;
  /**
   * P1-2（opus 对抗审·判据结构化）：机器可判的失败大类——"stalled"（见过 harness 的
   * Blocked/NeedsDecision 事件·契约退出码 3/4，且不是下面 budget_exhausted/
   * context_exhausted 两条特例）、"budget_exhausted"（**轮次**预算用完但一直在正常推进，
   * 不是卡住/等回答，别跟 stalled 混为一谈）、"context_exhausted"（本刀新增：单轮**上下文
   * （token）**预算耗尽，判定发生在模型这一轮被调用之前——没有「一直在正常推进」的证据，
   * 也不建议原样重派，跟按轮次算的 budget_exhausted 不是同一件事）或 "env"（真进程/环境
   * 故障）。只由后端按真实标志写，前端应该读这个字段做分类，不该再对 failure_reason 文本
   * 做正则嗅探——那条文本可能是 agent stdout/stderr 的原样透传，agent 可以在里面抄一句话
   * 冒充诚实停摆。
   */
  failure_kind?:
    | "stalled"
    | "budget_exhausted"
    | "context_exhausted"
    | "env"
    | null;
};
/** 镜像后端 db::TeamRunPendingRow（list_interrupted_team_runs 返回·崩溃中断 run·reload 渲中断条用）。 */
export type TeamRunPendingRow = {
  session_id: string;
  run_id: string;
  goal: string | null;
  lead_participant_id: string | null;
  assignments_json: string;
};
export type LeadSummaryBlock = {
  type: "lead_summary";
  run_id: string;
  summary_source:
    | "lead_synthesis"
    | "single_passthrough"
    | "fallback_raw"
    | "pending";
  status: SummaryStatus;
  sections: SummarySection[];
  findings: Finding[];
  artifact_refs: ArtifactRef[];
};
/** 镜像后端 db::AcceptanceCriterion（含 waiver·B5：reason 在 DB·Criterion 无此字段）。 */
export type AcceptanceCriterion = {
  id: string;
  session_id: string;
  run_id: string;
  task_id: string;
  contract_id: string | null;
  scope: "run" | "task";
  claim: string;
  verifier: string | null;
  evidence: string | null;
  status: "pending" | "passed" | "failed" | "waived";
  waiver: string | null;
  created_at: number;
};
export type CriterionTrust = {
  tier: "command_trace" | "self_report" | "unverified";
  degraded: boolean;
  label: string;
};

/** coding 闭环阶段（半自动串联·Plan 6 刀1）。 */
export type CodingPhase =
  | "finalizing" // 固化 artifact
  | "ask_verify" // askQ①：确认验证命令
  | "verifying" // 跑 run_verifier
  | "verify_failed" // L1 没过 → askQ（重试/改验收/先放着·v1 仅展示+先放着）
  | "ask_apply" // legacy b2a 前旧状态；新流程不再产生
  | "merging" // 合进 staging
  | "applying" // ff-only 应用到当前分支
  | "applied" // 落地完成
  | "landing_blocked" // 安全 preflight / L1 证据 / ff 落地被挡
  | "shelved" // 用户选「先放着」
  | "error"; // 任一步出错（诚实展示）

/** coding 闭环 Block（进会话流·渲染正在执行任务条 / askQ 卡）。 */
export type CodingTaskBlock = {
  type: "coding_task";
  run_id: string;
  assignment_id: string;
  worker_name: string;
  phase: CodingPhase;
  /** 当前 worker 第几步 / 共几步（复用 MemberUnit.steps_done/total·展示进度）。 */
  step_done?: number;
  step_total?: number;
  /** finalize 产出。 */
  artifact_id?: string | null;
  /** askQ① 推荐的验证命令（lead 提议·来自验收 verifier 或默认·用户可改）。 */
  verify_cmd?: string | null;
  /** 末态/错误的人读信息。 */
  detail?: string | null;
  /** lead 决策理由（可审计·折叠小行·原型屏②）。 */
  lead_rationale?: string;
};

export type Block =
  | { type: "text"; text: string }
  | { type: "image"; attachment_id: string; media_type: string }
  | { type: "thinking"; text: string }
  | CodingTaskBlock
  | {
      type: "tool";
      id: string;
      tool: string;
      summary: string;
      card: "command" | "compact";
      status: "running" | "ok" | "failed" | "interrupted";
      exit_code: number | null;
      output: string | null;
    }
  | {
      type: "approval";
      approval_id: string;
      run_id: string;
      tool: string;
      command: string;
      summary: string;
      cwd: string;
      request_kind?: string | null;
      status: "pending" | "approved" | "rejected" | "cancelled";
    }
  | {
      type: "team_run";
      run_id: string;
      goal: GoalContract | null;
      lead?: string | null;
      members: MemberUnit[];
    }
  | {
      type: "run_card";
      run_id: string;
      commit_sha: string | null;
      files_changed: number;
      insertions: number;
      deletions: number;
      interrupted: boolean;
      state?: "active" | "partially_undone" | "undone";
      /** Frontend-only hydration from the checkpoint ledger aggregate. */
      undo_total?: number;
      undo_undone?: number;
      /** Frontend-only feedback from the undo interaction in this process. */
      undo_result?: UndoResultRecord;
    }
  | {
      type: "lead_summary";
      run_id: string;
      summary_source:
        | "lead_synthesis"
        | "single_passthrough"
        | "fallback_raw"
        | "pending";
      status: SummaryStatus;
      sections: SummarySection[];
      findings: Finding[];
      artifact_refs: ArtifactRef[];
    }
  | {
      // B2：live-only gate 草案块（不持久化·冻结后清·数据在 App gateBySession state·按 session_id 取）
      type: "gate_card";
      session_id: string;
    }
  | {
      // B2：live-only 队长拟失败块（不持久化）
      type: "draft_failed";
      session_id: string;
    }
  | {
      // T-C3a §4.1-A：流内决策块（只承 ask / dispatch_confirm·coding 决策不走此块）
      type: "decision_card";
      decision_id: string;
      kind: "ask" | "dispatch_confirm";
      question: string;
      options: string[];
      recommended: string | null;
      rationale: string | null;
      payload: unknown | null;
      source_run_id: string;
      status: "pending" | "chosen" | "submitting" | "failed";
      chosen_option: string | null;
      created_at: number;
    }
  | {
      type: "dispatch_card";
      run_id: string;
      member: MemberUnit;
    }
  | {
      type: "scope_change";
      changes: ScopeChangeItem[];
    }
  | {
      // 刀 R R3-T3：后端归约器持久化的收尾卡块型（db.rs Block::RunTerminal 镜像）。
      // status 取值 "completed"/"error"/"interrupted"/"needs_decision"/"blocked"/"fallback"，
      // 可能出现未来未知值 → 前端按未知态兜底展示，不收紧字面量联合。
      type: "run_terminal";
      run_id: string;
      status: string;
      message?: string | null;
    };

export type DecisionCardBlock = Extract<Block, { type: "decision_card" }>;

export type ChatMessage = {
  role: "user" | "assistant";
  content: Block[];
  engine?: string;
  agent_id?: string | null;
  agent_name_snapshot?: string | null;
  // 后端 get_messages 自 1b 起带回（前端乐观追加的消息可能没有 → 可选）。
  // 注：不加数字 id 字段——前端已用 `ChatMessage & { id: string }` 表客户端消息 id，
  // 后端 DB 数字 id 仅供锚点 resolver（backend memory_read_source）·前端暂不需要。
  created_at?: number;
};

export type ReviewFileResult = {
  path: string;
  /** true = checkpoint 账本记过 preimage，可由 app 的逐文件撤销恢复。 */
  undoable: boolean;
};

export type ReviewResult = {
  has_changes: boolean;
  stat: string;
  patch: string;
  /** plan B3：结构化变更文件数（角标用） */
  files_changed: number;
  files: ReviewFileResult[];
  /** false = 项目不是带 HEAD 的 git 工作树，无法生成 diff。 */
  diff_available: boolean;
  /** 工作目录中未纳入本次 Review 归因集合的脏文件数。 */
  other_dirty_count?: number;
  /** 状态摘要用：已提交段落覆盖的不重复文件数（commit 1 归因求和的 range diff 合计）。 */
  committed_files_changed?: number;
  /** 状态摘要用：当前未提交（`git diff HEAD`）覆盖的不重复文件数。 */
  uncommitted_files_changed?: number;
};

/** 镜像 plan 1 + Phase 2 后端 repos_repo::RepoMeta（Phase 2 加 namespace_id；project-first 加 icon）。
 *  id / source ('local'|'github') / owner / name / path / status ('active'|'archived'|'invalid') / added_at / last_used_at / namespace_id (DEFAULT 'local') / icon */
export type RepoMeta = {
  id: string;
  source: string;
  owner: string | null;
  name: string;
  path: string;
  status: string;
  added_at: number;
  last_used_at: number | null;
  /** cluster L Phase 2 plan A Task 6：所属 namespace（DEFAULT 'local'） */
  namespace_id: string;
  icon?: string | null;
};

/** plan 1 后端 detect::DetectResult 镜像（plan 2a 仅 ProjectDropdown 占位用 · onboarding 完整 UI 留 plan 2b） */
export type DetectResult = {
  available: boolean;
  version: string | null;
  path: string | null;
};

/** 镜像 plan A Task 2 加完的 db::Session · 4 字段（含 repo_id + namespace_id）。
 *  plan B B4：namespace_id 让 openSession 跨 ns 切时同步 activeNamespaceId。 */
export type Session = {
  id: string;
  title: string;
  repo_id: string | null;
  namespace_id: string | null;
  /** 后端权威路由：true = 绑定用户真实项目，agent 直接 in-place 运行。 */
  in_place: boolean;
  group_id: string | null;
  parent_session_id: string | null;
  continued_to_session_id: string | null;
  /** session-hover-menu §5.4：秒级 epoch，前端排序用 */
  created_at: number;
  pinned: boolean;
  unread: boolean;
  archived: boolean;
  archived_at: number | null;
  /** 会话累计 token 用量，由后端每轮结束后累加 */
  total_input_tokens: number;
  total_output_tokens: number;
};

export type ParsedHandoff = {
  goal: string;
  state: string;
  next: string;
  decisions: string[];
  pitfalls: string[];
  risks: string[];
};

export type ContinuationHandoffDraft = {
  doc_markdown: string;
  suggested_title: string;
  memory_projection: ParsedHandoff | null;
  warnings: string[];
};

/** 镜像 plan A 后端 namespaces_repo::NamespaceMeta · 7 字段。
 *  id (Local 固定 'local') / kind ('local' | 'github_org') / name ('Local' | org name)
 *  / is_builtin (1=Local 不可删 / 0=github_org 可删) / last_active_repo_id (切回该 namespace 时 crumb repo 自动选规则)
 *  / added_at / last_used_at */
export type NamespaceMeta = {
  id: string;
  kind: string; // 'local' | 'github_org'（运行时校验 · TS 不收紧避后端 enum 加值时前端重构）
  name: string;
  is_builtin: number; // 0 | 1（rusqlite INTEGER）
  last_active_repo_id: string | null;
  added_at: number;
  last_used_at: number | null;
};

/** 镜像 plan A 后端 app_context 新 contract（plan B 用）。
 *  - namespaces: list_active_namespaces 排序后的全 active namespace
 *  - active_namespace_id: 当前活跃 namespace（启动默认 'local'）
 *  - active_repo_id: 当前活跃 repo（启动默认 'local-default'）
 *  - repos: 当前 active_namespace 下的 active repos（plan B 智能形态计算用）
 *
 *  注：plan 2a 老 app_context contract `{ repos: RepoMeta[] }` 已被本 contract 取代 ·
 *  plan B 落地时改 App.tsx 调 invoke<AppContext>('app_context') 用新 contract。
 */
export type AppContext = {
  namespaces: NamespaceMeta[];
  active_namespace_id: string;
  active_repo_id: string | null; // 仅 github_org 0 repo 时可能 null · Local 永有 local-default
  repos: RepoMeta[];
};

export type GroupMeta = {
  id: string;
  repo_id: string;
  name: string;
  position: number;
  created_at: number;
};

// 刀2.1 Plan3：lead_step 决策引擎返回的五动作。
// 注意：LeadAction 字段 snake_case（后端 enum 字段不 rename）。
export type LeadAction =
  | { action: "reply"; rationale: string }
  | {
      action: "dispatch_worker";
      rationale: string;
      task: string;
      scope_files: string[];
      agent_hint: string | null;
      goal_title?: string;
    }
  | { action: "propose_verifier"; rationale: string; cmd: string }
  | {
      action: "ask_user";
      rationale: string;
      question: string;
      options: string[];
      recommended: string | null;
    }
  | { action: "finish"; rationale: string; evidence_refs: string[] }
  // T-C3 b2b 改动条：改动条按钮把意图给队长·队长吐这 4 个结构化交付动作·前端路由到对应后端命令。
  | { action: "commit"; rationale: string }
  | { action: "push"; rationale: string }
  | { action: "create_pr"; rationale: string; title?: string; body?: string }
  | {
      action: "publish";
      rationale: string;
      repo_name?: string;
      private?: boolean;
    };

export type LeadStepOutcome =
  | { status: "duplicate" }
  | {
      status: "decided";
      action: LeadAction;
      decisionCard: DecisionCardBlock | null;
    };

export type SessionGoal = { text: string; title: string | null };

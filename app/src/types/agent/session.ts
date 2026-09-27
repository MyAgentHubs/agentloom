export type ReviewFileResult = {
  path: string;
  /** true = the checkpoint ledger recorded a preimage, which can be restored by the app's per-file undo. */
  undoable: boolean;
};

export type ReviewResult = {
  has_changes: boolean;
  stat: string;
  patch: string;
  /** Structured changed-file count used for the badge. */
  files_changed: number;
  files: ReviewFileResult[];
  /** false = the project is not a git worktree with a HEAD, so a diff cannot be generated. */
  diff_available: boolean;
  /** Number of dirty files in the working directory not included in this Review's attribution set. */
  other_dirty_count?: number;
  /** Used for the status summary: number of distinct files covered by committed segments (total range diff attributed by commit 1). */
  committed_files_changed?: number;
  /** Used for the status summary: number of distinct files covered by current uncommitted changes (`git diff HEAD`). */
  uncommitted_files_changed?: number;
};

/** Mirrors plan 1 + Phase 2 backend repos_repo::RepoMeta (Phase 2 adds namespace_id; project-first adds icon).
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
  /** cluster L Phase 2 plan A Task 6: owning namespace (DEFAULT 'local') */
  namespace_id: string;
  icon?: string | null;
};

/** Mirror of plan 1 backend detect::DetectResult (plan 2a is only a placeholder for ProjectDropdown·the complete onboarding UI is reserved for plan 2b) */
export type DetectResult = {
  available: boolean;
  version: string | null;
  path: string | null;
};

/** Mirrors db::Session after plan A Task 2 was completed·4 fields (including repo_id + namespace_id).
 * namespace_id lets openSession keep activeNamespaceId in sync when switching across namespaces. */
export type Session = {
  id: string;
  title: string;
  repo_id: string | null;
  namespace_id: string | null;
  /** Authoritative backend routing: true = bound to the user's real project, and the agent runs directly in-place. */
  in_place: boolean;
  group_id: string | null;
  parent_session_id: string | null;
  continued_to_session_id: string | null;
  /** Epoch in seconds, used for frontend sorting. */
  created_at: number;
  pinned: boolean;
  unread: boolean;
  archived: boolean;
  archived_at: number | null;
  /** Cumulative token usage for the session, accumulated by the backend after each turn */
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

/** Mirrors plan A backend namespaces_repo::NamespaceMeta·7 fields.
 *  id (Local is fixed as 'local') / kind ('local' | 'github_org') / name ('Local' | org name)
 *  / is_builtin (1=Local cannot be deleted / 0=github_org can be deleted) / last_active_repo_id (automatic crumb repo selection rule when switching back to this namespace)
 *  / added_at / last_used_at */
export type NamespaceMeta = {
  id: string;
  kind: string; // 'local' | 'github_org' (runtime validation · TS is not narrowed to avoid frontend refactors when backend enum adds values)
  name: string;
  is_builtin: number; // 0 | 1（rusqlite INTEGER）
  last_active_repo_id: string | null;
  added_at: number;
  last_used_at: number | null;
};

/** Mirrors the new contract of plan A backend app_context (used by plan B).
 *  - namespaces: all active namespaces sorted by list_active_namespaces
 *  - active_namespace_id: currently active namespace (defaults to 'local' at startup)
 *  - active_repo_id: currently active repo (defaults to 'local-default' at startup)
 *  - repos: active repos under the current active_namespace (used for plan B intelligent-form calculation)
 *
 *  Note: the old plan 2a app_context contract `{ repos: RepoMeta[] }` has been replaced by this contract·
 *  when implementing plan B, change App.tsx to call invoke<AppContext>('app_context') using the new contract.
 */
export type AppContext = {
  namespaces: NamespaceMeta[];
  active_namespace_id: string;
  active_repo_id: string | null; // May be null only when github_org has 0 repos · Local always has local-default
  repos: RepoMeta[];
};

export type GroupMeta = {
  id: string;
  repo_id: string;
  name: string;
  position: number;
  created_at: number;
};

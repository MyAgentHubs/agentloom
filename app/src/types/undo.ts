export type ChangeKind = "created" | "modified" | "deleted";

export type UndoPreview =
  | { kind: "missing" }
  | { kind: "text"; content: string }
  | { kind: "binary"; size_bytes: number }
  | { kind: "too_large"; size_bytes: number }
  | { kind: "unsupported"; file_type: string };

export type UndoEntry = {
  file_path: string;
  change_kind: ChangeKind;
  preimage_preview: UndoPreview;
  current_preview: UndoPreview;
  is_binary: boolean;
  size_bytes: number;
  current_digest: string;
  already_undone: boolean;
  /** F1：这条 preimage 是否因为「此后又被提交过」而陈旧——true 时前端必须禁止勾选、
   * 只展示原因，撤销会把这条记录之后发生的提交内容覆盖掉。 */
  stale: boolean;
};

export type UndoIssue = {
  file_path: string;
  reason: string;
};

export type UndoReport = {
  restored: string[];
  skipped: UndoIssue[];
  failed: UndoIssue[];
};

export type UndoSelectedEntry = Pick<UndoEntry, "file_path" | "change_kind">;

/** Transient feedback for the undo interaction that just completed. */
export type UndoResultRecord = {
  session_id: string;
  run_id: string;
  report: UndoReport;
  selected_entries: UndoSelectedEntry[];
  total_entries: number;
};

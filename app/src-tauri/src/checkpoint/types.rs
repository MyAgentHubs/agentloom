use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RecordPreimageOutcome {
    Recorded,
    SkippedOutsideRoot,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct CheckpointEntry {
    pub file_path: PathBuf,
    pub allowed_root: Option<PathBuf>,
    pub existed: bool,
    pub blob_sha: Option<String>,
    pub file_mode: Option<u32>,
    pub is_symlink: bool,
    pub pre_xattrs: Option<Vec<u8>>,
    pub undone_at: Option<i64>,
    pub created_at: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeKind {
    Created,
    Modified,
    Deleted,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum UndoPreview {
    Missing,
    Text { content: String },
    Binary { size_bytes: u64 },
    TooLarge { size_bytes: u64 },
    Unsupported { file_type: String },
}

impl UndoPreview {
    pub(super) fn is_binary(&self) -> bool {
        matches!(self, Self::Binary { .. })
    }

    pub(super) fn size_bytes(&self) -> u64 {
        match self {
            Self::Text { content } => content.len() as u64,
            Self::Binary { size_bytes } | Self::TooLarge { size_bytes } => *size_bytes,
            Self::Missing | Self::Unsupported { .. } => 0,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct UndoEntry {
    pub file_path: PathBuf,
    pub change_kind: ChangeKind,
    pub preimage_preview: UndoPreview,
    pub current_preview: UndoPreview,
    pub is_binary: bool,
    pub size_bytes: u64,
    pub current_digest: String,
    pub already_undone: bool,
    /// F1 补丁：这条 preimage 是否因为「所属 run 提交之后（或 run 仍未提交、pre_head 之后）
    /// 这个文件又被提交过」而陈旧。checkpoint.rs 本身不碰 git，这里恒为 false 占位——
    /// 真正的判定在 lib.rs::list_run_undo_entries_inner 里用 filter_fresh_checkpoint_paths
    /// 跑完之后原地覆写。陈旧时前端必须禁止勾选、只展示原因，不能让「点撤销」真的把
    /// preimage 字节写回磁盘覆盖掉后续提交的内容。
    #[serde(default)]
    pub stale: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct RestoreFailure {
    pub file_path: PathBuf,
    pub reason: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct RestoreReport {
    pub restored: Vec<PathBuf>,
    pub failed: Vec<RestoreFailure>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct UndoSkip {
    pub file_path: PathBuf,
    pub reason: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct UndoReport {
    pub restored: Vec<PathBuf>,
    pub failed: Vec<RestoreFailure>,
    pub skipped: Vec<UndoSkip>,
}

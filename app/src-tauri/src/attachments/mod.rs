//! 会话附件从 `~/.agentloom/pasted/`（app 域）迁到会话工作区
//! `<workspace 根>/.agentloom/attachments/`（用户项目域）——`lib.rs` / `worktree.rs`
//! 门禁不许再涨，新逻辑单独成模块。`dir` 管落点计算 + 读取范围校验，`exclude` 管
//! `.git/info/exclude` 幂等追加。

pub mod dir;
pub mod exclude;

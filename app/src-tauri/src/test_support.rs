//! 测试隔离基础设施（仅 cfg(test) 编译）：建临时目录 + 临时 sqlite，
//! 避免污染开发者本机 ~/.agentloom 与测试间状态互染。

#![cfg(test)]

use rusqlite::Connection;
use std::path::PathBuf;

/// 建一个临时根目录，返回 (TempDir guard, 路径)。guard drop 时自动清理。
pub fn tmp_root() -> (tempfile::TempDir, PathBuf) {
    let td = tempfile::tempdir().expect("建临时目录失败");
    let p = td.path().to_path_buf();
    (td, p)
}

/// 建内存 sqlite + 跑完整 init_schema（含 cluster L 新增的 repos 表）+ seed Local namespace（Phase 2 plan A Task 2 必加 · 防 FK 失败崩既有测）。
pub fn mem_db() -> Connection {
    let c = Connection::open_in_memory().expect("open in-memory sqlite 失败");
    crate::db::init_schema(&c).expect("init_schema 失败");
    // cluster L Phase 2 plan A Task 2 必修 #3：seed Local namespace + local-default repo
    // 防 repos.namespace_id / sessions.repo_id FK 约束失败崩既有 plan 1 / 2a 测试
    // rusqlite 0.32 bundled 默认 PRAGMA foreign_keys = 1 · 实测确认（/tmp/fk_test）
    c.execute(
        "INSERT OR IGNORE INTO namespaces (id, kind, name, is_builtin, added_at) VALUES ('local', 'local', 'Local', 1, 0)",
        [],
    )
    .expect("seed Local namespace 失败");
    std::fs::create_dir_all("/tmp/agentloom-mem-local-default")
        .expect("seed local-default 测试目录失败");
    c.execute(
        "INSERT OR IGNORE INTO repos (id, namespace_id, source, name, path, status, added_at) VALUES ('local-default', 'local', 'local', '我的项目', '/tmp/agentloom-mem-local-default', 'active', 0)",
        [],
    )
    .expect("seed local-default repo 失败");
    c
}

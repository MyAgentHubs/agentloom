#![cfg(test)]

use super::*;
use crate::agent::{AgentBackend, BorrowClaudeBackend, BuildContext, NativeBackend, ParseFn};
use crate::keychain::FakeKeyStore;
use crate::remote_crypto::{derive_k_pair, generate_x25519_keypair, open, seal, EnvelopeMeta};

const PAIR_TEST_NOW: u64 = 1_700_000_000;
const PAIR_TEST_NOW_MS: u64 = 1_700_000_000_000;
const PAIR_TEST_ROOM: &str = "0123456789abcdef0123456789abcdef";

fn pairing_gateway_hello(
    session: &remote_pairing::PairingSession,
    plaintext: &[u8],
) -> remote_gateway::PairHelloFrame {
    let (remote_secret, remote_public) = generate_x25519_keypair();
    let remote_k_pair = derive_k_pair(
        &remote_secret,
        &session.desktop_public,
        &session.pairing_token,
    )
    .unwrap();
    let meta = EnvelopeMeta {
        v: 1,
        room: session.room_id.clone(),
        epoch: 0,
        kind: "control".to_owned(),
        session: None,
        command_id: None,
    };
    let (token_ct, token_n) = seal(&remote_k_pair, &meta, plaintext);
    remote_gateway::PairHelloFrame {
        room: session.room_id.clone(),
        remote_pub: remote_public,
        token_ct,
        token_n,
        origin_connection_id: "conn-pairing-test".to_owned(),
    }
}

fn fresh_pairing_slot() -> Mutex<PairingSlot> {
    let (session, _) = remote_pairing::PairingSession::begin(
        "wss://relay.example.test",
        PAIR_TEST_ROOM,
        PAIR_TEST_NOW,
    );
    Mutex::new(PairingSlot::Waiting(session))
}

fn decrypt_pair_accept_tokens(
    slot: &Mutex<PairingSlot>,
    accept: &remote_gateway::PairAcceptFrame,
) -> (String, String) {
    let guard = slot.lock().unwrap();
    let PairingSlot::SentAccept { outcome, .. } = &*guard else {
        panic!("pair.accept tokens require a SentAccept slot")
    };
    let plaintext = open(
        &outcome.device_record.k_pair,
        &remote_pairing::pair_accept_tokens_meta(&accept.room, &accept.device_id),
        &accept.tokens_ct,
        &accept.tokens_n,
    )
    .expect("pair.accept token body must decrypt under K_pair");
    let tokens: serde_json::Value =
        serde_json::from_slice(&plaintext).expect("pair.accept token body must be JSON");
    (
        tokens["capability_token"].as_str().unwrap().to_owned(),
        tokens["refresh_token"].as_str().unwrap().to_owned(),
    )
}

fn pairing_test_db() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    db::init_schema(&conn).unwrap();
    conn
}

fn cli_path_test_db() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE app_settings (\
                 key TEXT PRIMARY KEY,\
                 value TEXT NOT NULL\
             );",
    )
    .unwrap();
    conn
}

// ---------------------------------------------------------------------------------
// M2-4b：active project setting 读写 + 凭据幂等 ensure
// ---------------------------------------------------------------------------------

/// R3 起，`remote_set_active_project_in_conn` 要求 repo 真的存在于 `repos` 表——
/// `pairing_test_db()`（`db::init_schema` 全量建表）本身不会自动 seed 'local' namespace /
/// 任何 repo 行（`db.rs` 自己的 `mem()` 测试帮手也要手动补这一步，同一原因：
/// `init_schema` 不负责建示例数据），这里补齐 FK 前置条件后插入两个可用的测试 repo。
fn remote_active_project_test_db() -> Connection {
    let conn = pairing_test_db();
    conn.execute(
        "INSERT OR IGNORE INTO namespaces (id, kind, name, is_builtin, added_at) \
             VALUES ('local', 'local', 'Local', 1, 0)",
        [],
    )
    .unwrap();
    repos_repo::add_repo(
        &conn,
        "repo-1",
        "local",
        "local",
        None,
        "repo-1",
        "/tmp/m24b-repo-1",
        None,
    )
    .unwrap();
    repos_repo::add_repo(
        &conn,
        "repo-2",
        "local",
        "local",
        None,
        "repo-2",
        "/tmp/m24b-repo-2",
        None,
    )
    .unwrap();
    conn
}

fn agent_profile(id: &str, has_key: bool, is_builtin: bool) -> db::AgentProfile {
    db::AgentProfile {
        id: id.to_string(),
        name: format!("Agent {id}"),
        access: "borrow".to_string(),
        provider: "claude".to_string(),
        primary_model: Some("claude-test".to_string()),
        endpoint: Some("https://api.example.test/v1".to_string()),
        auth_mode: Some("bearer".to_string()),
        model_opus: None,
        model_sonnet: None,
        model_haiku: None,
        model_subagent: None,
        reasoning_default: "auto".to_string(),
        max_output_tokens: None,
        api_timeout_ms: None,
        compat_disable_betas: false,
        compat_disable_nonessential: false,
        compat_disable_thinking: false,
        compat_proxy: None,
        custom_headers: None,
        extra_body: None,
        cap_reasoning: None,
        cap_computer_use: None,
        cap_lead: None,
        has_key,
        is_builtin,
        enabled: true,
        sort_order: 0,
        created_at: 100,
        updated_at: 100,
    }
}

fn lead_capable_profile(id: &str) -> db::AgentProfile {
    let mut profile = agent_profile(id, true, false);
    profile.access = "native".to_string();
    profile.provider = "claude".to_string();
    profile.cap_lead = Some("native_cli".to_string());
    profile
}

fn native_codex_profile(id: &str) -> db::AgentProfile {
    let mut profile = agent_profile(id, true, false);
    profile.access = "native".to_string();
    profile.provider = "codex".to_string();
    profile.primary_model = None;
    profile
}

fn borrow_lead_capable_profile(id: &str) -> db::AgentProfile {
    // agent_profile() 默认 access="borrow"。
    let mut profile = agent_profile(id, true, false);
    profile.provider = "deepseek".to_string();
    profile
}

fn harness_lead_capable_profile(id: &str) -> db::AgentProfile {
    let mut profile = agent_profile(id, true, false);
    profile.access = "harness".to_string();
    profile.provider = "deepseek".to_string();
    profile.endpoint = Some("https://api.deepseek.com/v1".to_string());
    profile.primary_model = Some("deepseek-chat".to_string());
    profile
}

fn insert_agent(conn: &Connection, profile: db::AgentProfile) {
    db::upsert_agent(conn, &profile).unwrap();
}

struct TestHomeGuard {
    old: Option<std::ffi::OsString>,
}

impl TestHomeGuard {
    fn set(path: &std::path::Path) -> Self {
        let old = std::env::var_os("HOME");
        std::env::set_var("HOME", path);
        Self { old }
    }
}

impl Drop for TestHomeGuard {
    fn drop(&mut self) {
        match &self.old {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
    }
}

fn git_ok(dir: &std::path::Path, args: &[&str]) {
    let out = std::process::Command::new("git")
        .current_dir(dir)
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn init_test_repo(repo: &std::path::Path) {
    std::fs::create_dir_all(repo).unwrap();
    git_ok(repo, &["init", "-q"]);
    git_ok(repo, &["config", "user.email", "t@t"]);
    git_ok(repo, &["config", "user.name", "t"]);
    git_ok(repo, &["config", "commit.gpgsign", "false"]);
    std::fs::write(repo.join("seed.md"), "seed\n").unwrap();
    git_ok(repo, &["add", "seed.md"]);
    git_ok(repo, &["commit", "-qm", "seed"]);
}

fn setup_trashed_parent_with_trashed_child(
    repo: &std::path::Path,
    parent: &str,
    child: &str,
) -> Db {
    let conn = crate::test_support::mem_db();
    let namespace_id = format!("gh-{parent}");
    let repo_id = format!("repo-{parent}");
    conn.execute(
        "INSERT OR IGNORE INTO namespaces (id, kind, name, is_builtin, added_at) \
             VALUES (?1, 'github_org', 'GitHub Delete Test', 0, 0)",
        [namespace_id.as_str()],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO repos (id, namespace_id, source, owner, name, path, status, added_at) \
             VALUES (?1, ?2, 'github', 'owner', 'repo', ?3, 'active', 0)",
        (
            repo_id.as_str(),
            namespace_id.as_str(),
            repo.to_str().unwrap(),
        ),
    )
    .unwrap();
    db::create_session(&conn, parent, "parent", &repo_id, &namespace_id).unwrap();
    db::create_session(&conn, child, "child", &repo_id, &namespace_id).unwrap();
    db::set_session_parent(&conn, child, Some(parent)).unwrap();
    db::set_session_continued_to(&conn, parent, Some(child)).unwrap();
    let db = Db(crate::perf_probe::TimedMutex::new(conn));
    let running = Running::default();
    delete_session_inner(&db, &running, child).unwrap();
    delete_session_inner(&db, &running, parent).unwrap();
    db
}

fn setup_repo_continuation_parent(
    conn: &Connection,
    repo: &std::path::Path,
    parent: &str,
    ensure_parent_workspace: bool,
) {
    namespaces_repo::add_namespace(conn, "ns-cont", "github_org", "Continuation", 0).unwrap();
    repos_repo::add_repo(
        conn,
        "repo-cont",
        "ns-cont",
        "github",
        Some("owner"),
        "repo",
        repo.to_str().unwrap(),
        None,
    )
    .unwrap();
    db::create_session(conn, parent, "Parent", "repo-cont", "ns-cont").unwrap();
    db::upsert_memory_block(conn, parent, "goal", "Parent goal", None, Some("lead")).unwrap();
    db::upsert_memory_block(conn, parent, "state", "Parent state", None, Some("lead")).unwrap();
    db::upsert_memory_block(conn, parent, "next", "Parent next", None, Some("lead")).unwrap();
    db::insert_memory_entry(
        conn,
        parent,
        "decision",
        "Parent decision",
        "[]",
        "[]",
        Some("lead"),
        Some("high"),
        false,
    )
    .unwrap();
    if ensure_parent_workspace && !session_is_in_place(conn, parent).unwrap() {
        worktree::ensure_workspace(parent, Some(repo), false).unwrap();
    }
}
fn setup_local_landed_multiline(
    conn: &rusqlite::Connection,
    project: &std::path::Path,
) -> (String, String) {
    let git = |args: &[&str]| {
        let out = std::process::Command::new("git")
            .current_dir(project)
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {:?}: {}",
            args,
            String::from_utf8_lossy(&out.stderr)
        );
    };
    git(&["init", "-q"]);
    git(&["config", "user.email", "t@t"]);
    git(&["config", "user.name", "t"]);
    git(&["config", "commit.gpgsign", "false"]);
    // R-B3 项 2（opus 复核 Major·夹具守护被拆·根锚定回归恢复辨别力）：`diff.relative true`
    // 让**任何**在这个仓库里跑的 `git diff` / `git diff --numstat`（不必显式传 --relative）
    // 都对 cwd 敏感——若某次回归把 review/landing 的 git cwd 从项目根改回 per-session 子
    // 目录（这里的子目录从不存在真实内容，纯粹是解析出的路径），仓根侧的 base.txt/
    // added.txt 会被 git 静默排除在 diff 之外，下面几个测试断言的「patch 里含
    // base.txt/added.txt」「files_changed == 2」会立刻转红——这就是「谁把实现改回子目录
    // 锚定这批测试立刻红」的机制来源，不是靠肉眼比对路径字符串。
    git(&["config", "diff.relative", "true"]);
    std::fs::write(project.join("base.txt"), "l1\nl2\nl3\n").unwrap();
    git(&["add", "."]);
    git(&["commit", "-qm", "base"]);
    let pre = crate::worktree::rev_parse_head(project).unwrap();

    // worker 改动：改 base.txt（删 1 改 → 实际 +2/-1）+ 新增多行文件（+3）。
    std::fs::write(project.join("base.txt"), "l1\nX2\nl3\nl4\n").unwrap();
    std::fs::write(project.join("added.txt"), "a1\na2\na3\n").unwrap();
    git(&["add", "."]);
    git(&["commit", "-qm", "landed"]);
    let landed = crate::worktree::rev_parse_head(project).unwrap();

    db::create_session(conn, "s1", "t", "local-default", "local").unwrap();
    // R-B2 项 2b → R-B3 项 2（Minor-9 注释勘误）：**恢复 NULL scope**（R-B2 曾误置成
    // 'root'，把这个夹具的子目录解析结果与根目录解析结果强行拍成同一条路径，使下面
    // `session_review_local_inplace_*` 系列测试对「review/landing 必须根锚定」这条不变
    // 量彻底失去辨别力——置 root 后无论实现读根还是读子目录，两者本就是同一个目录，测试
    // 测不出区别）。改回 NULL 后，子目录解析结果（`project/<safe_id(s1)>/`，纯解析、不
    // 建目录）与根目录（`project/`）不再重合，真正靠上面的 `diff.relative true` 制造根
    // cwd vs 子目录 cwd 的可观测差异——这样才是名副其实的「根锚定回归」守护。
    conn.execute(
        "UPDATE repos SET path = ?1 WHERE id = 'local-default'",
        [project.to_str().unwrap()],
    )
    .unwrap();
    crate::db::insert_artifact(
        conn,
        &crate::db::Artifact {
            id: "art-1".into(),
            session_id: "s1".into(),
            run_id: "r1".into(),
            member_assignment_id: "a1".into(),
            branch: "agentloom/a1".into(),
            base_sha: pre.clone(),
            commit_sha: Some(landed.clone()),
            files_changed: 2,
            state: "merged".into(),
            created_at: 1,
        },
    )
    .unwrap();
    // T2 的缺口：行数存 0（record_inplace_artifact_landing 写 insertions:0/deletions:0）。
    crate::db::insert_landing_commit(
        conn,
        &crate::db::LandingCommit {
            id: "lc-1".into(),
            session_id: "s1".into(),
            run_id: "r1".into(),
            artifact_id: Some("art-1".into()),
            pre_head: pre.clone(),
            landed_head: landed.clone(),
            commit_count: 1,
            files_changed: 2,
            insertions: 0,
            deletions: 0,
            created_at: crate::db::now_secs(),
        },
    )
    .unwrap();
    (pre, landed)
}

struct ReviewTestHome {
    old: Option<std::ffi::OsString>,
}

impl ReviewTestHome {
    fn set(path: &std::path::Path) -> Self {
        let old = std::env::var_os("HOME");
        std::env::set_var("HOME", path);
        Self { old }
    }
}

impl Drop for ReviewTestHome {
    fn drop(&mut self) {
        match self.old.take() {
            Some(value) => std::env::set_var("HOME", value),
            None => std::env::remove_var("HOME"),
        }
    }
}

fn setup_inplace_review_session(
    conn: &rusqlite::Connection,
    project: &std::path::Path,
    session_id: &str,
) {
    std::fs::create_dir_all(project).unwrap();
    let git = |args: &[&str]| {
        let output = std::process::Command::new("git")
            .current_dir(project)
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    };
    git(&["init", "-q"]);
    git(&["config", "user.email", "review@test"]);
    git(&["config", "user.name", "Review Test"]);
    git(&["config", "commit.gpgsign", "false"]);
    std::fs::write(project.join("tracked.md"), "before\n").unwrap();
    git(&["add", "tracked.md"]);
    git(&["commit", "-qm", "base"]);

    db::create_session(conn, session_id, "review", "local-default", "local").unwrap();
    conn.execute(
        "UPDATE repos SET path = ?1 WHERE id = 'local-default'",
        [project.to_str().unwrap()],
    )
    .unwrap();
}

fn review_file<'a>(review: &'a worktree::Review, path: &str) -> &'a worktree::ReviewFile {
    review
        .files
        .iter()
        .find(|file| file.path == path)
        .unwrap_or_else(|| panic!("Review 缺文件 {path}：{}", review.patch))
}

fn review_test_git(project: &std::path::Path, args: &[&str]) {
    let output = std::process::Command::new("git")
        .current_dir(project)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn checkpoint_review_path(conn: &rusqlite::Connection, session_id: &str, path: &std::path::Path) {
    conn.execute(
        "INSERT INTO checkpoint_entries \
             (session_id, run_id, file_path, existed, created_at) \
             VALUES (?1, 'review-run', ?2, 1, 1)",
        rusqlite::params![session_id, path.to_str().unwrap()],
    )
    .unwrap();
}

fn msg(role: &str, text: &str, engine: Option<&str>) -> db::Message {
    db::Message {
        id: 0,
        created_at: 0,
        role: role.to_string(),
        content: vec![db::Block::Text {
            text: text.to_string(),
        }],
        engine: engine.map(|e| e.to_string()),
        agent_id: None,
        agent_name_snapshot: None,
        revision: 1,
    }
}

fn git_cmd(dir: &std::path::Path, args: &[&str]) {
    let out = std::process::Command::new("git")
        .current_dir(dir)
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn git_out(dir: &std::path::Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git")
        .current_dir(dir)
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn command_args(cmd: &Command) -> Vec<String> {
    cmd.get_args()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect()
}

fn contains_adjacent_pair(args: &[String], left: &str, right: &str) -> bool {
    args.windows(2)
        .any(|window| window[0] == left && window[1] == right)
}

fn env_value(cmd: &Command, key: &str) -> Option<Option<String>> {
    cmd.get_envs()
        .find(|(k, _)| *k == key)
        .map(|(_, v)| v.map(|value| value.to_string_lossy().into_owned()))
}

struct TestHome {
    old_home: Option<std::ffi::OsString>,
    _guard: tempfile::TempDir,
    _home_env_guard: std::sync::MutexGuard<'static, ()>,
    path: std::path::PathBuf,
}

impl TestHome {
    fn new() -> Self {
        let home_env_guard = crate::worktree::test_home_lock();
        let old_home = std::env::var_os("HOME");
        let guard = tempfile::Builder::new()
            .prefix("agentloom-test-home")
            .tempdir_in("/private/tmp")
            .expect("建测试 HOME 失败");
        let path = guard.path().to_path_buf();
        std::env::set_var("HOME", &path);
        Self {
            old_home,
            _guard: guard,
            _home_env_guard: home_env_guard,
            path,
        }
    }

    fn apply(&self) {
        std::env::set_var("HOME", &self.path);
    }
}

impl Drop for TestHome {
    fn drop(&mut self) {
        match &self.old_home {
            Some(home) => std::env::set_var("HOME", home),
            None => std::env::remove_var("HOME"),
        }
    }
}

/// H1/A1 防回归（opus 对抗审 F5，D2 后加固）：断言 `db.0.lock()` 拿到的 guard 在 `cmd.spawn()`
/// 之前就已经释放——用源码形状断言而不是运行时断言，因为「锁的持有时长」是时序属性，单线程
/// 单测测不出观察得到的行为差异（build 完立即释放 vs 一直握到 spawn 完，两者跑同一份测试
/// 断言全绿）。仓内已有先例（`send_entries_call_shared_new_session_reservation_boundary`）用
/// `include_str!("lib.rs")` 切函数体做源码级校验，这里照搬同一手法。
///
/// **D2 改法（原「找独立成行 `};`」的版本被 reviewer 用一种绕过手法实证：把
/// `let conn = db.0.lock()...` 从内层 block 提到闭包顶层、内层 block 原样保留——guard 活过了
/// spawn，但内层 block 依然收着一个 `};`，旧断言照样绿）**：改成比缩进——正确写法里
/// `db.0.lock()` 在内层 block 里，缩进必须严格深于闭包体里 `cmd.spawn(` 的缩进；guard 一旦被
/// 挪到跟 spawn 同一层（或更外层），两者缩进就会相等或反过来，这里就会红。
///
/// **老实交代这条测试挡得住什么、挡不住什么**：挡得住"guard 被挪出内层 block、活到跟 spawn
/// 同一层或更外层"这一类改法（不管有没有保留 `{ }` 外壳）。挡不住的：比如在内层 block 外面
/// 又重新 `db.0.lock()` 一次、把第二个 guard 存到闭包捕获的变量里带到 spawn 之后——这种更绕
/// 的写法本测试看不出来，需要人工 review。
fn assert_spawn_after_lock_released(closure_body: &str, label: &str) {
    let lock_idx = closure_body.find("db.0.lock()").unwrap_or_else(|| {
        panic!("{label}: 闭包体里没找到 db.0.lock()，测试的切片标记可能已经过期")
    });
    // D5 续刀：prompt 改走 stdin 后，直接 `cmd.spawn(` 换成了统一口
    // `agent::spawn_with_stdin_prompt(&mut cmd, ..)`——搜索标记同步更新。
    let spawn_idx = closure_body
        .find("spawn_with_stdin_prompt(")
        .unwrap_or_else(|| {
            panic!("{label}: 闭包体里没找到 spawn_with_stdin_prompt(，测试的切片标记可能已经过期")
        });
    assert!(
        spawn_idx > lock_idx,
        "{label}: spawn_with_stdin_prompt( 出现在 db.0.lock() 之前，切片范围不对"
    );
    fn leading_spaces_of_line_at(text: &str, byte_idx: usize) -> usize {
        let line_start = text[..byte_idx].rfind('\n').map(|i| i + 1).unwrap_or(0);
        text[line_start..].chars().take_while(|c| *c == ' ').count()
    }
    let lock_indent = leading_spaces_of_line_at(closure_body, lock_idx);
    let spawn_indent = leading_spaces_of_line_at(closure_body, spawn_idx);
    assert!(
            lock_indent > spawn_indent,
            "{label}: db.0.lock() 所在行缩进（{lock_indent} 格）应严格深于 spawn_with_stdin_prompt( 所在行缩进\
             （{spawn_indent} 格）——db.0.lock() 应该在专门收 conn 的内层 block 里，build 完这个\
             内层 block 就结束、guard 随之释放，spawn 在外层、更浅的缩进上执行。缩进相等或更浅\
             说明 guard 被挪出了内层 block、活到了跟 spawn 同层或更外层（H1/A1 要修的正是这个）"
        );
}

/// H2 清单第一批同款护栏（`run_verifier_artifact` / `delete_session_inner`）：跟
/// `assert_spawn_after_lock_released` 一个精神——锁的持有时长是时序属性，单线程单测测不出
/// 运行时差异，只能用源码形状断言。这两处的"慢活"不是 `cmd.spawn(`（没有子进程 spawn 调用
/// 点），而是各自的慢函数调用（`crate::worktree::run_verifier(` /
/// `crate::worktree::trash_session_workspace(`），所以另起一个可传标记字符串的版本。
///
/// **没有照搬老版本的「比缩进」判法**：老版本靠「spawn 跟 lock 同在一个闭包体里、lock 在内层
/// block、spawn 在闭包顶层」这个固定形状，缩进深浅正好等价于时序先后。这两处的控制流形状不一样
/// ——`delete_session_inner` 里锁在 `let workspace = { ... };` 独立语句块中，慢活在紧接着的
/// `match workspace { Ok(SessionWorkspace::Repo(repo)) => { ... } }` 分支里，分支体缩进反而
/// **比** 锁所在的行更深，尽管它在时序上严格发生在锁释放之后——照搬缩进判法会对着正确代码误报红。
/// 改用直接测「包住这次 lock 的最内层 block 什么时候关闭」：从 `lock_marker` 位置起扫描花括号
/// 深度，第一次深度归零就是这个 block 收尾的字节位置（= guard 释放点），断言这个位置严格早于
/// `slow_marker` 出现的位置——这个判法不依赖具体缩进形状，对当前两种写法都成立，也依然能抓「把
/// 慢活挪回锁的 block 里」这种回退。
///
/// 2026-07-29 opus 对抗审对着这套 helper 实测出三组假阴性，本版逐条补了：
/// ① 锁块内注释含孤立 `}`——原始花括号计数会把注释里的 `}` 当成真的收尾，提前判定 guard 已
///    释放，掩盖「guard 其实还活到慢活调用之后」的真回归；
/// ② 双锁横跨——原来只查第一次出现的 `lock_marker`，如果回归是在正确的第一次 lock 之后又插了
///    一次跨过慢活调用的 `db.0.lock()`，只看第一次会漏掉第二次；
/// ③ 慢活后只有注释里出现 lock_marker——「补一道」检查原来是裸 `contains`，会被『真代码删掉了
///    重新拿锁，只留一句提到它的注释』骗过去，误判「已经重新拿锁」。
/// 修法：`strip_comments_and_strings` 把 `//`/`///` 行注释和双引号字符串字面量的内容整个替换
/// 掉再扫描（花括号计数 + marker 查找全在剥干净的文本上做，不再信注释/字符串里的字符）；
/// `extract_fn_body` 花括号配对精确截到这一个函数自己的收尾 `}`（不再靠"切到下一个 fn 名字符
/// 串出现处"，那种切法在本刀新插入 `finalize_session_trash` 之后会把它也吃进
/// `delete_session_inner` 的"函数体"里）；本函数改成扫描 slow_marker 之前**所有**
/// `lock_marker` 出现处，逐个验证各自的 block 都在 slow_marker 之前收尾。
/// 三组假阴性各自的最小复现 + 断言"新实现确实红/确实不误报"固化在下面
/// `assert_call_after_lock_released_catches_*` / `_does_not_false_positive_on_*` 几个 helper 自
/// 测里。
fn strip_comments_and_strings(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '/' && chars.peek() == Some(&'/') {
            // 行注释（含 `///` 文档注释）：吃到换行前，用占位符替换，不含花括号/marker 文本。
            while let Some(&nc) = chars.peek() {
                if nc == '\n' {
                    break;
                }
                chars.next();
            }
            out.push_str("/*stripped-comment*/");
        } else if c == '"' {
            // 双引号字符串字面量（处理 `\"` 转义）：整段内容用占位符替换。
            while let Some(nc) = chars.next() {
                if nc == '\\' {
                    chars.next(); // 转义字符本体也吃掉，不放进 out
                    continue;
                }
                if nc == '"' {
                    break;
                }
            }
            out.push_str("\"stripped-string\"");
        } else {
            out.push(c);
        }
    }
    out
}

/// 从（已经 `strip_comments_and_strings` 剥干净的）源码文本里精确抠出某个顶层 fn 的函数体：
/// 定位 `fn_needle`，找函数签名后的第一个 `{`，花括号配对找到匹配的收尾 `}`——不再依赖"下一
/// 个 fn 名字符串出现的位置"做截断（那种切法在本刀往 `delete_session_inner` 和
/// `restore_session` 之间插了 `finalize_session_trash` 后会把邻居函数也吃进来，2026-07-29
/// opus 对抗审揪出的假阴性之一）。
fn extract_fn_body<'a>(stripped_source: &'a str, fn_needle: &str, label: &str) -> &'a str {
    let after_sig = stripped_source.split(fn_needle).nth(1).unwrap_or_else(|| {
        panic!("{label}: 源码里没找到 {fn_needle:?}，测试的切片标记可能已经过期")
    });
    let open_rel = after_sig
        .find('{')
        .unwrap_or_else(|| panic!("{label}: {fn_needle:?} 后面没找到函数体开头的 `{{`"));
    let from_open = &after_sig[open_rel..];
    let mut depth: i32 = 0;
    for (i, c) in from_open.char_indices() {
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return &from_open[..=i];
                }
            }
            _ => {}
        }
    }
    panic!("{label}: {fn_needle:?} 的函数体没扫到匹配的收尾 `}}`，测试的切片标记可能已经过期");
}

fn test_db() -> db::Db {
    db::Db(crate::perf_probe::TimedMutex::new(
        crate::test_support::mem_db(),
    ))
}

// 刀 R R3-T1：归约器过滤 lead 编排内部工具（mcp__agentloom__* / ToolSearch）——
// 与前端 live HIDDEN_TOOLS（app/src/lib/streamItems.ts）同款语义，直接驱动 DisplayReducer。

fn base_run_outcome(run_id: &str) -> display_reduce::RunOutcome {
    display_reduce::RunOutcome {
        run_id: run_id.to_string(),
        exit_success: true,
        interrupted: false,
        saw_error: false,
        saw_blocked: false,
        saw_needs_decision: false,
        finish_called: Some(true),
        commit_sha: None,
        files_changed: None,
        insertions: None,
        deletions: None,
        final_text: None,
    }
}

#[path = "tests/agent_environment.rs"]
mod agent_environment;
#[path = "tests/agent_keys.rs"]
mod agent_keys;
#[path = "tests/agent_profiles.rs"]
mod agent_profiles;
#[path = "tests/answer_question.rs"]
mod answer_question;
#[path = "tests/artifact_merge.rs"]
mod artifact_merge;
#[path = "tests/attachments.rs"]
mod attachments;
#[path = "tests/autofeed.rs"]
mod autofeed;
#[path = "tests/boot_trace.rs"]
mod boot_trace;
#[path = "tests/cli_paths.rs"]
mod cli_paths;
#[path = "tests/codex_images.rs"]
mod codex_images;
#[path = "tests/continuation_inheritance.rs"]
mod continuation_inheritance;
#[path = "tests/continuation_lifecycle.rs"]
mod continuation_lifecycle;
#[path = "tests/delivery_order.rs"]
mod delivery_order;
#[path = "tests/dispatch_reconcile.rs"]
mod dispatch_reconcile;
#[path = "tests/display_reduction.rs"]
mod display_reduction;
#[path = "tests/drain_scheduling.rs"]
mod drain_scheduling;
#[path = "tests/event_envelope.rs"]
mod event_envelope;
#[path = "tests/finalizer.rs"]
mod finalizer;
#[path = "tests/first_event_injection.rs"]
mod first_event_injection;
#[path = "tests/first_event_watchdog.rs"]
mod first_event_watchdog;
#[path = "tests/github_projects.rs"]
mod github_projects;
#[path = "tests/handoff.rs"]
mod handoff;
#[path = "tests/inplace_delivery.rs"]
mod inplace_delivery;
#[path = "tests/invoke_names.rs"]
mod invoke_names;
#[path = "tests/landing.rs"]
mod landing;
#[path = "tests/lead_commands.rs"]
mod lead_commands;
#[path = "tests/lead_compaction.rs"]
mod lead_compaction;
#[path = "tests/lead_contracts.rs"]
mod lead_contracts;
#[path = "tests/lead_terminal.rs"]
mod lead_terminal;
#[path = "tests/lock_boundaries.rs"]
mod lock_boundaries;
#[path = "tests/pairing_handshake.rs"]
mod pairing_handshake;
#[path = "tests/project_bootstrap.rs"]
mod project_bootstrap;
#[path = "tests/project_files.rs"]
mod project_files;
#[path = "tests/project_management.rs"]
mod project_management;
#[path = "tests/prompts.rs"]
mod prompts;
#[path = "tests/relay_workers.rs"]
mod relay_workers;
#[path = "tests/remote_answers.rs"]
mod remote_answers;
#[path = "tests/remote_delivery_routing.rs"]
mod remote_delivery_routing;
#[path = "tests/remote_inbox.rs"]
mod remote_inbox;
#[path = "tests/remote_input.rs"]
mod remote_input;
#[path = "tests/remote_pairing_completion.rs"]
mod remote_pairing_completion;
#[path = "tests/remote_refresh.rs"]
mod remote_refresh;
#[path = "tests/remote_registry.rs"]
mod remote_registry;
#[path = "tests/remote_settings.rs"]
mod remote_settings;
#[path = "tests/resume_pending.rs"]
mod resume_pending;
#[path = "tests/review_checkpoints.rs"]
mod review_checkpoints;
#[path = "tests/review_git_isolation.rs"]
mod review_git_isolation;
#[path = "tests/review_history.rs"]
mod review_history;
#[path = "tests/review_landing.rs"]
mod review_landing;
#[path = "tests/run_closeout.rs"]
mod run_closeout;
#[path = "tests/run_recovery.rs"]
mod run_recovery;
#[path = "tests/run_reservation.rs"]
mod run_reservation;
#[path = "tests/run_slot_lifecycle.rs"]
mod run_slot_lifecycle;
#[path = "tests/sandbox_commands.rs"]
mod sandbox_commands;
#[path = "tests/send_plan.rs"]
mod send_plan;
#[path = "tests/session_deletion.rs"]
mod session_deletion;
#[path = "tests/session_lifecycle.rs"]
mod session_lifecycle;
#[path = "tests/source_invariants.rs"]
mod source_invariants;
#[path = "tests/startup.rs"]
mod startup;
#[path = "tests/stop_processes.rs"]
mod stop_processes;
#[path = "tests/team_plan.rs"]
mod team_plan;
#[path = "tests/terminal_transport.rs"]
mod terminal_transport;
#[path = "tests/windows_process.rs"]
mod windows_process;
#[path = "tests/workspace_reconcile.rs"]
mod workspace_reconcile;
#[path = "tests/workspace_routing.rs"]
mod workspace_routing;

use rusqlite::Connection;

pub(super) fn apply(conn: &Connection) -> rusqlite::Result<()> {
    apply_foundation(conn)?;
    apply_coordination(conn)?;
    apply_agent_memory(conn)?;
    Ok(())
}

fn apply_foundation(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS app_settings (
            key TEXT PRIMARY KEY,
            value TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS sessions (
            id TEXT PRIMARY KEY,
            title TEXT NOT NULL,
            created_at INTEGER NOT NULL,
            total_input_tokens INTEGER NOT NULL DEFAULT 0,
            total_output_tokens INTEGER NOT NULL DEFAULT 0
        );
        CREATE TABLE IF NOT EXISTS session_agent_configs (
            session_id TEXT PRIMARY KEY REFERENCES sessions(id) ON DELETE CASCADE,
            lead_agent_id TEXT REFERENCES agents(id) ON DELETE SET NULL,
            member_agent_ids TEXT NOT NULL DEFAULT '[]' CHECK (json_valid(member_agent_ids))
        );
        CREATE TABLE IF NOT EXISTS messages (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            session_id TEXT NOT NULL,
            role TEXT NOT NULL,
            content TEXT NOT NULL CHECK (json_valid(content)),
            engine TEXT,
            agent_id TEXT,
            agent_name_snapshot TEXT,
            -- 刀 R P0-2：防重复写键（可空·NULL 不参与下方部分唯一索引）。
            dedup_key TEXT,
            -- msgfix1 T2（M0 §10.7）：该消息内容版本唯一真相源。新建行默认 1，每次 content 原地更新点原子 +1（见 update_dispatch_card_terminal / update_decision_card_status）。
            revision INTEGER NOT NULL DEFAULT 1,
            created_at INTEGER NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_messages_session ON messages(session_id, id);
        CREATE INDEX IF NOT EXISTS idx_messages_history
            ON messages(session_id, id DESC)
            WHERE role IN ('user','assistant');
        CREATE TABLE IF NOT EXISTS member_report_delivery (
            session_id TEXT NOT NULL,
            message_id INTEGER NOT NULL,
            assignment_id TEXT,
            delivered_at INTEGER,
            PRIMARY KEY (session_id, message_id)
        );
        CREATE TABLE IF NOT EXISTS attachments (
            id TEXT PRIMARY KEY,
            session_id TEXT NOT NULL,
            kind TEXT NOT NULL,
            sha256 TEXT NOT NULL,
            media_type TEXT,
            byte_size INTEGER,
            rel_path TEXT NOT NULL,
            width INTEGER,
            height INTEGER,
            created_at INTEGER NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_attachments_sha ON attachments(sha256);
        -- cluster L 新增（plan 1）：repos 表
        CREATE TABLE IF NOT EXISTS repos (
            id TEXT PRIMARY KEY,
            source TEXT NOT NULL DEFAULT 'local'
                CHECK (source IN ('local', 'github')),
            owner TEXT,
            name TEXT NOT NULL,
            path TEXT NOT NULL UNIQUE,
            status TEXT NOT NULL DEFAULT 'active'
                CHECK (status IN ('active', 'archived', 'invalid')),
            added_at INTEGER NOT NULL,
            last_used_at INTEGER
        );
        -- cluster L Phase 2 新增：namespaces 表（spec §3.2）
        CREATE TABLE IF NOT EXISTS namespaces (
            id TEXT PRIMARY KEY,
            kind TEXT NOT NULL
                CHECK (kind IN ('local', 'github_org')),
            name TEXT NOT NULL,
            is_builtin INTEGER NOT NULL DEFAULT 0,
            last_active_repo_id TEXT,
            added_at INTEGER NOT NULL,
            last_used_at INTEGER
        );
        -- cluster L Phase 3 plan C2-A：Local virtual groups 持久层
        CREATE TABLE IF NOT EXISTS session_groups (
            id TEXT PRIMARY KEY,
            namespace_id TEXT NOT NULL REFERENCES namespaces(id) ON DELETE CASCADE,
            repo_id TEXT NOT NULL REFERENCES repos(id) ON DELETE CASCADE,
            name TEXT NOT NULL,
            position INTEGER NOT NULL DEFAULT 0,
            created_at INTEGER NOT NULL
        );
        -- plan B1 §1：run_commits ledger（每轮一行 · 轮账本 + 内联卡数据源）
        CREATE TABLE IF NOT EXISTS run_commits (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            session_id TEXT NOT NULL,
            run_id TEXT NOT NULL,
            engine TEXT NOT NULL,
            pre_head TEXT NOT NULL,
            post_head TEXT,
            commit_sha TEXT,
            files_changed INTEGER,
            insertions INTEGER,
            deletions INTEGER,
            interrupted INTEGER NOT NULL DEFAULT 0,
            state TEXT NOT NULL DEFAULT 'running'
                CHECK (state IN ('running', 'active', 'failed', 'undone', 'kept', 'discarded')),
            created_at INTEGER NOT NULL,
            UNIQUE (session_id, run_id)
        );
        CREATE INDEX IF NOT EXISTS idx_run_commits_session ON run_commits(session_id, id);
        CREATE TABLE IF NOT EXISTS run_commit_intents (
            session_id TEXT NOT NULL,
            run_id TEXT NOT NULL,
            expected_head TEXT NOT NULL,
            previous_state TEXT NOT NULL,
            created_at INTEGER NOT NULL,
            PRIMARY KEY (session_id, run_id)
        );
        CREATE TABLE IF NOT EXISTS checkpoint_entries (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            session_id TEXT NOT NULL,
            run_id TEXT NOT NULL,
            member_id TEXT,
            file_path TEXT NOT NULL,
            existed INTEGER NOT NULL,
            blob_sha TEXT,
            file_mode INTEGER,
            is_symlink INTEGER NOT NULL DEFAULT 0,
            pre_xattrs BLOB,
            allowed_root TEXT,
            post_sha TEXT,
            post_missing INTEGER NOT NULL DEFAULT 0,
            post_file_type TEXT,
            post_mode INTEGER,
            post_nlink INTEGER,
            post_inode INTEGER,
            post_xattr_sha TEXT,
            post_tainted INTEGER NOT NULL DEFAULT 0,
            undone_at INTEGER,
            created_at INTEGER NOT NULL,
            UNIQUE (session_id, run_id, file_path)
        );
        CREATE INDEX IF NOT EXISTS idx_checkpoint_entries_run
            ON checkpoint_entries(session_id, run_id);",
    )?;
    Ok(())
}

fn apply_coordination(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "-- Agent Team M2 §5.3：team run 启动锚点（崩溃恢复 + member cleanup 数据源）。
        CREATE TABLE IF NOT EXISTS team_run_pending (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            session_id TEXT NOT NULL,
            run_id TEXT NOT NULL UNIQUE,
            goal TEXT,
            lead_participant_id TEXT,
            assignments_json TEXT NOT NULL DEFAULT '[]',
            started_at INTEGER NOT NULL,
            state TEXT NOT NULL DEFAULT 'running' CHECK (state IN ('running','interrupted','done')),
            created_at INTEGER NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_team_run_pending_session ON team_run_pending(session_id, id);
        CREATE TABLE IF NOT EXISTS decision_ledger (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            session_id TEXT NOT NULL,
            run_id TEXT,
            source_assignment_id TEXT,
            text TEXT NOT NULL,
            source_refs_json TEXT NOT NULL DEFAULT '[]',
            supersedes_json TEXT NOT NULL DEFAULT '[]',
            source_kind TEXT,
            confidence TEXT,
            created_at INTEGER NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_decision_ledger_session ON decision_ledger(session_id, id);
        CREATE TABLE IF NOT EXISTS memory_entries (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            session_id TEXT NOT NULL,
            category TEXT NOT NULL,
            text TEXT NOT NULL,
            source_refs_json TEXT NOT NULL DEFAULT '[]' CHECK (json_valid(source_refs_json)),
            supersedes_json TEXT NOT NULL DEFAULT '[]' CHECK (json_valid(supersedes_json)),
            source TEXT,
            confidence TEXT,
            pinned INTEGER NOT NULL DEFAULT 0,
            created_at INTEGER NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_memory_entries_session ON memory_entries(session_id, id);

        -- 刀2.1（spec §6.1）：Lead Decision Loop 会话级持久游标。crash 重启据此续。
        -- autonomy 是安全档位·后端 lead_step 要读 → 落 DB（非 localStorage）。
        CREATE TABLE IF NOT EXISTS lead_loop_state (
            session_id TEXT PRIMARY KEY,
            autonomy TEXT NOT NULL DEFAULT 'cautious'
                CHECK (autonomy IN ('cautious','handsfree','auto')),
            active_run_id TEXT,
            active_task_id TEXT,
            last_event_cursor TEXT,
            updated_at INTEGER NOT NULL
        );

        -- coding 闭环 刀1（spec §1.7）：持久 Task Graph 状态机·落 app 域·守 D32。
        CREATE TABLE IF NOT EXISTS artifacts (
            id TEXT PRIMARY KEY,
            session_id TEXT NOT NULL,
            run_id TEXT NOT NULL,
            member_assignment_id TEXT NOT NULL,
            branch TEXT NOT NULL,
            base_sha TEXT NOT NULL,
            commit_sha TEXT,
            files_changed INTEGER NOT NULL DEFAULT 0,
            state TEXT NOT NULL DEFAULT 'finalizing'
                CHECK (state IN ('finalizing','ready','merged','discarded')),
            created_at INTEGER NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_artifacts_run ON artifacts(session_id, run_id);
        -- 幂等（review 折入·codex#6/opus）：同一 member 同一 run 只一条 artifact·重复 finalize 命中既有。
        CREATE UNIQUE INDEX IF NOT EXISTS idx_artifacts_member ON artifacts(session_id, run_id, member_assignment_id);
        CREATE TABLE IF NOT EXISTS verifications (
            id TEXT PRIMARY KEY,
            artifact_id TEXT NOT NULL,
            cmd TEXT NOT NULL,
            artifact_sha TEXT NOT NULL,
            exit_code INTEGER,
            output_ref TEXT,
            verdict TEXT NOT NULL DEFAULT 'pending'
                CHECK (verdict IN ('pending','passed','failed')),
            created_at INTEGER NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_verifications_artifact ON verifications(artifact_id);
        CREATE TABLE IF NOT EXISTS reviews (
            id TEXT PRIMARY KEY,
            artifact_id TEXT NOT NULL,
            reviewer_agent TEXT NOT NULL,
            advisory INTEGER NOT NULL DEFAULT 1,
            verdict TEXT NOT NULL DEFAULT 'pending'
                CHECK (verdict IN ('pending','pass','fail')),
            notes TEXT,
            created_at INTEGER NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_reviews_artifact ON reviews(artifact_id);
        CREATE TABLE IF NOT EXISTS merge_candidates (
            id TEXT PRIMARY KEY,
            artifact_id TEXT NOT NULL UNIQUE,
            staging_branch TEXT NOT NULL,
            state TEXT NOT NULL DEFAULT 'pending'
                CHECK (state IN ('pending','merged','rejected')),
            merged_sha TEXT,
            created_at INTEGER NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_merge_candidates_artifact ON merge_candidates(artifact_id);
        CREATE TABLE IF NOT EXISTS landing_commits (
            id TEXT PRIMARY KEY,
            session_id TEXT NOT NULL,
            run_id TEXT NOT NULL,
            artifact_id TEXT,
            pre_head TEXT NOT NULL,
            landed_head TEXT NOT NULL,
            commit_count INTEGER NOT NULL DEFAULT 0,
            files_changed INTEGER NOT NULL DEFAULT 0,
            insertions INTEGER NOT NULL DEFAULT 0,
            deletions INTEGER NOT NULL DEFAULT 0,
            created_at INTEGER NOT NULL,
            UNIQUE(session_id, run_id, landed_head)
        );
        CREATE INDEX IF NOT EXISTS idx_landing_commits_session ON landing_commits(session_id, id);",
    )?;
    Ok(())
}

fn apply_agent_memory(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "-- Agent Team M1a（缝5·§三.1）：run 级目标契约。只 draft/frozen 两态——
        -- M1a 的 frozen 是假冻结、不引状态机；真冻结 = Plan&Acceptance Gate 归 M2。
        CREATE TABLE IF NOT EXISTS goal_contracts (
            id TEXT PRIMARY KEY,
            session_id TEXT NOT NULL,
            run_id TEXT NOT NULL UNIQUE,
            goal TEXT NOT NULL,
            lead_participant_id TEXT NOT NULL,
            status TEXT NOT NULL DEFAULT 'draft' CHECK (status IN ('draft', 'frozen')),
            assignments_json TEXT NOT NULL DEFAULT '[]',
            created_at INTEGER NOT NULL,
            goal_title TEXT
        );

        -- Agent Team M1a（缝5·§三.2）：验收标准 day-1 存住（claim/verifier/evidence/status/scope）。
        -- contract_id 关联 goal_contracts；scope 区分整 team 的(run) vs 单任务的(task)。
        -- 本里程碑只存取，不跑验证、不做 roll-up（M2/M3/期2）。
        CREATE TABLE IF NOT EXISTS acceptance_criteria (
            id TEXT PRIMARY KEY,
            session_id TEXT NOT NULL,
            run_id TEXT NOT NULL,
            task_id TEXT NOT NULL,
            contract_id TEXT,
            scope TEXT NOT NULL DEFAULT 'task' CHECK (scope IN ('run', 'task')),
            claim TEXT NOT NULL,
            verifier TEXT,
            evidence TEXT,
            status TEXT NOT NULL DEFAULT 'pending'
                CHECK (status IN ('pending', 'passed', 'failed', 'waived')),
            waiver TEXT,
            created_at INTEGER NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_acceptance_run ON acceptance_criteria(session_id, run_id);
        CREATE TABLE IF NOT EXISTS agents (
            id TEXT NOT NULL PRIMARY KEY,
            name TEXT NOT NULL,
            access TEXT NOT NULL
                CHECK (access IN ('native', 'borrow', 'harness')),
            provider TEXT NOT NULL,
            primary_model TEXT,
            endpoint TEXT,
            auth_mode TEXT
                CHECK (auth_mode IS NULL OR auth_mode IN ('bearer', 'x_api_key')),
            model_opus TEXT,
            model_sonnet TEXT,
            model_haiku TEXT,
            model_subagent TEXT,
            reasoning_default TEXT NOT NULL DEFAULT 'auto'
                CHECK (reasoning_default IN ('auto', 'none', 'minimal', 'low', 'medium', 'high', 'xhigh', 'max')),
            max_output_tokens INTEGER,
            api_timeout_ms INTEGER,
            compat_disable_betas INTEGER NOT NULL DEFAULT 0,
            compat_disable_nonessential INTEGER NOT NULL DEFAULT 0,
            compat_disable_thinking INTEGER NOT NULL DEFAULT 0,
            compat_proxy TEXT,
            custom_headers TEXT,
            extra_body TEXT,
            cap_reasoning TEXT,
            cap_computer_use TEXT,
            cap_lead TEXT,
            has_key INTEGER NOT NULL DEFAULT 0,
            is_builtin INTEGER NOT NULL DEFAULT 0,
            enabled INTEGER NOT NULL DEFAULT 1,
            sort_order INTEGER NOT NULL DEFAULT 0,
            created_at INTEGER NOT NULL,
            updated_at INTEGER NOT NULL
        );
        CREATE TABLE IF NOT EXISTS memory_blocks (
            session_id TEXT NOT NULL,
            slot TEXT NOT NULL,
            text TEXT NOT NULL,
            title TEXT,
            anchor_refs_json TEXT NOT NULL DEFAULT '[]',
            updated_by TEXT,
            updated_at INTEGER NOT NULL,
            revision INTEGER NOT NULL DEFAULT 0,
            updated_run_id TEXT,
            PRIMARY KEY (session_id, slot)
        );",
    )?;
    Ok(())
}

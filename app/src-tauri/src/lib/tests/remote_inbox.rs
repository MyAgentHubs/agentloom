#![cfg(test)]

use super::*;

struct RemoteInputSendTestHarness {
    conn: std::cell::RefCell<Connection>,
    busy: std::cell::Cell<bool>,
    delivery_calls: std::cell::Cell<usize>,
    delivered_texts: std::cell::RefCell<Vec<String>>,
}

impl RemoteInputSendTestHarness {
    fn new(busy: bool) -> Self {
        Self {
            conn: std::cell::RefCell::new(crate::test_support::mem_db()),
            busy: std::cell::Cell::new(busy),
            delivery_calls: std::cell::Cell::new(0),
            delivered_texts: std::cell::RefCell::new(Vec::new()),
        }
    }

    fn send(
        &self,
        session_id: &str,
        command_id: &str,
        text: &str,
    ) -> Option<remote_gateway::AckOutcome> {
        let payload = serde_json::json!({"text": text}).to_string();
        remote_input_send_ack(
            || {
                db::enqueue_remote_input(
                    &self.conn.borrow(),
                    session_id,
                    command_id,
                    "input.send",
                    &payload,
                )
                .map_err(|e| e.to_string())
            },
            || {
                drain_remote_inbox_loop(
                    || {
                        db::next_pending_remote_input(&self.conn.borrow(), session_id)
                            .unwrap()
                            .map(|entry| (entry.id, entry.command_id, entry.kind, entry.payload))
                    },
                    |kind, payload, _command_id| {
                        if self.busy.get() {
                            return Err(format!("SESSION_ALREADY_RUNNING: {session_id}"));
                        }
                        let text = parse_remote_input(kind, payload)?;
                        self.delivery_calls.set(self.delivery_calls.get() + 1);
                        self.delivered_texts.borrow_mut().push(text);
                        Ok(())
                    },
                    |id, _command_id| {
                        db::mark_remote_input_delivered(&self.conn.borrow(), id).is_ok()
                    },
                    |id, _command_id, error| {
                        db::record_remote_input_failure(&self.conn.borrow(), id, error).ok()
                    },
                    |id, _command_id, error| {
                        db::mark_remote_input_failed(&self.conn.borrow(), id, error).is_ok()
                    },
                );
            },
            || {
                db::remote_inbox_terminal_state_by_command_id(&self.conn.borrow(), command_id)
                    .map_err(|e| e.to_string())
            },
        )
    }
}

#[test]
fn remote_input_send_enqueue_error_returns_no_ack_without_drain() {
    let drain_calls = std::sync::atomic::AtomicU64::new(0);

    let outcome = remote_input_send_ack(
        || Err("boom".to_string()),
        || {
            drain_calls.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        },
        || panic!("enqueue 失败后不得查询终态"),
    );

    assert_eq!(outcome, None);
    assert_eq!(drain_calls.load(std::sync::atomic::Ordering::Relaxed), 0);
}

#[test]
fn remote_input_send_same_frame_queues_after_enqueue_recovers() {
    let conn = crate::test_support::mem_db();
    let session_id = "s-enqueue-recovery";
    let command_id = "cmd-enqueue-recovery";
    let payload = serde_json::json!({"text": "retry"}).to_string();

    let first = remote_input_send_ack(
        || Err("boom".to_string()),
        || panic!("enqueue 失败后不得排空"),
        || panic!("enqueue 失败后不得查询终态"),
    );
    assert_eq!(first, None);
    let count_after_failure: i64 = conn
        .query_row("SELECT COUNT(*) FROM remote_inbox", [], |row| row.get(0))
        .unwrap();
    assert_eq!(count_after_failure, 0);

    let second = remote_input_send_ack(
        || {
            db::enqueue_remote_input(&conn, session_id, command_id, "input.send", &payload)
                .map_err(|e| e.to_string())
        },
        || {},
        || panic!("新插入行不应查询终态"),
    );
    assert_eq!(second, Some(remote_gateway::AckOutcome::Queued));
    let inserted: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM remote_inbox WHERE command_id = ?1",
            [command_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(inserted, 1);
}

#[test]
fn remote_input_send_idle_drains_once_returns_queued_and_marks_delivered() {
    let harness = RemoteInputSendTestHarness::new(false);

    // 生产 drain 是异步线程；此处用同步闭包测试纯决策逻辑。新行仍回 queued，
    // delivered_at 已落库只是同步 drain 的副作用，不代表 ack 承诺投递完成。
    assert_eq!(
        harness.send("s-input-idle", "cmd-input-idle", "idle"),
        Some(remote_gateway::AckOutcome::Queued)
    );
    assert_eq!(harness.delivery_calls.get(), 1);
    let delivered_at: Option<i64> = harness
        .conn
        .borrow()
        .query_row(
            "SELECT delivered_at FROM remote_inbox WHERE command_id = 'cmd-input-idle'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(delivered_at.is_some());
}

#[test]
fn remote_input_send_ack_loss_retry_first_queued_then_ok_without_redelivery() {
    let harness = RemoteInputSendTestHarness::new(false);

    assert_eq!(
        harness.send("s-input-retry", "cmd-input-retry", "once"),
        Some(remote_gateway::AckOutcome::Queued)
    );
    assert_eq!(
        harness.send("s-input-retry", "cmd-input-retry", "duplicate"),
        Some(remote_gateway::AckOutcome::Ok)
    );
    assert_eq!(harness.delivery_calls.get(), 1);
    assert_eq!(harness.delivered_texts.borrow().as_slice(), ["once"]);
}

#[test]
fn remote_input_send_busy_retry_stays_queued_with_one_ledger_row() {
    let harness = RemoteInputSendTestHarness::new(true);

    assert_eq!(
        harness.send("s-input-busy", "cmd-input-busy", "busy"),
        Some(remote_gateway::AckOutcome::Queued)
    );
    assert_eq!(
        harness.send("s-input-busy", "cmd-input-busy", "duplicate"),
        Some(remote_gateway::AckOutcome::Queued)
    );
    assert_eq!(harness.delivery_calls.get(), 0);
    let count: i64 = harness
        .conn
        .borrow()
        .query_row(
            "SELECT COUNT(*) FROM remote_inbox WHERE command_id = 'cmd-input-busy'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 1);
}

#[test]
fn remote_input_send_new_idle_command_drains_older_pending_entry_first() {
    let harness = RemoteInputSendTestHarness::new(true);
    assert!(db::enqueue_remote_input(
        &harness.conn.borrow(),
        "s-input-fifo",
        "cmd-input-old",
        "input.send",
        &serde_json::json!({"text": "old"}).to_string(),
    )
    .unwrap());
    harness.busy.set(false);

    assert_eq!(
        harness.send("s-input-fifo", "cmd-input-new", "new"),
        Some(remote_gateway::AckOutcome::Queued)
    );
    assert_eq!(
        harness.delivered_texts.borrow().as_slice(),
        ["old", "new"],
        "即时排空必须复用 FIFO，后到的新行不得越过历史 pending 行"
    );
}

#[test]
fn remote_input_send_failed_duplicate_returns_failed_without_delivery() {
    let harness = RemoteInputSendTestHarness::new(false);
    assert!(db::enqueue_remote_input(
        &harness.conn.borrow(),
        "s-input-failed",
        "cmd-input-failed",
        "input.send",
        "{}",
    )
    .unwrap());
    let entry = db::next_pending_remote_input(&harness.conn.borrow(), "s-input-failed")
        .unwrap()
        .unwrap();
    db::mark_remote_input_failed(&harness.conn.borrow(), entry.id, "TEST_FAILED").unwrap();

    assert_eq!(
        harness.send("s-input-failed", "cmd-input-failed", "duplicate"),
        Some(remote_gateway::AckOutcome::Failed)
    );
    assert_eq!(harness.delivery_calls.get(), 0);
}

#[test]
fn drain_remote_inbox_loop_stops_without_marking_delivered_on_busy() {
    use std::collections::VecDeque;
    let mut pending: VecDeque<(i64, String, String, String)> = VecDeque::from([
        (
            1,
            "cmd-a".to_string(),
            "input.send".to_string(),
            "{\"text\":\"a\"}".to_string(),
        ),
        (
            2,
            "cmd-b".to_string(),
            "input.send".to_string(),
            "{\"text\":\"b\"}".to_string(),
        ),
    ]);
    let mut delivered_ids: Vec<i64> = Vec::new();
    let mut delivery_calls = 0;
    drain_remote_inbox_loop(
        || pending.pop_front(),
        |_kind, _payload, _command_id| {
            delivery_calls += 1;
            Err("SESSION_ALREADY_RUNNING: s-x".to_string())
        },
        |id, _command_id| {
            delivered_ids.push(id);
            true
        },
        |_id, _command_id, _error| panic!("busy 不得记录失败次数"),
        |_id, _command_id, _error| panic!("busy 不得标失败终态"),
    );
    assert_eq!(delivery_calls, 1, "撞忙应在第一条投递后立即停");
    assert!(delivered_ids.is_empty(), "撞忙即停：零条 mark_delivered");
    assert_eq!(pending.len(), 1, "第二条应原样留在队列里，等下次释放");
}

#[test]
fn drain_remote_inbox_loop_drains_fifo_and_skips_non_busy_failures_without_blocking() {
    use std::collections::VecDeque;
    let mut pending: VecDeque<(i64, String, String, String)> = VecDeque::from([
        (
            1,
            "cmd-a".to_string(),
            "input.send".to_string(),
            "{\"text\":\"a\"}".to_string(),
        ),
        (
            2,
            "cmd-bogus".to_string(),
            "control.bogus".to_string(),
            "{}".to_string(),
        ),
        (
            3,
            "cmd-c".to_string(),
            "input.send".to_string(),
            "{\"text\":\"c\"}".to_string(),
        ),
    ]);
    let mut delivered_ids: Vec<i64> = Vec::new();
    let mut failed_ids: Vec<i64> = Vec::new();
    let mut seen_kinds: Vec<String> = Vec::new();
    drain_remote_inbox_loop(
        || pending.pop_front(),
        |kind, _payload, _command_id| {
            seen_kinds.push(kind.to_string());
            if kind == "input.send" {
                Ok(())
            } else {
                Err(format!("UNKNOWN_REMOTE_INBOX_KIND:{kind}"))
            }
        },
        |id, _command_id| {
            delivered_ids.push(id);
            true
        },
        |_id, _command_id, _error| panic!("parse 失败不得累计投递 attempts"),
        |id, _command_id, _error| {
            failed_ids.push(id);
            true
        },
    );
    assert_eq!(
        delivered_ids,
        vec![1, 3],
        "只有真正投递成功的条目才能 mark_delivered"
    );
    assert_eq!(failed_ids, vec![2], "未知 kind 必须标 failed 终态");
    assert_eq!(
        seen_kinds,
        vec!["input.send", "control.bogus", "input.send"]
    );
}

#[test]
fn drain_remote_inbox_loop_retries_delivery_failure_then_marks_third_failure_terminal() {
    use std::cell::Cell;

    let attempts = Cell::new(0_i64);
    let terminal = Cell::new(false);
    let delivery_calls = Cell::new(0);
    let marked_failed = Cell::new(0);
    let following_delivered = Cell::new(false);

    for expected_attempts in 1..=3 {
        drain_remote_inbox_loop(
            || {
                if !terminal.get() {
                    Some((
                        1,
                        "cmd-retry".to_string(),
                        "input.send".to_string(),
                        "{\"text\":\"retry\"}".to_string(),
                    ))
                } else if !following_delivered.get() {
                    Some((
                        2,
                        "cmd-after".to_string(),
                        "input.send".to_string(),
                        "{\"text\":\"after\"}".to_string(),
                    ))
                } else {
                    None
                }
            },
            |_kind, payload, _command_id| {
                delivery_calls.set(delivery_calls.get() + 1);
                if payload.contains("retry") {
                    Err("AGENT_NOT_FOUND".to_string())
                } else {
                    Ok(())
                }
            },
            |id, command_id| {
                assert_eq!((id, command_id), (2, "cmd-after"));
                following_delivered.set(true);
                true
            },
            |_id, _command_id, error| {
                assert_eq!(error, "AGENT_NOT_FOUND");
                attempts.set(attempts.get() + 1);
                Some(attempts.get())
            },
            |_id, _command_id, error| {
                assert_eq!(error, "AGENT_NOT_FOUND");
                marked_failed.set(marked_failed.get() + 1);
                terminal.set(true);
                true
            },
        );
        assert_eq!(attempts.get(), expected_attempts);
        assert_eq!(
            terminal.get(),
            expected_attempts >= 3,
            "前两次保留 pending，第三次标失败终态"
        );
        assert_eq!(
            following_delivered.get(),
            expected_attempts >= 3,
            "前两次不得越过失败条目；第三次终态化后应继续后续 FIFO 条目"
        );
    }

    assert_eq!(delivery_calls.get(), 4, "第三轮还应投递终态条目之后的一条");
    assert_eq!(marked_failed.get(), 1, "仅第三次失败标终态");
}

#[test]
fn drain_remote_inbox_loop_stops_when_record_failure_write_fails() {
    let mut next_calls = 0;
    let mut delivery_calls = 0;
    drain_remote_inbox_loop(
        || {
            next_calls += 1;
            Some((
                next_calls,
                format!("cmd-{next_calls}"),
                "input.send".to_string(),
                "{\"text\":\"x\"}".to_string(),
            ))
        },
        |_kind, _payload, _command_id| {
            delivery_calls += 1;
            Err("AGENT_NOT_FOUND".to_string())
        },
        |_id, _command_id| panic!("投递失败不得 mark_delivered"),
        |_id, _command_id, _error| None,
        |_id, _command_id, _error| panic!("计数写失败不得继续标终态"),
    );
    assert_eq!(delivery_calls, 1, "计数写失败安全阀应立即停");
    assert_eq!(next_calls, 1, "计数写失败后不得再取下一条");
}

#[test]
fn drain_remote_inbox_loop_stops_after_mark_delivered_failure() {
    use std::collections::VecDeque;
    let mut pending: VecDeque<(i64, String, String, String)> = VecDeque::from([
        (
            1,
            "cmd-a".to_string(),
            "input.send".to_string(),
            "{\"text\":\"a\"}".to_string(),
        ),
        (
            2,
            "cmd-b".to_string(),
            "input.send".to_string(),
            "{\"text\":\"b\"}".to_string(),
        ),
    ]);
    let mut delivery_calls = 0;
    drain_remote_inbox_loop(
        || pending.pop_front(),
        |_kind, _payload, _command_id| {
            delivery_calls += 1;
            Ok(())
        },
        |_id, _command_id| false,
        |_id, _command_id, _error| panic!("成功分支不得 record_failure"),
        |_id, _command_id, _error| panic!("成功分支不得 mark_failed"),
    );
    assert_eq!(delivery_calls, 1, "mark 失败后不得继续投递第二条");
}

#[test]
fn drain_remote_inbox_loop_stops_before_redelivering_same_id() {
    let mut next_pending_calls = 0;
    let mut delivery_calls = 0;
    drain_remote_inbox_loop(
        || {
            next_pending_calls += 1;
            Some((
                1,
                "cmd-x".to_string(),
                "input.send".to_string(),
                "{\"text\":\"x\"}".to_string(),
            ))
        },
        |_kind, _payload, _command_id| {
            delivery_calls += 1;
            Ok(())
        },
        |_id, _command_id| true,
        |_id, _command_id, _error| panic!("成功分支不得 record_failure"),
        |_id, _command_id, _error| panic!("成功分支不得 mark_failed"),
    );
    assert!(
        next_pending_calls <= 2,
        "同 id 第二次出现时必须停止查询循环"
    );
    assert_eq!(delivery_calls, 1, "同一条不得被二次投递");
}

#[test]
fn remote_inbox_emit_loads_new_solo_user_message_even_when_delivery_fails() {
    let source = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/lib.rs"));
    let production = source.split("\n#[cfg(test)]\nmod tests {").next().unwrap();
    let delivery_body = production
        .split("fn deliver_remote_inbox_entry(")
        .nth(1)
        .unwrap()
        .split("\nconst RESUME_WITHOUT_MESSAGE_FALLBACK_PROMPT")
        .next()
        .unwrap();
    let emit_call =
        "emit_remote_inbox_message_if_new(app, session_id, &dedup_key, existed_before);";
    assert_eq!(
        delivery_body.matches(emit_call).count(),
        2,
        "team/solo 两条投递路由都必须尝试回显"
    );
    assert!(
        delivery_body.contains(&format!("{emit_call}\n        return result;")),
        "team 路由必须在无条件尝试回显后原样返回投递结果"
    );
    assert!(
        delivery_body.contains(&format!("{emit_call}\n    result\n}}")),
        "solo 路由必须在无条件尝试回显后原样返回投递结果"
    );

    let conn = crate::test_support::mem_db();
    let session_id = "s-remote-emit-solo";
    let dedup_key = "remote_input:cmd-remote-emit-solo";

    let existed_before = db::get_message_by_session_and_dedup_key(&conn, session_id, dedup_key)
        .unwrap()
        .is_some();
    assert!(!existed_before);
    assert!(db::append_message_dedup_and_publish(
        &conn,
        session_id,
        "user",
        &[Block::Text {
            text: "手机发来的消息".to_string(),
        }],
        None,
        Some("solo-agent"),
        Some("Solo Agent"),
        dedup_key,
    )
    .unwrap());

    // user 消息已在 append 阶段落库，但后续 run ledger/spawn 失败：回显判据仍应只看
    // existed_before + 查回结果，不与投递整体的 Err 绑定。
    let delivery_result: Result<(), String> = Err("spawn failed".to_string());
    assert!(delivery_result.is_err());
    let message = remote_inbox_message_to_emit(&conn, session_id, dedup_key, existed_before)
        .unwrap()
        .expect("投递失败时，本次新落库的 remote user 消息仍必须可供外层 emit");
    assert_eq!(message.role, "user");
    assert_eq!(message.agent_id.as_deref(), Some("solo-agent"));
    assert_eq!(message.agent_name_snapshot.as_deref(), Some("Solo Agent"));
    assert_eq!(
        message.content,
        vec![Block::Text {
            text: "手机发来的消息".to_string(),
        }]
    );
    let stored_dedup_key: String = conn
        .query_row(
            "SELECT dedup_key FROM messages WHERE id = ?1",
            [message.id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(stored_dedup_key, dedup_key);
}

#[test]
fn remote_inbox_emit_duplicate_delivery_returns_none() {
    let conn = crate::test_support::mem_db();
    let session_id = "s-remote-emit-duplicate";
    let dedup_key = "remote_input:cmd-remote-emit-duplicate";

    assert!(db::append_message_dedup_and_publish(
        &conn,
        session_id,
        "user",
        &[Block::Text {
            text: "只显示一次".to_string(),
        }],
        None,
        None,
        None,
        dedup_key,
    )
    .unwrap());
    let existed_before = db::get_message_by_session_and_dedup_key(&conn, session_id, dedup_key)
        .unwrap()
        .is_some();
    assert!(existed_before);
    assert!(!db::append_message_dedup_and_publish(
        &conn,
        session_id,
        "user",
        &[Block::Text {
            text: "只显示一次".to_string(),
        }],
        None,
        None,
        None,
        dedup_key,
    )
    .unwrap());

    assert_eq!(
        remote_inbox_message_to_emit(&conn, session_id, dedup_key, existed_before,).unwrap(),
        None,
        "同 command_id 重投命中既有行时不得产生第二次 emit 语义"
    );
    assert_eq!(db::get_messages(&conn, session_id).unwrap().len(), 1);
}

#[test]
fn remote_inbox_redelivery_before_mark_delivered_dedupes_via_command_id_key() {
    // P0-c 步骤 9：at-least-once 窗口——这是 `remote_input_key(command_id)` 存在的理由。
    // 第一轮：deliver 真落库成功，但 mark_delivered 模拟落库失败（崩溃/断连场景）——
    // `drain_remote_inbox_loop` 的既有安全阀让循环立即停，remote_inbox 那一行仍是 pending。
    // 第二轮：模拟进程重启/重连后再次排空——next_pending 找到同一条仍 pending 的行（同
    // command_id），deliver 被真实地再调用一次；这次是同一把 `remote_input_key(command_id)`
    // 键，INSERT OR IGNORE 挡下第二次落库，也不重复发 msg.completed 里程碑（publish 只发生
    // 在真插入那次）。这是 command_id 穿线（而非退回 None 键、每次现场生成 run_id）的直接
    // 证据：若穿线断开，两轮会各自派生不同 dedup_key，messages 表会落 2 行、publish 2 次。
    //
    // 装配边界（P0-c 返工·测试硬度钉①）：下面的 deliver 闭包没有调用生产
    // `deliver_remote_inbox_entry`——那个函数第一行就要 `app.state::<Db>()`，真调用它需要
    // 整套 Tauri `AppHandle`/`Db`/`Running`/`member_runner::TeamRunning` 装配，本仓无
    // `tauri::test` mock 装配、加这套装配属新基建、超出本轮 clean 单任务范围。这里因此只能
    // 手写调用它「内部能到的最深生产层」——`parse_remote_input` +
    // `db::append_message_dedup_and_publish`——去验证「同 command_id 键两轮重投确实
    // 去重」这条 DB 层行为；但这样绕过了 `deliver_remote_inbox_entry` 本身「把 command_id
    // 传给 send_message 最后一参」那行源码，验证不了它被改掉（比如 solo 路误传 None）的
    // 回归。这个穿线断裂的缺口由同 mod 内的
    // `deliver_remote_inbox_entry_solo_route_threads_command_id_dedup_key`（源码字面匹配）
    // 补上——两个测试合起来才是「重投测试」对 command_id 穿线的完整覆盖。
    let conn = crate::test_support::mem_db();
    let session_id = "s-redeliver";
    let command_id = "cmd-redeliver";
    let payload = serde_json::json!({"text": "重投测试"}).to_string();
    assert!(
        db::enqueue_remote_input(&conn, session_id, command_id, "input.send", &payload).unwrap()
    );

    crate::remote_gateway::test_take_publish_log();
    let mut deliver_calls = 0;

    // 第一轮：deliver 成功，但 mark_delivered 模拟落库失败——安全阀停，行仍 pending。
    drain_remote_inbox_loop(
        || {
            db::next_pending_remote_input(&conn, session_id)
                .unwrap()
                .map(|e| (e.id, e.command_id, e.kind, e.payload))
        },
        |kind, payload, command_id| {
            deliver_calls += 1;
            let text = parse_remote_input(kind, payload)?;
            db::append_message_dedup_and_publish(
                &conn,
                session_id,
                "user",
                &[Block::Text { text }],
                None,
                None,
                None,
                &display_reduce::remote_input_key(command_id),
            )
            .map_err(|e| e.to_string())?;
            Ok(())
        },
        |_id, _command_id| false, // 模拟 mark_delivered 落库失败
        |_id, _command_id, _error| panic!("成功分支不得 record_failure"),
        |_id, _command_id, _error| panic!("成功分支不得 mark_failed"),
    );

    // 第二轮：同一条仍 pending 的行被再次取到、再次投递（真实 at-least-once 重投）；这次
    // mark_delivered 真正成功，收尾这条。
    drain_remote_inbox_loop(
        || {
            db::next_pending_remote_input(&conn, session_id)
                .unwrap()
                .map(|e| (e.id, e.command_id, e.kind, e.payload))
        },
        |kind, payload, command_id| {
            deliver_calls += 1;
            let text = parse_remote_input(kind, payload)?;
            db::append_message_dedup_and_publish(
                &conn,
                session_id,
                "user",
                &[Block::Text { text }],
                None,
                None,
                None,
                &display_reduce::remote_input_key(command_id),
            )
            .map_err(|e| e.to_string())?;
            Ok(())
        },
        |id, _command_id| db::mark_remote_input_delivered(&conn, id).is_ok(),
        |_id, _command_id, _error| panic!("成功分支不得 record_failure"),
        |_id, _command_id, _error| panic!("成功分支不得 mark_failed"),
    );

    assert_eq!(
        deliver_calls, 2,
        "两轮各投递一次（真实 at-least-once 重投）"
    );
    let messages = db::get_messages(&conn, session_id).unwrap();
    assert_eq!(
        messages.len(),
        1,
        "同 command_id 重投不得产生第二条落库消息: {messages:?}"
    );
    assert_eq!(
        crate::remote_gateway::test_take_publish_log(),
        vec!["msg.completed"],
        "同 command_id 重投不得重复发布 msg.completed 里程碑"
    );
    // P0-c 返工（测试硬度钉②-b）：remote 路 dedup_key 字面断言——硬编码
    // `remote_input:{command_id}`，不复算 `display_reduce::remote_input_key`，防「键工厂
    // 改常量」类变异。
    let dedup_key: String = conn
        .query_row(
            "SELECT dedup_key FROM messages WHERE session_id = ?1",
            [session_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        dedup_key, "remote_input:cmd-redeliver",
        "重投落库行 dedup_key 必须字面等于 remote_input:{{command_id}}"
    );
}

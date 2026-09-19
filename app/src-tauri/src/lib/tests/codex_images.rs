#![cfg(test)]

use super::*;

fn set_test_modified(path: &std::path::Path, modified: std::time::SystemTime) {
    let file = std::fs::OpenOptions::new().write(true).open(path).unwrap();
    file.set_times(std::fs::FileTimes::new().set_modified(modified))
        .unwrap();
}

#[test]
fn codex_image_scan_missing_dir_is_empty() {
    let tmp = tempfile::tempdir().unwrap();
    assert!(scan_new_images(&tmp.path().join("missing"), std::time::UNIX_EPOCH).is_empty());
}

#[test]
fn codex_image_scan_filters_mtime_and_extensions() {
    let tmp = tempfile::tempdir().unwrap();
    let since = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000);
    let old_png = tmp.path().join("old.png");
    let new_png = tmp.path().join("new.PNG");
    let new_jpg = tmp.path().join("new.jpg");
    let new_txt = tmp.path().join("new.txt");
    for path in [&old_png, &new_png, &new_jpg, &new_txt] {
        std::fs::write(path, b"test").unwrap();
    }
    set_test_modified(&old_png, since - std::time::Duration::from_secs(1));
    for path in [&new_png, &new_jpg, &new_txt] {
        set_test_modified(path, since);
    }

    assert_eq!(scan_new_images(tmp.path(), since), vec![new_png, new_jpg]);
}

#[test]
fn codex_image_scan_sorts_and_caps_at_twenty() {
    let tmp = tempfile::tempdir().unwrap();
    for index in (0..25).rev() {
        std::fs::write(tmp.path().join(format!("image-{index:02}.webp")), b"test").unwrap();
    }

    let images = scan_new_images(tmp.path(), std::time::UNIX_EPOCH);
    assert_eq!(images.len(), 20);
    assert_eq!(images[0], tmp.path().join("image-00.webp"));
    assert_eq!(images[19], tmp.path().join("image-19.webp"));
}

#[test]
fn codex_image_thread_id_is_captured_only_for_codex_runs() {
    let events =
        agent_event::parse_codex_line(r#"{"type":"thread.started","thread_id":"thread-123"}"#);
    let event = events.first().expect("thread.started should parse");
    assert_eq!(
        codex_thread_id_from_event(ParseFn::Codex, event),
        Some("thread-123")
    );
    assert_eq!(codex_thread_id_from_event(ParseFn::Claude, event), None);
}

#[test]
fn codex_image_tool_events_form_a_persistable_compact_tool_block() {
    let images = vec![
        std::path::PathBuf::from("/tmp/generated/one.png"),
        std::path::PathBuf::from("/tmp/generated/two.webp"),
    ];
    let events = codex_image_tool_events("run-image", &images);
    let mut reducer = display_reduce::DisplayReducer::new("run-image");
    for event in &events {
        reducer.feed(event);
    }
    reducer.feed(&agent_event::AgentEvent::Completed {
        cost_usd: None,
        input_tokens: None,
        output_tokens: None,
        final_text: None,
        result: None,
        run_id: None,
        commit_sha: None,
        files_changed: None,
        insertions: None,
        deletions: None,
        interrupted: None,
    });

    let message = reducer
        .finish(&base_run_outcome("run-image"))
        .expect("image tool block should be persisted");
    assert!(message.blocks.iter().any(|block| matches!(
        block,
        Block::Tool {
            tool,
            card: db::BlockCardKind::Compact,
            status: db::BlockToolStatus::Ok,
            output: Some(output),
            ..
        } if tool == "image_gen"
            && output == "/tmp/generated/one.png\n/tmp/generated/two.webp"
    )));
}

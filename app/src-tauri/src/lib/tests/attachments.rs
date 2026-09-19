#![cfg(test)]

use super::*;

#[test]
fn app_info_contains_product_name() {
    assert!(app_info().contains("AgentLoom"));
}

#[test]
fn write_text_file_writes_md_and_rejects_other_ext() {
    let dir = std::env::temp_dir().join(format!("agentloom-wtf-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join("x.md");
    write_text_file(p.to_string_lossy().to_string(), "hi".into()).unwrap();
    assert_eq!(std::fs::read_to_string(&p).unwrap(), "hi");

    let bad = dir.join("x.txt");
    assert_eq!(
        write_text_file(bad.to_string_lossy().to_string(), "hi".into()).unwrap_err(),
        "AL_ERR:file.markdownOnly"
    );
}

#[test]
fn write_temp_html_returns_existing_path() {
    let p = write_temp_html("<!doctype html><html></html>".into()).unwrap();
    assert!(std::path::Path::new(&p).exists());
    assert!(p.ends_with(".html"));
    assert_eq!(
        std::fs::read_to_string(&p).unwrap(),
        "<!doctype html><html></html>"
    );
}

#[test]
fn save_pasted_image_writes_png_to_app_directory() {
    let base = tempfile::tempdir().unwrap();
    let encoded = base64::engine::general_purpose::STANDARD.encode(b"\x89PNG\r\n\x1a\n");

    let saved = save_pasted_image_in(&encoded, "image/png", base.path()).unwrap();
    let path = std::path::PathBuf::from(saved);

    assert!(path.is_absolute());
    assert!(path.exists());
    assert_eq!(path.extension().and_then(|ext| ext.to_str()), Some("png"));
    assert!(path.starts_with(base.path().join(".agentloom").join("attachments")));
    assert_eq!(std::fs::read(path).unwrap(), b"\x89PNG\r\n\x1a\n");
}

#[test]
fn save_pasted_image_rejects_unsupported_media_type() {
    let base = tempfile::tempdir().unwrap();
    let encoded = base64::engine::general_purpose::STANDARD.encode(b"image");

    let error = save_pasted_image_in(&encoded, "image/bmp", base.path()).unwrap_err();

    assert!(error.contains("unsupported pasted image media type"));
}

#[test]
fn save_pasted_image_rejects_payload_over_ten_mb() {
    let base = tempfile::tempdir().unwrap();
    let encoded =
        base64::engine::general_purpose::STANDARD.encode(vec![0_u8; 10 * 1024 * 1024 + 1]);

    let error = save_pasted_image_in(&encoded, "image/png", base.path()).unwrap_err();

    assert_eq!(error, "pasted image exceeds 10 MB");
}

#[test]
fn save_pasted_image_rejects_invalid_base64() {
    let base = tempfile::tempdir().unwrap();

    let error = save_pasted_image_in("not base64!", "image/png", base.path()).unwrap_err();

    assert!(error.contains("invalid pasted image base64"));
}

#[test]
fn save_pasted_text_writes_content_byte_for_byte() {
    let base = tempfile::tempdir().unwrap();
    let content = "超长粘贴文本\nline two\n";

    let saved = save_pasted_text_in(content, base.path()).unwrap();
    let path = std::path::PathBuf::from(saved);

    assert!(path.is_absolute());
    assert!(path.exists());
    assert_eq!(path.extension().and_then(|ext| ext.to_str()), Some("txt"));
    assert!(path.starts_with(base.path().join(".agentloom").join("attachments")));
    assert_eq!(std::fs::read(&path).unwrap(), content.as_bytes());
}

#[test]
fn save_pasted_text_does_not_clobber_concurrent_pastes() {
    let base = tempfile::tempdir().unwrap();

    let first = save_pasted_text_in("first paste", base.path()).unwrap();
    let second = save_pasted_text_in("second paste", base.path()).unwrap();

    assert_ne!(first, second);
    assert_eq!(std::fs::read_to_string(&first).unwrap(), "first paste");
    assert_eq!(std::fs::read_to_string(&second).unwrap(), "second paste");
}

#[test]
fn read_attachment_reads_text_file() {
    let p = std::env::temp_dir().join(format!(
        "agentloom-read-attachment-text-{}.txt",
        std::process::id()
    ));
    std::fs::write(&p, "hello world").unwrap();

    let attachment = read_attachment_at(&p).unwrap();

    assert_eq!(attachment.kind, "text");
    assert_eq!(attachment.content, "hello world");
    assert!(!attachment.truncated);
    assert_eq!(attachment.image_base64, None);
    assert_eq!(attachment.media_type, None);
    let serialized = serde_json::to_value(&attachment).unwrap();
    assert!(serialized.get("imageBase64").is_none());
    assert!(serialized.get("mediaType").is_none());
    let _ = std::fs::remove_file(p);
}

#[test]
fn read_attachment_returns_png_bytes_and_media_type() {
    let p = std::env::temp_dir().join(format!(
        "agentloom-read-attachment-image-{}.png",
        std::process::id()
    ));
    std::fs::write(&p, b"\x89PNG\r\n\x1a\n").unwrap();

    let attachment = read_attachment_at(&p).unwrap();

    assert_eq!(attachment.kind, "image");
    assert_eq!(attachment.content, "");
    assert_eq!(attachment.image_base64.as_deref(), Some("iVBORw0KGgo="));
    assert_eq!(attachment.media_type.as_deref(), Some("image/png"));
    let _ = std::fs::remove_file(p);
}

#[test]
fn read_attachment_rejects_fake_image_bytes() {
    let p = std::env::temp_dir().join(format!(
        "agentloom-read-attachment-fake-image-{}.png",
        std::process::id()
    ));
    std::fs::write(&p, b"not really a png").unwrap();

    let attachment = read_attachment_at(&p).unwrap();

    assert_eq!(attachment.kind, "image");
    assert_eq!(attachment.image_base64, None);
    assert_eq!(attachment.media_type, None);
    let _ = std::fs::remove_file(p);
}

#[test]
fn read_attachment_sniffs_supported_image_media_types() {
    let cases: [(&str, &[u8], &str); 5] = [
        ("jpg", b"\xff\xd8\xffpayload", "image/jpeg"),
        ("gif", b"GIF87apayload", "image/gif"),
        ("gif", b"GIF89apayload", "image/gif"),
        ("webp", b"RIFF\x04\0\0\0WEBPpayload", "image/webp"),
        ("bmp", b"BMpayload", "image/bmp"),
    ];

    for (index, (ext, bytes, expected_media_type)) in cases.iter().enumerate() {
        let p = std::env::temp_dir().join(format!(
            "agentloom-read-attachment-magic-{}-{index}.{ext}",
            std::process::id()
        ));
        std::fs::write(&p, bytes).unwrap();

        let attachment = read_attachment_at(&p).unwrap();

        assert_eq!(attachment.kind, "image");
        assert!(attachment.image_base64.is_some());
        assert_eq!(attachment.media_type.as_deref(), Some(*expected_media_type));
        let _ = std::fs::remove_file(p);
    }
}

#[test]
fn read_attachment_returns_svg_bytes_without_magic_sniffing() {
    let p = std::env::temp_dir().join(format!(
        "agentloom-read-attachment-image-{}.svg",
        std::process::id()
    ));
    std::fs::write(&p, b"<svg xmlns=\"http://www.w3.org/2000/svg\"></svg>").unwrap();

    let attachment = read_attachment_at(&p).unwrap();

    assert_eq!(attachment.kind, "image");
    assert!(attachment.image_base64.is_some());
    assert_eq!(attachment.media_type.as_deref(), Some("image/svg+xml"));
    let _ = std::fs::remove_file(p);
}

#[test]
fn read_attachment_omits_bytes_for_oversized_image() {
    const MAX_IMAGE_BYTES: u64 = 10 * 1024 * 1024;
    let p = std::env::temp_dir().join(format!(
        "agentloom-read-attachment-large-image-{}.png",
        std::process::id()
    ));
    std::fs::write(&p, b"\x89PNG\r\n\x1a\n").unwrap();
    std::fs::OpenOptions::new()
        .write(true)
        .open(&p)
        .unwrap()
        .set_len(MAX_IMAGE_BYTES + 1)
        .unwrap();

    let attachment = read_attachment_at(&p).unwrap();

    assert_eq!(attachment.kind, "image");
    assert_eq!(attachment.byte_len, MAX_IMAGE_BYTES + 1);
    assert_eq!(attachment.image_base64, None);
    assert_eq!(attachment.media_type.as_deref(), Some("image/png"));
    let _ = std::fs::remove_file(p);
}

#[test]
fn read_attachment_rejects_missing_file() {
    let p = std::env::temp_dir().join(format!(
        "agentloom-read-attachment-missing-{}.txt",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&p);

    assert!(read_attachment_at(&p).is_err());
}

#[test]
fn open_attachment_accepts_html_path() {
    // open_attachment_external 只走 A（工作区/`~/.agentloom`）不享 B，
    // 所以这里必须给一个覆盖住 file 的 workspace base，而不是像之前那样传 None
    // 就能打开工作区外任意 html——那正是被返工修掉的边界不一致。
    let (_guard, root) = crate::test_support::tmp_root();
    let file = root.join("report.HTML");
    std::fs::write(&file, "<html></html>").unwrap();

    let resolved = resolve_open_attachment_path(file.to_str().unwrap(), Some(&root)).unwrap();

    assert_eq!(resolved, file.canonicalize().unwrap());
}

#[test]
fn open_attachment_rejects_non_html_path() {
    let (_guard, root) = crate::test_support::tmp_root();
    let file = root.join("report.txt");
    std::fs::write(&file, "not html").unwrap();

    let error = resolve_open_attachment_path(file.to_str().unwrap(), Some(&root)).unwrap_err();

    assert_eq!(error, "AL_ERR:file.htmlOnly");
}

#[test]
fn open_attachment_rejects_html_path_outside_workspace_even_without_session() {
    // 姊妹命令与 read_attachment 同款边界——base=None 时只放 `~/.agentloom`，
    // 工作区外的任意 html 一律拒（旧行为是完全不设防，任意路径只要是 .html 就放行）。
    let (_guard, root) = crate::test_support::tmp_root();
    let file = root.join("report.html");
    std::fs::write(&file, "<html></html>").unwrap();

    let error = resolve_open_attachment_path(file.to_str().unwrap(), None).unwrap_err();

    assert!(error.contains("outside the workspace"), "{error}");
}

#[test]
fn open_attachment_rejects_relative_path_outside_session_directory() {
    let (_guard, root) = crate::test_support::tmp_root();
    let workspace = root.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    std::fs::write(root.join("outside.html"), "<html></html>").unwrap();

    let error = resolve_open_attachment_path("../outside.html", Some(&workspace)).unwrap_err();

    assert!(error.contains("outside session directory"), "{error}");
}

#[test]
fn open_attachment_resolves_relative_path_in_session_directory() {
    let (_guard, root) = crate::test_support::tmp_root();
    let nested = root.join("artifacts");
    let file = nested.join("report.htm");
    std::fs::create_dir_all(&nested).unwrap();
    std::fs::write(&file, "<html></html>").unwrap();

    let resolved = resolve_open_attachment_path("artifacts/report.htm", Some(&root)).unwrap();

    assert_eq!(resolved, file.canonicalize().unwrap());
}

#[test]
fn resolve_attachment_path_preserves_absolute_path() {
    let (_guard, root) = crate::test_support::tmp_root();
    let base = root.join("workspace");
    let absolute = root.join("outside.txt");
    std::fs::create_dir_all(&base).unwrap();
    std::fs::write(&absolute, "outside").unwrap();

    assert_eq!(
        resolve_attachment_path(absolute.to_str().unwrap(), Some(&base)).unwrap(),
        absolute
    );
}

#[test]
fn resolve_attachment_path_expands_home() {
    let home = std::env::temp_dir().join(format!(
        "agentloom-resolve-attachment-home-{}",
        std::process::id()
    ));
    let original_home = std::env::var_os("HOME");
    std::env::set_var("HOME", &home);

    let resolved = resolve_attachment_path("~/foo.txt", None).unwrap();

    match original_home {
        Some(value) => std::env::set_var("HOME", value),
        None => std::env::remove_var("HOME"),
    }
    assert_eq!(resolved, home.join("foo.txt"));
}

#[test]
fn resolve_attachment_path_joins_relative_path_to_base() {
    let (_guard, root) = crate::test_support::tmp_root();
    let nested = root.join("a");
    let file = nested.join("b.txt");
    std::fs::create_dir_all(&nested).unwrap();
    std::fs::write(&file, "hello").unwrap();

    assert_eq!(
        resolve_attachment_path("a/b.txt", Some(&root)).unwrap(),
        file.canonicalize().unwrap()
    );
}

#[test]
fn resolve_attachment_path_finds_unique_basename_in_session_directory() {
    let (_guard, root) = crate::test_support::tmp_root();
    let nested = root.join("a");
    let file = nested.join("x.md");
    std::fs::create_dir_all(&nested).unwrap();
    std::fs::write(&file, "unique").unwrap();

    assert_eq!(
        resolve_attachment_path("x.md", Some(&root)).unwrap(),
        file.canonicalize().unwrap()
    );
}

#[test]
fn resolve_attachment_path_rejects_ambiguous_basename() {
    let (_guard, root) = crate::test_support::tmp_root();
    for directory in ["a", "b", "c"] {
        let nested = root.join(directory);
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(nested.join("x.md"), directory).unwrap();
    }

    let canonical_root = root.canonicalize().unwrap();
    let mut matches = Vec::new();
    let mut visited_entries = 0;
    let outcome = find_attachment_basename_matches(
        &canonical_root,
        std::ffi::OsStr::new("x.md"),
        &canonical_root,
        usize::MAX,
        &mut visited_entries,
        &mut matches,
    );

    assert_eq!(outcome, AttachmentBasenameSearchOutcome::Complete);
    assert_eq!(matches.len(), 2);
    assert_eq!(visited_entries, 4, "search must stop at the second match");

    let error = resolve_attachment_path("x.md", Some(&root)).unwrap_err();

    assert!(
        error.starts_with("AL_ERR:file.ambiguousBasename"),
        "{error}"
    );
    let params = error
        .strip_prefix("AL_ERR:file.ambiguousBasename:")
        .unwrap();
    let params: serde_json::Value = serde_json::from_str(params).unwrap();
    let candidates = params["1"].as_str().unwrap();
    assert_eq!(candidates.split(" · ").count(), 2, "{error}");
}

#[test]
fn resolve_attachment_path_returns_dedicated_error_when_basename_budget_is_exceeded() {
    let (_guard, root) = crate::test_support::tmp_root();
    let nested = root.join("only-entry");
    std::fs::create_dir_all(&nested).unwrap();
    std::fs::write(nested.join("x.md"), "would match without the budget").unwrap();

    let error = resolve_attachment_path_with_basename_budget("x.md", Some(&root), 1).unwrap_err();

    assert!(error.starts_with("AL_ERR:file.basenameBudget:"), "{error}");
    let params: serde_json::Value =
        serde_json::from_str(error.strip_prefix("AL_ERR:file.basenameBudget:").unwrap()).unwrap();
    assert_eq!(params["0"], "x.md");
}

#[test]
fn resolve_attachment_path_respects_gitignore_outside_git_repo() {
    let (_guard, root) = crate::test_support::tmp_root();
    let ignored = root.join("ignored");
    let visible = root.join("visible");
    std::fs::create_dir_all(&ignored).unwrap();
    std::fs::create_dir_all(&visible).unwrap();
    std::fs::write(root.join(".gitignore"), "ignored/\n").unwrap();
    std::fs::write(ignored.join("x.md"), "ignored match").unwrap();
    let visible_match = visible.join("x.md");
    std::fs::write(&visible_match, "visible match").unwrap();

    assert_eq!(
        resolve_attachment_path("x.md", Some(&root)).unwrap(),
        visible_match.canonicalize().unwrap()
    );
}

#[test]
fn resolve_attachment_path_preserves_missing_basename_error() {
    let (_guard, root) = crate::test_support::tmp_root();
    let missing = root.join("missing.md");
    let canonicalize_error = missing.canonicalize().unwrap_err();

    let error = resolve_attachment_path("missing.md", Some(&root)).unwrap_err();

    assert_eq!(
        error,
        format!(
            "cannot resolve attachment path {}: {canonicalize_error}",
            missing.to_string_lossy()
        )
    );
}

#[test]
fn resolve_attachment_path_does_not_search_for_relative_path_with_separator() {
    let (_guard, root) = crate::test_support::tmp_root();
    let nested = root.join("elsewhere");
    std::fs::create_dir_all(&nested).unwrap();
    std::fs::write(nested.join("x.md"), "not a fallback candidate").unwrap();

    let error = resolve_attachment_path("missing/x.md", Some(&root)).unwrap_err();

    assert!(error.contains("cannot resolve attachment path"), "{error}");
}

#[test]
fn resolve_attachment_path_does_not_search_for_backslash_path() {
    let (_guard, root) = crate::test_support::tmp_root();
    std::fs::write(
        root.join("visible-entry"),
        "forces a zero-budget scan to fail",
    )
    .unwrap();
    let path = r"C:\tmp\logo.png";
    let joined = root.join(path);
    let canonicalize_error = joined.canonicalize().unwrap_err();

    let error = resolve_attachment_path_with_basename_budget(path, Some(&root), 0).unwrap_err();

    assert_eq!(
        error,
        format!(
            "cannot resolve attachment path {}: {canonicalize_error}",
            joined.to_string_lossy()
        )
    );
}

#[test]
fn resolve_attachment_path_skips_excluded_directory_during_basename_search() {
    let (_guard, root) = crate::test_support::tmp_root();
    let nested = root.join("node_modules");
    std::fs::create_dir_all(&nested).unwrap();
    std::fs::write(nested.join("x.md"), "excluded").unwrap();

    let error = resolve_attachment_path("x.md", Some(&root)).unwrap_err();

    assert!(error.contains("cannot resolve attachment path"), "{error}");
}

#[test]
fn resolve_attachment_path_skips_hidden_directory_during_basename_search() {
    let (_guard, root) = crate::test_support::tmp_root();
    let nested = root.join(".hidden");
    std::fs::create_dir_all(&nested).unwrap();
    std::fs::write(nested.join("x.png"), "hidden").unwrap();

    let error = resolve_attachment_path("x.png", Some(&root)).unwrap_err();

    assert!(error.contains("cannot resolve attachment path"), "{error}");
}

#[test]
fn resolve_attachment_path_searches_when_session_base_itself_is_hidden() {
    let (_guard, root) = crate::test_support::tmp_root();
    let base = root.join(".agentloom");
    let nested = base.join("visible");
    let file = nested.join("x.png");
    std::fs::create_dir_all(&nested).unwrap();
    std::fs::write(&file, "inside hidden base").unwrap();

    assert_eq!(
        resolve_attachment_path("x.png", Some(&base)).unwrap(),
        file.canonicalize().unwrap()
    );
}

#[test]
fn resolve_attachment_path_skips_vendor_directory_during_basename_search() {
    let (_guard, root) = crate::test_support::tmp_root();
    let nested = root.join("vendor");
    std::fs::create_dir_all(&nested).unwrap();
    std::fs::write(nested.join("x.png"), "vendored").unwrap();

    let error = resolve_attachment_path("x.png", Some(&root)).unwrap_err();

    assert!(error.contains("cannot resolve attachment path"), "{error}");
}

#[test]
fn local_session_relative_attachment_uses_local_workspace() {
    let conn = crate::test_support::mem_db();
    conn.execute(
        "INSERT INTO sessions (id, title, namespace_id, created_at) \
             VALUES ('s-local-attachment', 'Local', 'local', 0)",
        [],
    )
    .unwrap();
    let (_guard, root) = crate::test_support::tmp_root();
    let workspace = root.join("local-default");
    std::fs::create_dir_all(&workspace).unwrap();
    std::fs::write(workspace.join("result.txt"), "local result").unwrap();

    let base = resolve_session_attachment_base_in(&conn, "s-local-attachment", &workspace).unwrap();
    let resolved = resolve_attachment_path("result.txt", Some(&base)).unwrap();
    let attachment = read_attachment_at(&resolved).unwrap();

    assert_eq!(base, workspace);
    assert_eq!(attachment.content, "local result");
}

#[test]
fn local_project_session_relative_attachment_uses_bound_project() {
    let conn = crate::test_support::mem_db();
    let (_guard, root) = crate::test_support::tmp_root();
    let project = root.join("bound-project");
    let asset = project.join("assets/x.png");
    std::fs::create_dir_all(asset.parent().unwrap()).unwrap();
    std::fs::write(&asset, "image bytes").unwrap();
    repos_repo::add_repo(
        &conn,
        "repo-local-attachment",
        "local",
        "local",
        None,
        "bound-project",
        project.to_str().unwrap(),
        None,
    )
    .unwrap();
    db::create_session(
        &conn,
        "s-local-project-attachment",
        "Local project",
        "repo-local-attachment",
        "local",
    )
    .unwrap();

    let unrelated_local_base = root.join("unrelated-local-base");
    std::fs::create_dir_all(&unrelated_local_base).unwrap();

    let base = resolve_session_attachment_base_in(
        &conn,
        "s-local-project-attachment",
        &unrelated_local_base,
    )
    .unwrap();

    assert_eq!(base, project);
    assert_eq!(
        resolve_attachment_path("assets/x.png", Some(&base)).unwrap(),
        asset.canonicalize().unwrap()
    );
}

#[test]
fn repo_session_relative_attachment_uses_repo_root() {
    let conn = crate::test_support::mem_db();
    let (_guard, root) = crate::test_support::tmp_root();
    let repo = root.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    std::fs::write(repo.join("result.txt"), "repo result").unwrap();
    namespaces_repo::add_namespace(&conn, "ns-attachment", "github_org", "org", 0).unwrap();
    repos_repo::add_repo(
        &conn,
        "repo-attachment",
        "ns-attachment",
        "github",
        None,
        "repo",
        repo.to_str().unwrap(),
        None,
    )
    .unwrap();
    db::create_session(
        &conn,
        "s-repo-attachment",
        "Repo",
        "repo-attachment",
        "ns-attachment",
    )
    .unwrap();

    let ignored_local = root.join("ignored-local");
    let base =
        resolve_session_attachment_base_in(&conn, "s-repo-attachment", &ignored_local).unwrap();
    let resolved = resolve_attachment_path("result.txt", Some(&base)).unwrap();
    let attachment = read_attachment_at(&resolved).unwrap();

    assert_eq!(base, repo);
    assert_eq!(attachment.content, "repo result");
}

#[test]
fn repo_session_missing_workspace_root_errors_without_creating_it() {
    let conn = crate::test_support::mem_db();
    let (_guard, root) = crate::test_support::tmp_root();
    // 项目目录被挪走/删掉之后的场景：repo 路径记在 DB 里，但盘上不存在。
    let missing = root.join("moved-away-repo");
    namespaces_repo::add_namespace(&conn, "ns-missing-repo", "github_org", "org", 0).unwrap();
    repos_repo::add_repo(
        &conn,
        "repo-missing",
        "ns-missing-repo",
        "github",
        None,
        "repo",
        missing.to_str().unwrap(),
        None,
    )
    .unwrap();
    db::create_session(
        &conn,
        "s-missing-repo",
        "Repo",
        "repo-missing",
        "ns-missing-repo",
    )
    .unwrap();

    let ignored_local = root.join("ignored-local");
    let error =
        resolve_session_attachment_base_in(&conn, "s-missing-repo", &ignored_local).unwrap_err();

    assert!(error.contains("does not exist"), "{error}");
    assert!(
        !missing.exists(),
        "must not conjure the missing workspace root back into being"
    );
}

#[test]
fn resolve_attachment_path_rejects_parent_traversal() {
    let (_guard, root) = crate::test_support::tmp_root();
    let base = root.join("workspace");
    std::fs::create_dir_all(&base).unwrap();
    std::fs::write(root.join("secret.txt"), "secret").unwrap();

    let error = resolve_attachment_path("../secret.txt", Some(&base)).unwrap_err();

    assert!(error.contains("outside session directory"), "{error}");
}

#[test]
fn resolve_attachment_path_rejects_relative_path_without_base() {
    let error = resolve_attachment_path("a/b.txt", None).unwrap_err();
    assert!(error.contains("relative"));
}

#[test]
fn save_pasted_image_bound_project_session_writes_to_project_attachments_dir() {
    let conn = crate::test_support::mem_db();
    let (_guard, root) = crate::test_support::tmp_root();
    let project = root.join("bound-project-t21");
    std::fs::create_dir_all(&project).unwrap();
    repos_repo::add_repo(
        &conn,
        "repo-t21-project",
        "local",
        "local",
        None,
        "t21-project",
        project.to_str().unwrap(),
        None,
    )
    .unwrap();
    db::create_session(&conn, "s-t21-project", "T21", "repo-t21-project", "local").unwrap();

    let base = resolve_session_attachment_base_in(&conn, "s-t21-project", &root).unwrap();
    let encoded = base64::engine::general_purpose::STANDARD.encode(b"\x89PNG\r\n\x1a\n");
    let saved = save_pasted_image_in(&encoded, "image/png", &base).unwrap();
    let path = std::path::PathBuf::from(saved);

    assert!(path.starts_with(project.join(".agentloom").join("attachments")));
    assert!(path.exists());
}

#[test]
fn save_pasted_text_local_session_writes_to_local_workspace_attachments_dir() {
    let conn = crate::test_support::mem_db();
    conn.execute(
        "INSERT INTO sessions (id, title, namespace_id, created_at) \
             VALUES ('s-t21-local', 'Local T21', 'local', 0)",
        [],
    )
    .unwrap();
    let (_guard, root) = crate::test_support::tmp_root();
    let workspace = root.join("t21-local-workspace");
    std::fs::create_dir_all(&workspace).unwrap();

    let base = resolve_session_attachment_base_in(&conn, "s-t21-local", &workspace).unwrap();
    let saved = save_pasted_text_in("local session paste", &base).unwrap();
    let path = std::path::PathBuf::from(saved);

    assert!(path.starts_with(workspace.join(".agentloom").join("attachments")));
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "local session paste"
    );
}

#[test]
fn import_attachment_into_workspace_copies_file_and_dedupes_name_collision() {
    let conn = crate::test_support::mem_db();
    let (_guard, root) = crate::test_support::tmp_root();
    let project = root.join("attach-import-project");
    std::fs::create_dir_all(&project).unwrap();
    repos_repo::add_repo(
        &conn,
        "repo-attach-import",
        "local",
        "local",
        None,
        "attach-import-project",
        project.to_str().unwrap(),
        None,
    )
    .unwrap();
    db::create_session(
        &conn,
        "s-attach-import",
        "T21b",
        "repo-attach-import",
        "local",
    )
    .unwrap();

    let source_dir = root.join("desktop");
    std::fs::create_dir_all(&source_dir).unwrap();
    let source = source_dir.join("shot.png");
    std::fs::write(&source, b"\x89PNG\r\n\x1a\n fake-but-fine-for-a-copy-test").unwrap();

    let db = crate::db::Db(crate::perf_probe::TimedMutex::new(conn));
    let copied = crate::attachments::dir::import_attachment_into_workspace(
        &db,
        "s-attach-import",
        source.to_str().unwrap(),
    )
    .unwrap();
    let copied_path = std::path::PathBuf::from(&copied);

    assert!(copied_path.starts_with(project.join(".agentloom").join("attachments")));
    assert_eq!(
        std::fs::read(&copied_path).unwrap(),
        std::fs::read(&source).unwrap()
    );
    // 原文件保留、没被挪走。
    assert!(source.exists());

    // 再拷一次同名文件：不覆盖、加序号。
    let copied_again = crate::attachments::dir::import_attachment_into_workspace(
        &db,
        "s-attach-import",
        source.to_str().unwrap(),
    )
    .unwrap();
    assert_ne!(copied, copied_again);
    assert!(std::path::PathBuf::from(&copied_again)
        .file_name()
        .unwrap()
        .to_string_lossy()
        .contains("shot-1"));
}

#[test]
fn read_attachment_legacy_pasted_absolute_path_is_still_readable_within_session_scope() {
    let (_guard, root) = crate::test_support::tmp_root();
    let workspace = root.join("workspace-with-legacy-read");
    std::fs::create_dir_all(&workspace).unwrap();
    let home = root.join("legacy-home");
    let legacy = home.join(".agentloom").join("pasted").join("old.png");
    std::fs::create_dir_all(legacy.parent().unwrap()).unwrap();
    std::fs::write(&legacy, "legacy bytes").unwrap();
    let original_home = std::env::var_os("HOME");
    std::env::set_var("HOME", &home);

    let resolved = resolve_attachment_path(legacy.to_str().unwrap(), Some(&workspace));
    let guard_result = resolved.as_ref().ok().map(|resolved_path| {
        crate::attachments::dir::assert_read_attachment_absolute_scope(
            legacy.to_str().unwrap(),
            resolved_path,
            Some(&workspace),
        )
    });

    match original_home {
        Some(value) => std::env::set_var("HOME", value),
        None => std::env::remove_var("HOME"),
    }

    let resolved = resolved.unwrap();
    assert_eq!(resolved, legacy);
    assert!(guard_result.unwrap().is_ok());
    let attachment = read_attachment_at(&resolved).unwrap();
    assert_eq!(attachment.kind, "image");
}

#[test]
fn read_attachment_rejects_absolute_path_outside_session_and_home() {
    let (_guard, root) = crate::test_support::tmp_root();
    let workspace = root.join("workspace-rejects-outside");
    std::fs::create_dir_all(&workspace).unwrap();
    let elsewhere = root.join("elsewhere-t21");
    let secret = elsewhere.join("secret.txt");
    std::fs::create_dir_all(&elsewhere).unwrap();
    std::fs::write(&secret, "secret").unwrap();

    let resolved = resolve_attachment_path(secret.to_str().unwrap(), Some(&workspace)).unwrap();
    let error = crate::attachments::dir::assert_read_attachment_absolute_scope(
        secret.to_str().unwrap(),
        &resolved,
        Some(&workspace),
    )
    .unwrap_err();

    assert!(error.contains("outside the workspace"), "{error}");
}

#[test]
fn project_session_pasted_text_readable_with_session_id_rejected_without() {
    // composeText 全链：项目会话粘贴长文本 -> save_pasted_text 落
    // `<项目>/.agentloom/attachments/paste-*.txt` -> read_attachment 必须带 sessionId
    // 才读得到（不带 sessionId 时既不在 `~/.agentloom/` 下，也不是位图，只能被拒）。
    let conn = crate::test_support::mem_db();
    let (_guard, root) = crate::test_support::tmp_root();
    let project = root.join("compose-bound-project");
    std::fs::create_dir_all(&project).unwrap();
    repos_repo::add_repo(
        &conn,
        "repo-compose-project",
        "local",
        "local",
        None,
        "compose-bound-project",
        project.to_str().unwrap(),
        None,
    )
    .unwrap();
    db::create_session(
        &conn,
        "s-compose-project",
        "Compose project",
        "repo-compose-project",
        "local",
    )
    .unwrap();

    let base = resolve_session_attachment_base(&conn, "s-compose-project").unwrap();
    assert_eq!(base, project);
    let pasted_path = save_pasted_text_in("HELLO_FILE_BODY", &base).unwrap();
    assert!(
        pasted_path.starts_with(project.join(".agentloom/attachments").to_str().unwrap()),
        "{pasted_path}"
    );

    let with_session = resolve_attachment_path(&pasted_path, Some(&base)).unwrap();
    assert!(
        crate::attachments::dir::assert_read_attachment_absolute_scope(
            &pasted_path,
            &with_session,
            Some(&base),
        )
        .is_ok()
    );

    let without_session = resolve_attachment_path(&pasted_path, None).unwrap();
    let error = crate::attachments::dir::assert_read_attachment_absolute_scope(
        &pasted_path,
        &without_session,
        None,
    )
    .unwrap_err();
    assert!(error.contains("outside the workspace"), "{error}");
}

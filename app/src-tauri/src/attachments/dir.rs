//! 会话附件目录落点 + `read_attachment` 绝对路径读取范围校验。
//!
//! 目标目录：会话工作区根 `<root>/.agentloom/attachments/`——`SessionWorkspace::Repo(path)`
//! 落绑定项目本身，`SessionWorkspace::Local` 落它现有的本地工作区（未绑定项目时是
//! `~/.agentloom/local/default`，绑定 `local-default` 项目时是该项目目录本身）。调用方已经
//! 在算「会话工作区根」（`resolve_session_attachment_base_in` / `BuildContext::wt`），这里
//! 只管「工作区根 -> 附件目录」这一步，顺带幂等挂 git exclude。

use std::io::Read;
use std::path::{Path, PathBuf};

/// 工作区根目录下的附件落点，目录不存在则创建；顺带幂等把 `.agentloom/` 挂进
/// `.git/info/exclude`（非 git 目录是空操作，见 `super::exclude`）。`workspace_root` 允许相对
/// 路径（挪自原 `save_pasted_*_in` 的既有兜底，行为不变——只是搬了个文件）。
pub fn attachments_dir_for_workspace(workspace_root: &Path) -> Result<PathBuf, String> {
    let workspace_root = if workspace_root.is_absolute() {
        workspace_root.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|e| format!("cannot resolve attachments directory: {e}"))?
            .join(workspace_root)
    };
    let dir = workspace_root.join(".agentloom").join("attachments");
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("cannot create attachments directory: {e}"))?;
    super::exclude::ensure_agentloom_dir_excluded(&workspace_root);
    Ok(dir)
}

/// 粘贴/拖入附件的落盘基准目录：有会话就落会话工作区根，没有就退回旧 app 域 home
/// （兼容 `composeText` 那条不带 `session_id` 的既有「附件对话框选任意本机文件」路径）。
pub fn pasted_input_base(
    db: &tauri::State<'_, crate::db::Db>,
    session_id: Option<String>,
) -> Result<PathBuf, String> {
    let Some(sid) = session_id else {
        return Ok(crate::home_dir_for_attachment());
    };
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    crate::resolve_session_attachment_base(&conn, &sid)
}

/// `read_attachment` / `open_attachment_external` 共用：`session_id` 有值才解析会话工作区根，
/// 无值就是 `None`（两个命令各自决定 `None` 下的放行范围，这里只管解析）。
pub fn resolve_session_base_option(
    db: &tauri::State<'_, crate::db::Db>,
    session_id: Option<String>,
) -> Result<Option<PathBuf>, String> {
    let Some(sid) = session_id else {
        return Ok(None);
    };
    let conn = db.0.lock().map_err(|e| e.to_string())?;
    Ok(Some(crate::resolve_session_attachment_base(&conn, &sid)?))
}

/// 附件对话框「拷贝进工作区」命令的实体：把用户经系统文件对话框选中的任意本机文件拷进
/// `<会话工作区>/.agentloom/attachments/`（同名冲突加数字序号），返回拷贝后的绝对路径——
/// 调用方（`InputArea.tsx` 的 `attachFile`）此后用新路径组消息，`read_attachment`/渲染都落
/// 在工作区内走 A 规则，不必依赖 B 规则的位图豁免。必须带 `session_id`（无 session 的既有
/// 调用点不走这条新路，见 `pasted_input_base`）。
pub fn import_attachment_into_workspace(
    db: &crate::db::Db,
    session_id: &str,
    source_path: &str,
) -> Result<String, String> {
    let base = {
        let conn = db.0.lock().map_err(|e| e.to_string())?;
        crate::resolve_session_attachment_base(&conn, session_id)?
    };
    let dest_dir = attachments_dir_for_workspace(&base)?;
    let source = Path::new(source_path);
    let file_name = source
        .file_name()
        .ok_or_else(|| format!("attachment path has no file name: {source_path}"))?;
    let dest = unique_destination(&dest_dir, Path::new(file_name));
    std::fs::copy(source, &dest)
        .map_err(|e| format!("cannot copy attachment into workspace: {e}"))?;
    Ok(dest.to_string_lossy().to_string())
}

/// `import_attachment_into_workspace` 的 tauri 命令包装（`generate_handler!` 里按全路径
/// `attachments::dir::import_attachment_into_workspace_cmd` 注册）：只做 `State<Db>` 解引用，
/// 真正逻辑在上面那个可直接单测的版本里。
#[tauri::command]
pub fn import_attachment_into_workspace_cmd(
    db: tauri::State<'_, crate::db::Db>,
    session_id: String,
    path: String,
) -> Result<String, String> {
    import_attachment_into_workspace(&db, &session_id, &path)
}

/// 目标目录内挑一个不冲突的文件名：`name.ext` 已存在就试 `name-1.ext`、`name-2.ext`……
fn unique_destination(dir: &Path, file_name: &Path) -> PathBuf {
    let candidate = dir.join(file_name);
    if !candidate.exists() {
        return candidate;
    }
    let stem = file_name
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    let ext = file_name
        .extension()
        .map(|e| e.to_string_lossy().to_string());
    let mut n = 1u32;
    loop {
        let name = match &ext {
            Some(ext) => format!("{stem}-{n}.{ext}"),
            None => format!("{stem}-{n}"),
        };
        let candidate = dir.join(name);
        if !candidate.exists() {
            return candidate;
        }
        n += 1;
    }
}

/// 绝对路径是否落在允许的根集合内：① 当前会话工作区（若已解析出）、
/// ② `home_agentloom_dir`（旧版 `pasted/` 遗留附件 + 未绑定项目的本地会话都在它下面）。
/// `canonical_path` 必须已经 `canonicalize()` 过（穿透 symlink、吃掉 `..`）。
fn absolute_attachment_path_allowed(
    canonical_path: &Path,
    session_workspace_root: Option<&Path>,
    home_agentloom_dir: &Path,
) -> bool {
    if let Some(root) = session_workspace_root {
        if let Ok(canonical_root) = root.canonicalize() {
            if canonical_path.starts_with(&canonical_root) {
                return true;
            }
        }
    }
    match home_agentloom_dir.canonicalize() {
        Ok(canonical_home) => canonical_path.starts_with(&canonical_home),
        Err(_) => false,
    }
}

/// 位图放行判定（B 规则）：扩展名 ∈ png/jpg/jpeg/gif/webp **且**魔数验证匹配该扩展名、
/// ≤ 10MB——与 `read_attachment_at` 的既有嗅探/大小上限一致，但只挑这四种「直接可渲染的
/// 位图」；svg 是文本（内容可含脚本/外链），bmp 不在名单内，两者都不享 B、只走 A。扩展名
/// 校验挡住「魔数是 png、扩展名却是 `.txt`」这类多态文件——它们会被 `read_attachment_at`
/// 当文本读回 webview，不该靠 B 放行。
fn looks_like_verified_bitmap(canonical_path: &Path) -> bool {
    const MAX_IMAGE_BYTES: u64 = 10 * 1024 * 1024;
    let ext = canonical_path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    let expected_media_type = match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        _ => return false,
    };
    let Ok(metadata) = std::fs::metadata(canonical_path) else {
        return false;
    };
    if !metadata.is_file() || metadata.len() > MAX_IMAGE_BYTES {
        return false;
    }
    let Ok(mut file) = std::fs::File::open(canonical_path) else {
        return false;
    };
    let mut head = [0u8; 12];
    let Ok(n) = file.read(&mut head) else {
        return false;
    };
    crate::sniff_image_media_type(&head[..n]) == Some(expected_media_type)
}

/// `read_attachment` 专用守卫：只有「原始输入本身就是绝对路径 / `~` 展开」这一路才需要额外
/// 校验——相对路径已经被 `resolve_attachment_path` 强制收在 session base 内，不重复判断。
/// 放行条件 = A（工作区或 `~/.agentloom/` 内）或 B（魔数验证过的位图，见
/// `looks_like_verified_bitmap`）；`session_id` 是否有值只影响 A 里「工作区」这一根是否存在，
/// 不再整段跳过校验。
pub fn assert_read_attachment_absolute_scope(
    original_path: &str,
    resolved: &Path,
    session_workspace_root: Option<&Path>,
) -> Result<(), String> {
    assert_absolute_scope_with_home(
        original_path,
        resolved,
        session_workspace_root,
        &crate::home_dir_for_attachment().join(".agentloom"),
        true,
    )
}

/// `open_attachment_external` 专用守卫：只走 A，不享 B——位图放行是给「只读渲染」开的口子，
/// 系统 opener 会把文件交给外部程序处理，不该对图片格式网开一面。
pub fn assert_open_attachment_absolute_scope(
    original_path: &str,
    resolved: &Path,
    session_workspace_root: Option<&Path>,
) -> Result<(), String> {
    assert_absolute_scope_with_home(
        original_path,
        resolved,
        session_workspace_root,
        &crate::home_dir_for_attachment().join(".agentloom"),
        false,
    )
}

fn assert_absolute_scope_with_home(
    original_path: &str,
    resolved: &Path,
    session_workspace_root: Option<&Path>,
    home_agentloom_dir: &Path,
    allow_verified_bitmap: bool,
) -> Result<(), String> {
    let looks_absolute_input = original_path == "~"
        || original_path.starts_with("~/")
        || Path::new(original_path).is_absolute();
    if !looks_absolute_input {
        return Ok(());
    }
    let canonical = resolved.canonicalize().map_err(|e| {
        format!(
            "cannot resolve attachment path {}: {e}",
            resolved.to_string_lossy()
        )
    })?;
    if absolute_attachment_path_allowed(&canonical, session_workspace_root, home_agentloom_dir) {
        return Ok(());
    }
    if allow_verified_bitmap && looks_like_verified_bitmap(&canonical) {
        return Ok(());
    }
    Err(format!(
        "attachment path is outside the workspace and is not a verified image: {}",
        resolved.to_string_lossy()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    const PNG_MAGIC: &[u8] = b"\x89PNG\r\n\x1a\n rest-of-file-not-real-png-but-magic-is-enough";

    fn write(path: &Path, content: &str) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, content).unwrap();
    }

    fn write_bytes(path: &Path, bytes: &[u8]) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, bytes).unwrap();
    }

    fn read_ok(
        original_path: &str,
        resolved: &Path,
        session_workspace_root: Option<&Path>,
        home: &Path,
    ) -> Result<(), String> {
        assert_absolute_scope_with_home(original_path, resolved, session_workspace_root, home, true)
    }

    fn open_ok(
        original_path: &str,
        resolved: &Path,
        session_workspace_root: Option<&Path>,
        home: &Path,
    ) -> Result<(), String> {
        assert_absolute_scope_with_home(
            original_path,
            resolved,
            session_workspace_root,
            home,
            false,
        )
    }

    #[test]
    fn attachments_dir_for_workspace_creates_nested_dir() {
        let root = tempfile::tempdir().unwrap();
        let dir = attachments_dir_for_workspace(root.path()).unwrap();
        assert!(dir.is_dir());
        assert_eq!(dir, root.path().join(".agentloom").join("attachments"));
    }

    #[test]
    fn unique_destination_picks_first_free_name_when_collisions_exist() {
        let dir = tempfile::tempdir().unwrap();
        write(&dir.path().join("shot.png"), "existing-1");
        write(&dir.path().join("shot-1.png"), "existing-2");

        let dest = unique_destination(dir.path(), Path::new("shot.png"));

        assert_eq!(dest, dir.path().join("shot-2.png"));
    }

    #[test]
    fn unique_destination_keeps_original_name_when_no_collision() {
        let dir = tempfile::tempdir().unwrap();

        let dest = unique_destination(dir.path(), Path::new("shot.png"));

        assert_eq!(dest, dir.path().join("shot.png"));
    }

    #[test]
    fn allows_absolute_path_inside_session_workspace() {
        let root = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let file = root.path().join("a.png");
        write(&file, "x");

        assert!(read_ok(
            file.to_str().unwrap(),
            &file,
            Some(root.path()),
            home.path()
        )
        .is_ok());
    }

    #[test]
    fn allows_absolute_path_inside_legacy_home_agentloom() {
        let root = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let legacy = home.path().join("pasted").join("x.png");
        write(&legacy, "x");

        assert!(read_ok(
            legacy.to_str().unwrap(),
            &legacy,
            Some(root.path()),
            home.path()
        )
        .is_ok());
    }

    #[test]
    fn rejects_absolute_path_outside_all_roots() {
        let root = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let elsewhere = tempfile::tempdir().unwrap();
        let secret = elsewhere.path().join("etc_passwd_stand_in.txt");
        write(&secret, "root:x:0:0");

        let error = read_ok(
            secret.to_str().unwrap(),
            &secret,
            Some(root.path()),
            home.path(),
        )
        .unwrap_err();

        assert!(error.contains("outside the workspace"), "{error}");
    }

    #[test]
    fn rejects_parent_traversal_that_escapes_workspace() {
        let root = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let secret = outside.path().join("secret.txt");
        write(&secret, "secret");
        // 绝对路径里带 `..`：字面量在 workspace 内但 canonicalize 后逃出去。
        let traversal = root
            .path()
            .join("..")
            .join(
                outside
                    .path()
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .to_string(),
            )
            .join("secret.txt");

        let error = read_ok(
            traversal.to_str().unwrap(),
            &traversal,
            Some(root.path()),
            home.path(),
        )
        .unwrap_err();

        assert!(error.contains("outside the workspace"), "{error}");
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlink_pointing_outside_workspace() {
        let root = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let secret = outside.path().join("secret.txt");
        write(&secret, "secret");
        let link = root.path().join("link.txt");
        std::os::unix::fs::symlink(&secret, &link).unwrap();

        let error = read_ok(
            link.to_str().unwrap(),
            &link,
            Some(root.path()),
            home.path(),
        )
        .unwrap_err();

        assert!(error.contains("outside the workspace"), "{error}");
    }

    #[test]
    fn rejects_etc_passwd_style_absolute_path() {
        let root = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();

        let error = read_ok(
            "/etc/passwd",
            Path::new("/etc/passwd"),
            Some(root.path()),
            home.path(),
        )
        .unwrap_err();

        assert!(error.contains("outside the workspace"), "{error}");
    }

    #[test]
    fn skips_check_for_relative_input_even_when_resolved_lands_outside() {
        // 相对路径已经被 resolve_attachment_path 强制收在 base 内解析——这层守卫不重复判断，
        // 传一个「resolved 恰好落在别处」的场景只是验证「不看 resolved、只看原始输入形态」。
        let root = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let elsewhere = tempfile::tempdir().unwrap();
        let file = elsewhere.path().join("x.txt");
        write(&file, "x");

        assert!(read_ok("x.txt", &file, Some(root.path()), home.path()).is_ok());
    }

    // --- 返工新增：session_id=None 不再整段放行，改按 A/B 规则判 ---

    #[test]
    fn session_none_rejects_etc_passwd() {
        let home = tempfile::tempdir().unwrap();

        let error =
            read_ok("/etc/passwd", Path::new("/etc/passwd"), None, home.path()).unwrap_err();

        assert!(error.contains("outside the workspace"), "{error}");
    }

    #[test]
    fn session_none_allows_real_png_outside_workspace_via_bitmap_fallback() {
        // 「桌面真 png」的替身：session_id=None（没有工作区根）、不在 ~/.agentloom 内，
        // 但魔数验证过是真位图——B 规则放行。
        let home = tempfile::tempdir().unwrap();
        let desktop = tempfile::tempdir().unwrap();
        let photo = desktop.path().join("shot.png");
        write_bytes(&photo, PNG_MAGIC);

        assert!(read_ok(photo.to_str().unwrap(), &photo, None, home.path()).is_ok());
    }

    #[test]
    fn session_some_allows_real_png_agent_wrote_to_tmp_dir() {
        // 对应 MessageContent.images.test.tsx 那类「agent 用 --out=/tmp/a.png 生成图、消息里
        // 自动内联」用例：有会话工作区（不含 tmp）、图落在 std::env::temp_dir() 下——B 规则
        // 照样放行，不因为 session 有值就退化成只走 A。
        let root = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let plot = std::env::temp_dir().join(format!(
            "agentloom-t21b-probe-{}-{}.png",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        write_bytes(&plot, PNG_MAGIC);

        let result = read_ok(
            plot.to_str().unwrap(),
            &plot,
            Some(root.path()),
            home.path(),
        );
        let _ = std::fs::remove_file(&plot);

        assert!(result.is_ok(), "{result:?}");
    }

    #[test]
    fn session_none_rejects_text_disguised_as_png_extension() {
        let home = tempfile::tempdir().unwrap();
        let desktop = tempfile::tempdir().unwrap();
        let fake = desktop.path().join("not-really.png");
        write(&fake, "just plain text, not a png");

        let error = read_ok(fake.to_str().unwrap(), &fake, None, home.path()).unwrap_err();

        assert!(error.contains("outside the workspace"), "{error}");
    }

    #[test]
    fn session_none_rejects_png_magic_with_disallowed_extension() {
        // 魔数是真 png，但扩展名是 `.txt`——B 规则要求扩展名也在名单内，否则
        // `read_attachment_at` 会把它当文本读回 webview，不该靠位图豁免放行。
        let home = tempfile::tempdir().unwrap();
        let desktop = tempfile::tempdir().unwrap();
        let polyglot = desktop.path().join("evil.txt");
        write_bytes(&polyglot, PNG_MAGIC);

        let error = read_ok(polyglot.to_str().unwrap(), &polyglot, None, home.path()).unwrap_err();

        assert!(error.contains("outside the workspace"), "{error}");
    }

    #[test]
    fn session_none_allows_polyglot_png_with_trailing_bytes_when_extension_matches() {
        // 多态文件：png 魔数开头 + 尾随任意其它字节，但扩展名是 `.png`——按 B 规则放行
        // （`PNG_MAGIC` 本身就带一段任意尾随字节，这里显式起名覆盖 brief 点名的语料）。
        let home = tempfile::tempdir().unwrap();
        let desktop = tempfile::tempdir().unwrap();
        let polyglot = desktop.path().join("shot.png");
        write_bytes(&polyglot, PNG_MAGIC);

        assert!(read_ok(polyglot.to_str().unwrap(), &polyglot, None, home.path()).is_ok());
    }

    #[test]
    fn session_none_rejects_real_png_over_ten_megabytes() {
        let home = tempfile::tempdir().unwrap();
        let desktop = tempfile::tempdir().unwrap();
        let big = desktop.path().join("huge.png");
        let mut bytes = PNG_MAGIC.to_vec();
        bytes.resize(10 * 1024 * 1024 + 1, 0u8);
        write_bytes(&big, &bytes);

        let error = read_ok(big.to_str().unwrap(), &big, None, home.path()).unwrap_err();

        assert!(error.contains("outside the workspace"), "{error}");
    }

    #[test]
    fn session_none_allows_real_png_at_exactly_ten_megabytes() {
        let home = tempfile::tempdir().unwrap();
        let desktop = tempfile::tempdir().unwrap();
        let exact = desktop.path().join("exact.png");
        let mut bytes = PNG_MAGIC.to_vec();
        bytes.resize(10 * 1024 * 1024, 0u8);
        write_bytes(&exact, &bytes);

        assert!(read_ok(exact.to_str().unwrap(), &exact, None, home.path()).is_ok());
    }

    #[test]
    fn session_none_allows_symlink_to_real_png_outside_workspace_without_panic() {
        let home = tempfile::tempdir().unwrap();
        let desktop = tempfile::tempdir().unwrap();
        let real = desktop.path().join("real.png");
        write_bytes(&real, PNG_MAGIC);
        let link = desktop.path().join("link.png");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&real, &link).unwrap();

        assert!(read_ok(link.to_str().unwrap(), &link, None, home.path()).is_ok());
    }

    #[test]
    fn session_none_rejects_directory_named_dot_png() {
        let home = tempfile::tempdir().unwrap();
        let desktop = tempfile::tempdir().unwrap();
        let dir_as_png = desktop.path().join("dir.png");
        std::fs::create_dir_all(&dir_as_png).unwrap();

        let error =
            read_ok(dir_as_png.to_str().unwrap(), &dir_as_png, None, home.path()).unwrap_err();

        assert!(error.contains("outside the workspace"), "{error}");
    }

    #[test]
    #[cfg(unix)]
    fn session_none_rejects_fifo_named_dot_png() {
        let home = tempfile::tempdir().unwrap();
        let desktop = tempfile::tempdir().unwrap();
        let fifo = desktop.path().join("pipe.png");
        let c_path = std::ffi::CString::new(fifo.to_str().unwrap()).unwrap();
        let made = unsafe { libc::mkfifo(c_path.as_ptr(), 0o600) };
        assert_eq!(made, 0, "mkfifo failed");

        let error = read_ok(fifo.to_str().unwrap(), &fifo, None, home.path()).unwrap_err();

        assert!(error.contains("outside the workspace"), "{error}");
    }

    #[test]
    fn session_none_rejects_zero_byte_dot_png() {
        let home = tempfile::tempdir().unwrap();
        let desktop = tempfile::tempdir().unwrap();
        let empty = desktop.path().join("empty.png");
        write_bytes(&empty, &[]);

        let error = read_ok(empty.to_str().unwrap(), &empty, None, home.path()).unwrap_err();

        assert!(error.contains("outside the workspace"), "{error}");
    }

    // --- 返工新增：有 session 时 svg 不享 B，只走 A ---

    #[test]
    fn session_some_rejects_svg_outside_workspace() {
        let root = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let svg = outside.path().join("icon.svg");
        write(&svg, "<svg></svg>");

        let error =
            read_ok(svg.to_str().unwrap(), &svg, Some(root.path()), home.path()).unwrap_err();

        assert!(error.contains("outside the workspace"), "{error}");
    }

    #[test]
    fn session_some_allows_svg_inside_workspace() {
        let root = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let svg = root.path().join("icon.svg");
        write(&svg, "<svg></svg>");

        assert!(read_ok(svg.to_str().unwrap(), &svg, Some(root.path()), home.path()).is_ok());
    }

    // --- 返工新增：open_attachment_external 只走 A，不享 B ---

    #[test]
    fn open_external_rejects_html_outside_workspace() {
        let root = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let html = outside.path().join("report.html");
        write(&html, "<html></html>");

        let error = open_ok(
            html.to_str().unwrap(),
            &html,
            Some(root.path()),
            home.path(),
        )
        .unwrap_err();

        assert!(error.contains("outside the workspace"), "{error}");
    }

    #[test]
    fn open_external_does_not_get_bitmap_fallback_for_png_outside_workspace() {
        // 与 read_attachment 的关键差异：即便是真 png，open_attachment_external 也不放行。
        let root = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let photo = outside.path().join("shot.png");
        write_bytes(&photo, PNG_MAGIC);

        let error = open_ok(
            photo.to_str().unwrap(),
            &photo,
            Some(root.path()),
            home.path(),
        )
        .unwrap_err();

        assert!(error.contains("outside the workspace"), "{error}");
    }
}

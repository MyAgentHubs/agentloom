//! Image attachment loading and provider image capability defaults, shared by CLI/orchestrator logic.
//!
//! Design highlights:
//! - Trust magic bytes, not extensions: fake images with renamed extensions must be rejected.
//! - Limit each image to 10 MB and each turn to 8 images; fail hard with the image and reason.
//! - `ImageBlock` base64 data stays in memory and never reaches the journal or conversation.json:
//!   `ImageBlock` serialization via `Serialize` skips the `data_base64` field.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{HarnessError, Result};

pub const MAX_IMAGE_BYTES: usize = 10 * 1024 * 1024;
pub const MAX_IMAGES_PER_TURN: usize = 8;

/// 一张已加载的图片：base64 内容 + 元信息。`data_base64` 永不序列化落盘。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageBlock {
    pub media_type: String,
    #[serde(default, skip_serializing)]
    pub data_base64: String,
    pub source_path: Option<PathBuf>,
    pub bytes: usize,
    /// Hexadecimal SHA-256 content fingerprint, computed at initial load. On resume,
    /// prefer this over byte count plus `media_type`: those checks miss same-length
    /// content replacements and can silently substitute historical images.
    /// `#[serde(default)]` supplies `None` for older conversation.json files without
    /// this field; rereading then falls back to byte-count checks for compatibility.
    #[serde(default)]
    pub sha256: Option<String>,
}

/// 按文件头魔数识别的图片格式（扩展名不可信——只认字节）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageFormat {
    Png,
    Jpeg,
    Gif,
    Webp,
}

impl ImageFormat {
    pub fn media_type(self) -> &'static str {
        match self {
            ImageFormat::Png => "image/png",
            ImageFormat::Jpeg => "image/jpeg",
            ImageFormat::Gif => "image/gif",
            ImageFormat::Webp => "image/webp",
        }
    }
}

/// 按魔数嗅探格式；识别不出（含假扩展名的文本文件）一律 None。
pub fn sniff_format(bytes: &[u8]) -> Option<ImageFormat> {
    if bytes.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]) {
        return Some(ImageFormat::Png);
    }
    if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        return Some(ImageFormat::Jpeg);
    }
    if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        return Some(ImageFormat::Gif);
    }
    if bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        return Some(ImageFormat::Webp);
    }
    None
}

/// 加载 + 校验一批图片路径（CLI `--image` 的落地实现）。
/// 校验顺序：数量上限 → 逐张存在性 → 魔数识别 → 单张大小上限。
/// 任何一条不合法都直接报错退出，文案说清哪张为什么。
pub fn load_images(paths: &[PathBuf]) -> Result<Vec<ImageBlock>> {
    if paths.len() > MAX_IMAGES_PER_TURN {
        return Err(HarnessError::InvalidConfig(format!(
            "too many --image attachments: {} given, max {MAX_IMAGES_PER_TURN} per turn",
            paths.len()
        )));
    }
    let mut out = Vec::with_capacity(paths.len());
    for path in paths {
        out.push(load_one_image(path)?);
    }
    Ok(out)
}

fn load_one_image(path: &Path) -> Result<ImageBlock> {
    let display = path.to_string_lossy().into_owned();
    let meta = std::fs::metadata(path);
    if !matches!(&meta, Ok(m) if m.is_file()) {
        return Err(HarnessError::InvalidConfig(format!(
            "--image {display}: file not found"
        )));
    }
    // 先看 stat 出来的大小再决定要不要整读进内存——否则一个几百 MB 的手滑附件会先被
    // 整个读进 RSS 才判超限，8 张 --image 就是 8 倍量级的内存放大（无需读内容即可拒绝）。
    let declared_len = meta.unwrap().len();
    if declared_len as u128 > MAX_IMAGE_BYTES as u128 {
        return Err(HarnessError::InvalidConfig(format!(
            "--image {display}: {declared_len} bytes exceeds the {MAX_IMAGE_BYTES}-byte (10 MB) limit"
        )));
    }
    let bytes = std::fs::read(path).map_err(|e| {
        HarnessError::InvalidConfig(format!("--image {display}: failed to read file: {e}"))
    })?;
    if bytes.len() > MAX_IMAGE_BYTES {
        return Err(HarnessError::InvalidConfig(format!(
            "--image {display}: {} bytes exceeds the {MAX_IMAGE_BYTES}-byte (10 MB) limit",
            bytes.len()
        )));
    }
    let format = sniff_format(&bytes).ok_or_else(|| {
        HarnessError::InvalidConfig(format!(
            "--image {display}: not a recognized image (checked PNG/JPEG/GIF/WEBP magic bytes; \
             file extension is not trusted)"
        ))
    })?;
    use base64::Engine;
    let data_base64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
    let sha256 = Some(sha256_hex(&bytes));
    Ok(ImageBlock {
        media_type: format.media_type().to_string(),
        data_base64,
        source_path: Some(path.to_path_buf()),
        bytes: bytes.len(),
        sha256,
    })
}

/// Hexadecimal SHA-256 content fingerprint, formatted byte by byte without requiring
/// the digest output to implement `LowerHex`: sha2 0.11's `Array` output does not
/// implement that trait, unlike the older `GenericArray` output.
fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(bytes);
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

/// 未显式配置 supports_images 时，按 provider_id 家族猜默认值。
/// Glm/Kimi 分组复用 `provider::native_search::provider_family`（原生搜索那套家族识别，
/// 避免两处子串判断各写一份、彼此漂移——这正是 bigmodel.cn 这类不含 glm/zhipu/zai 子串的
/// GLM 官方域名 id 曾被错判成不支持图片的根因）；anthropic/claude/openai/gpt 家族目前
/// 没有对应的 `ProviderFamily` 分类，仍按子串猜。
pub fn default_supports_images(provider_id: &str) -> bool {
    use crate::provider::native_search::{provider_family, ProviderFamily};
    let id = provider_id.to_ascii_lowercase();
    let has = |needle: &str| id.contains(needle);
    if has("deepseek") {
        return false;
    }
    match provider_family(provider_id) {
        ProviderFamily::Glm => return true,
        ProviderFamily::Kimi | ProviderFamily::Qwen => return false,
        ProviderFamily::Generic => {}
    }
    if has("zai") || has("bigmodel") {
        return true;
    }
    if has("anthropic") || has("claude") {
        return true;
    }
    if has("openai") || has("gpt") {
        return true;
    }
    false
}

/// Final default when `supports_images` is not explicitly configured: first consult the model-name
/// seed table. GLM support depends on the specific model: only vision models such as `glm-4v`,
/// `glm-4.1v`, `glm-4.5v`, and `glm-4.6v` support images; text-only models such as `glm-5.x` do not.
/// If no seed matches, fall back to the provider-family default from `default_supports_images`.
pub fn resolve_default_supports_images(provider_id: &str, model: &str) -> bool {
    crate::supports_images_seed::seed_by_model(provider_id, model)
        .unwrap_or_else(|| default_supports_images(provider_id))
}

/// 拼给用户的降级说明行（附件没随消息发出去时·附在该条用户消息文本末尾）。
pub fn degraded_notice(file_label: &str, model: &str) -> String {
    format!(
        "[附件图片 {file_label} 未随消息发送：当前模型（{model}）不支持图片输入，请勿声称已看到或读取该图片]"
    )
}

/// Degradation notice when the provider actually rejects images (`reason:"provider_rejected"`).
/// Say "rejected image input", not "the model does not support image input": the latter is a guess
/// and often wrong, since format, size, or image count may be the cause. Include a summary of the
/// provider's original error so callers and models see the actual failure instead of empty reassurance.
fn provider_rejected_notice(file_label: &str, provider_error: Option<&str>) -> String {
    match provider_error {
        Some(err) if !err.is_empty() => format!(
            "[附件图片 {file_label} 未随消息发送：当前模型/端点拒绝了图片输入（{err}），请勿声称已看到或读取该图片]"
        ),
        _ => format!(
            "[附件图片 {file_label} 未随消息发送：当前模型/端点拒绝了图片输入，请勿声称已看到或读取该图片]"
        ),
    }
}

/// Notice for a historical image reloaded on resume but omitted from the outgoing
/// message; wording must match `reason`. Previously, `source_missing`,
/// `source_changed`, and `too_many_images` all claimed the attachment was unavailable
/// because its source was missing or changed. Exceeding the 8-image limit implies
/// neither condition. Resume's final `save_conversation` persists this notice in
/// conversation text, so an inaccurate explanation would mislead every later model read.
fn stale_notice(file_label: &str, reason: &str) -> String {
    if reason == "too_many_images" {
        format!(
            "[附件图片 {file_label} 超出每轮 8 张上限，未随消息发送，请勿声称已看到或读取该图片]"
        )
    } else {
        format!(
            "[附件图片 {file_label} 未随消息发送：附件已不可用（原文件缺失或内容已变化），请勿声称已看到或读取该图片]"
        )
    }
}

fn image_label(image: &ImageBlock) -> String {
    image
        .source_path
        .as_ref()
        .and_then(|p| p.file_name())
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "(unknown)".to_string())
}

/// 唯一的调试日志出口（本仓没有 log/tracing 依赖）：`MYAGENT_DEBUG` 设了才吐，
/// 绝不吐到 stdout（不能污染 `--jsonl` 协议流）。
pub(crate) fn debug_log(msg: &str) {
    if std::env::var_os("MYAGENT_DEBUG").is_some() {
        eprintln!("[myagent debug] {msg}");
    }
}

/// Restore historical images after messages are loaded on resume. In conversation.json,
/// `skip_serializing` omits `data_base64`; deserialization always supplies an empty
/// string, leaving image metadata in memory without the image data.
/// Reread each `source_path` under all `load_one_image` constraints: stat against
/// `MAX_IMAGE_BYTES` before reading, validate magic bytes, and check the content
/// fingerprint. Previously, rereads bypassed all constraints of initial `--image`
/// loading. Successful rereads continue normally; missing paths, changed content,
/// or size violations remove the image, append an attachment-unavailable notice,
/// and emit `attachment.dropped` with `source_missing` or `source_changed` as `reason`.
/// Only empty `data_base64` from persisted and reloaded images needs restoration;
/// fresh `load_images` results have nonempty data and are skipped without rereading.
/// **Truncate to `MAX_IMAGES_PER_TURN` before rereading.** Previously, truncation
/// followed the reload loop, so images 9 through N were fully read and base64-encoded
/// before being discarded. N historical images near 10MB in conversation.json could
/// waste N x 10MB of disk reads and N x 13.3MB of allocations on discarded data.
/// Excess images now receive `too_many_images` without reading a single byte.
/// Reread the retained images individually under the same `MAX_IMAGES_PER_TURN`
/// hard limit as initial `--image` loading. For missing paths, changed content, or
/// exceeded limits, remove the image, append the corresponding notice, and emit
/// `attachment.dropped`; `reason` distinguishes `source_missing`, `source_changed`,
/// and `too_many_images`.
pub fn reload_stale_images(
    messages: &mut [crate::provider::ChatMessage],
    events: &mut crate::events::EventRecorder,
) -> Result<()> {
    for message in messages.iter_mut() {
        if message.images.is_empty() {
            continue;
        }
        let mut notice = String::new();
        let all_images = std::mem::take(&mut message.images);
        let (to_process, overflow) = if all_images.len() > MAX_IMAGES_PER_TURN {
            let mut all_images = all_images;
            let overflow = all_images.split_off(MAX_IMAGES_PER_TURN);
            (all_images, overflow)
        } else {
            (all_images, Vec::new())
        };
        // 超出上限的直接剥掉——不进下面的重读循环，不读盘、不算 base64。
        for image in &overflow {
            drop_one_stale_image(events, image, "too_many_images", &mut notice)?;
        }
        let mut kept = Vec::with_capacity(to_process.len());
        for mut image in to_process {
            if !image.data_base64.is_empty() {
                kept.push(image);
                continue;
            }
            match reload_one_image(&image) {
                Ok((data_base64, sha256)) => {
                    image.data_base64 = data_base64;
                    // Backfill the newly computed content fingerprint into `ImageBlock.sha256`.
                    // An older conversation.json without sha256 upgrades automatically after
                    // its first successful resume reread. Subsequent rereads compare content
                    // fingerprints instead of continually falling back to byte-count checks,
                    // which would otherwise leave same-length replacements undetectable forever.
                    image.sha256 = Some(sha256);
                    kept.push(image);
                }
                Err(reason) => {
                    drop_one_stale_image(events, &image, reason, &mut notice)?;
                }
            }
        }
        message.images = kept;
        if !notice.is_empty() {
            match &mut message.content {
                Some(content) => content.push_str(&notice),
                None => message.content = Some(notice.trim_start().to_string()),
            }
        }
    }
    Ok(())
}

/// Remove a historical image that failed to reload or exceeded a limit, emit
/// `attachment.dropped`, and append a notice selected by `reason`; see `stale_notice`.
fn drop_one_stale_image(
    events: &mut crate::events::EventRecorder,
    image: &ImageBlock,
    reason: &'static str,
    notice: &mut String,
) -> Result<()> {
    let label = image_label(image);
    events.emit(
        "attachment.dropped",
        serde_json::json!({
            "reason": reason,
            "file": label,
            "media_type": image.media_type,
            "bytes": image.bytes,
        }),
    )?;
    notice.push('\n');
    notice.push_str(&stale_notice(&label, reason));
    Ok(())
}

/// Reread a historical image from its recorded `source_path`. Success returns
/// `(newly encoded base64, computed hexadecimal sha256 fingerprint)`; failure returns
/// an event reason (`source_missing` | `source_changed`). Match `load_one_image`'s
/// validation order: use `metadata()` to check `MAX_IMAGE_BYTES` before a full read,
/// avoiding loading a stale file of hundreds of MB into memory just to reject it.
/// Then sniff magic bytes and verify content, preferring a recorded `sha256` hash:
/// byte count and media type alone cannot detect same-length content replacements.
/// For older conversation.json files without this field, fall back to byte-count
/// checks for compatibility and emit a `debug_log` line. The caller backfills the
/// returned fingerprint into `ImageBlock`, automatically upgrading older conversations.
fn reload_one_image(image: &ImageBlock) -> std::result::Result<(String, String), &'static str> {
    let path = image.source_path.as_ref().ok_or("source_missing")?;
    let meta = std::fs::metadata(path).map_err(|_| "source_missing")?;
    if !meta.is_file() {
        return Err("source_missing");
    }
    if meta.len() as u128 > MAX_IMAGE_BYTES as u128 {
        return Err("source_changed");
    }
    let bytes = std::fs::read(path).map_err(|_| "source_missing")?;
    if bytes.len() > MAX_IMAGE_BYTES {
        return Err("source_changed");
    }
    let format = sniff_format(&bytes).ok_or("source_changed")?;
    if format.media_type() != image.media_type {
        return Err("source_changed");
    }
    let computed_sha256 = sha256_hex(&bytes);
    match &image.sha256 {
        Some(expected) => {
            if &computed_sha256 != expected {
                return Err("source_changed");
            }
        }
        None => {
            debug_log(&format!(
                "reload_one_image: {} 没有记录 sha256（老 conversation.json），退回长度核对",
                path.display()
            ));
            if bytes.len() != image.bytes {
                return Err("source_changed");
            }
        }
    }
    use base64::Engine;
    Ok((
        base64::engine::general_purpose::STANDARD.encode(&bytes),
        computed_sha256,
    ))
}

/// 目标 provider 不支持图片时的诚实降级：不发 image 块——把图片从消息剥掉，并在该条用户
/// 消息文本末尾追加明确说明；每张被丢的图片记一条 `attachment.dropped` journal 事件。
/// capabilities.supports_images == true 时是 no-op（不动 messages）。
pub fn degrade_unsupported_images(
    messages: &mut [crate::provider::ChatMessage],
    capabilities: &crate::provider::ProviderCapabilities,
    events: &mut crate::events::EventRecorder,
) -> Result<()> {
    degrade_images_with_reason(
        messages,
        capabilities,
        events,
        "provider_no_image_support",
        None,
        true,
    )
}

/// Like `degrade_unsupported_images`, with a parameterized removal reason for recovery in production.
/// Use `"provider_rejected"` when the provider actually rejects an image request with HTTP 400;
/// the journal distinguishes this from `"provider_no_image_support"`, where configuration or the
/// seed table already indicated no image support. For `provider_rejected`, `provider_error` carries
/// a truncated summary of the provider's original error into both the event payload and notice;
/// pass `None` for other reasons. After a runtime override takes effect on the same provider instance,
/// subsequent turns must still remove images and append notices because wire copies are temporary
/// and need removal each turn. To avoid duplicate `attachment.dropped` events for the same image,
/// callers can set `emit_event` to `false`, retaining only image removal and notice updates.
#[allow(clippy::too_many_arguments)]
pub fn degrade_images_with_reason(
    messages: &mut [crate::provider::ChatMessage],
    capabilities: &crate::provider::ProviderCapabilities,
    events: &mut crate::events::EventRecorder,
    reason: &str,
    provider_error: Option<&str>,
    emit_event: bool,
) -> Result<()> {
    if capabilities.supports_images {
        return Ok(());
    }
    for message in messages.iter_mut() {
        if message.images.is_empty() {
            continue;
        }
        let dropped = std::mem::take(&mut message.images);
        let mut notice = String::new();
        for image in &dropped {
            let label = image_label(image);
            if emit_event {
                let mut payload = serde_json::json!({
                    "reason": reason,
                    "provider_id": capabilities.provider_id,
                    "model": capabilities.model_id,
                    "file": label,
                    "media_type": image.media_type,
                    "bytes": image.bytes,
                });
                if let Some(err) = provider_error {
                    payload["provider_error"] = serde_json::json!(err);
                }
                events.emit("attachment.dropped", payload)?;
            }
            notice.push('\n');
            if reason == "provider_rejected" {
                notice.push_str(&provider_rejected_notice(&label, provider_error));
            } else {
                notice.push_str(&degraded_notice(&label, &capabilities.model_id));
            }
        }
        match &mut message.content {
            Some(content) => content.push_str(&notice),
            None => message.content = Some(notice.trim_start().to_string()),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const PNG_MAGIC: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];

    #[test]
    fn sniff_png() {
        let mut bytes = PNG_MAGIC.to_vec();
        bytes.extend_from_slice(&[0, 1, 2, 3]);
        assert_eq!(sniff_format(&bytes), Some(ImageFormat::Png));
    }

    #[test]
    fn sniff_jpeg() {
        let bytes = [0xFF, 0xD8, 0xFF, 0xE0, 0, 1];
        assert_eq!(sniff_format(&bytes), Some(ImageFormat::Jpeg));
    }

    #[test]
    fn sniff_gif() {
        let bytes = b"GIF89a....";
        assert_eq!(sniff_format(bytes), Some(ImageFormat::Gif));
    }

    #[test]
    fn sniff_webp() {
        let mut bytes = b"RIFF".to_vec();
        bytes.extend_from_slice(&[0, 0, 0, 0]);
        bytes.extend_from_slice(b"WEBP");
        assert_eq!(sniff_format(&bytes), Some(ImageFormat::Webp));
    }

    #[test]
    fn sniff_rejects_plain_text_even_with_png_extension() {
        // 假扩展名：内容其实是文本——魔数嗅探必须拒绝，不看路径后缀。
        let bytes = b"this is not an image, just text pretending to be one";
        assert_eq!(sniff_format(bytes), None);
    }

    #[test]
    fn load_one_image_rejects_fake_extension() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fake.png");
        std::fs::write(&path, b"not actually a png").unwrap();
        let err = load_images(&[path]).unwrap_err();
        assert!(err.to_string().contains("not a recognized image"));
    }

    #[test]
    fn load_one_image_rejects_missing_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("missing.png");
        let err = load_images(&[path]).unwrap_err();
        assert!(err.to_string().contains("file not found"));
    }

    #[test]
    fn load_one_image_rejects_oversize() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("big.png");
        let mut bytes = PNG_MAGIC.to_vec();
        bytes.resize(MAX_IMAGE_BYTES + 1, 0u8);
        std::fs::write(&path, &bytes).unwrap();
        let err = load_images(&[path]).unwrap_err();
        assert!(err.to_string().contains("exceeds"));
    }

    #[test]
    fn load_images_rejects_too_many() {
        let dir = tempfile::tempdir().unwrap();
        let mut paths = Vec::new();
        for i in 0..(MAX_IMAGES_PER_TURN + 1) {
            let path = dir.path().join(format!("img{i}.png"));
            let mut bytes = PNG_MAGIC.to_vec();
            bytes.extend_from_slice(&[0, 1, 2, 3]);
            std::fs::write(&path, &bytes).unwrap();
            paths.push(path);
        }
        let err = load_images(&paths).unwrap_err();
        assert!(err.to_string().contains("too many"));
    }

    #[test]
    fn load_images_accepts_valid_png() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ok.png");
        let mut bytes = PNG_MAGIC.to_vec();
        bytes.extend_from_slice(&[9, 9, 9]);
        std::fs::write(&path, &bytes).unwrap();
        let images = load_images(&[path]).unwrap();
        assert_eq!(images.len(), 1);
        assert_eq!(images[0].media_type, "image/png");
        assert_eq!(images[0].bytes, bytes.len());
        assert!(!images[0].data_base64.is_empty());
    }

    #[test]
    fn image_block_serialize_skips_base64_data() {
        let block = ImageBlock {
            media_type: "image/png".into(),
            data_base64: "verysecretbase64".into(),
            source_path: Some(PathBuf::from("/tmp/x.png")),
            bytes: 123,
            sha256: None,
        };
        let s = serde_json::to_string(&block).unwrap();
        assert!(!s.contains("verysecretbase64"));
        assert!(!s.contains("data_base64"));
        assert!(s.contains("\"bytes\":123"));
    }

    fn test_recorder(dir: &Path) -> crate::events::EventRecorder {
        crate::events::EventRecorder::new(
            "r1",
            None,
            None,
            &dir.join("events.jsonl"),
            crate::events::OutputMode::Silent,
        )
        .unwrap()
    }

    fn caps(supports_images: bool) -> crate::provider::ProviderCapabilities {
        crate::provider::ProviderCapabilities {
            provider_id: "deepseek".into(),
            model_id: "deepseek-v4-flash".into(),
            supports_streaming: true,
            supports_reasoning_deltas: false,
            supports_tool_calling: true,
            supports_images,
            supports_computer_use: false,
            supports_shell_tool: true,
            max_context_tokens: None,
            output_token_limit: None,
            server_side_search: false,
        }
    }

    #[test]
    fn degrade_is_noop_when_provider_supports_images() {
        let dir = tempfile::tempdir().unwrap();
        let mut events = test_recorder(dir.path());
        let mut messages = vec![crate::provider::ChatMessage::user_with_images(
            "look at this",
            vec![ImageBlock {
                media_type: "image/png".into(),
                data_base64: "QUJD".into(),
                source_path: Some(PathBuf::from("/tmp/shot.png")),
                bytes: 3,
                sha256: None,
            }],
        )];
        degrade_unsupported_images(&mut messages, &caps(true), &mut events).unwrap();
        assert_eq!(messages[0].images.len(), 1);
        assert_eq!(messages[0].content.as_deref(), Some("look at this"));
    }

    #[test]
    fn degrade_strips_images_and_appends_notice_and_emits_event() {
        let dir = tempfile::tempdir().unwrap();
        let mut events = test_recorder(dir.path());
        let mut messages = vec![crate::provider::ChatMessage::user_with_images(
            "look at this",
            vec![ImageBlock {
                media_type: "image/png".into(),
                data_base64: "QUJD".into(),
                source_path: Some(PathBuf::from("/tmp/shot.png")),
                bytes: 3,
                sha256: None,
            }],
        )];
        degrade_unsupported_images(&mut messages, &caps(false), &mut events).unwrap();
        assert!(messages[0].images.is_empty());
        let content = messages[0].content.as_deref().unwrap();
        assert!(content.starts_with("look at this"));
        assert!(content.contains("shot.png"));
        assert!(content.contains("deepseek-v4-flash"));
        assert!(content.contains("请勿声称已看到或读取该图片"));

        let journal = std::fs::read_to_string(dir.path().join("events.jsonl")).unwrap();
        assert!(journal.contains("attachment.dropped"));
        assert!(journal.contains("shot.png"));
        assert!(journal.contains("provider_no_image_support"));
    }

    #[test]
    fn default_supports_images_table() {
        assert!(default_supports_images("glm"));
        assert!(default_supports_images("zai"));
        assert!(default_supports_images("anthropic"));
        assert!(default_supports_images("claude"));
        assert!(default_supports_images("openai"));
        assert!(!default_supports_images("deepseek"));
        assert!(!default_supports_images("kimi"));
        assert!(!default_supports_images("moonshot"));
        assert!(!default_supports_images("some-unknown-provider"));
    }

    #[test]
    fn default_supports_images_recognizes_bigmodel_alias_via_provider_family() {
        // The official GLM domain family may use "bigmodel" as its id, with no glm/zhipu/zai
        // substring. Recognize this alias alongside `provider_family` grouping so fail-closed
        // detection does not incorrectly reject image support.
        assert!(default_supports_images("bigmodel"));
        // qwen 家族现在也走 provider_family 的分组（Qwen => false），与原子串猜行为一致。
        assert!(!default_supports_images("qwen"));
    }

    #[test]
    fn load_one_image_rejects_oversize_before_reading_full_file_into_memory() {
        // Verify stat checks size before any full read: a 1.5 GiB sparse file should be
        // rejected after one `metadata()` call without loading the entire file into memory.
        // Sparse files occupy few disk blocks on most filesystems, so assert elapsed time
        // rather than RSS for portability: expect microsecond-scale stat work, not a 1.5 GiB read.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("huge.png");
        let f = std::fs::File::create(&path).unwrap();
        f.set_len(1_610_612_736).unwrap(); // 1.5 GiB，稀疏（不占实际磁盘块）
        drop(f);
        {
            use std::io::Write;
            let mut f = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
            f.write_all(&PNG_MAGIC).unwrap();
        }
        let start = std::time::Instant::now();
        let err = load_images(&[path]).unwrap_err();
        let elapsed = start.elapsed();
        assert!(err.to_string().contains("exceeds"));
        assert!(
            // A 50ms wall-clock limit can falsely fail on busy CI machines or volumes without
            // sparse-file support, where creating this 1.5 GiB file is costly. Allowing 500ms
            // still guards against reading all 1.5 GiB before rejection (hundreds of milliseconds
            // to seconds); this assertion does not aim to distinguish microsecond-scale stat work.
            elapsed < std::time::Duration::from_millis(500),
            "先 stat 判大小应在毫秒级内拒绝，不应读完 1.5 GiB：实测 {elapsed:?}"
        );
    }
}

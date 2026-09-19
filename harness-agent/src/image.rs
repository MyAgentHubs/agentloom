//! 图片附件加载 + provider 图片能力默认表（纯逻辑·CLI/orchestrator 复用）。
//!
//! 设计要点（见 T1 brief）：
//! - 只信魔数，不信扩展名——上传一个改了后缀的假图必须被拒。
//! - 单张 ≤ 10 MB、单轮 ≤ 8 张，超限直接硬报错（哪张为什么）。
//! - `ImageBlock` 里的 base64 数据只在内存里流转，绝不落盘到 journal / conversation.json
//!   （由 `ImageBlock` 的 `Serialize` 跳过 `data_base64` 保证）。

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
    /// 内容指纹（十六进制 SHA-256），首次加载时算好——resume 重读时优先比这个，
    /// 而不是只比字节数+media_type（t12-img 第三轮 opus 审 P2-2：同长度换内容检测
    /// 不到，历史图片会被静默掉包）。`#[serde(default)]`：老 conversation.json 没有
    /// 这个字段，反序列化补 `None`，重读时退回原来的长度核对，向后兼容。
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

/// 十六进制 SHA-256（P2-2 的内容指纹）。逐字节手写十六进制，不依赖 digest 输出类型
/// 是否实现 `LowerHex`（sha2 0.11 的 `Array` 输出类型不像旧版 `GenericArray` 那样有
/// 该 impl）。
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

/// 未显式配置 supports_images 时的最终默认值：先查 model 名种子表（T19：GLM 家族按
/// 型号判——只有 `glm-4v`/`glm-4.1v`/`glm-4.5v`/`glm-4.6v` 这类视觉型号吃图，`glm-5.x`
/// 等文本型号不吃），查不到再回落 `default_supports_images` 的 provider 家族默认。
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

/// T19 P2-1：厂商实际拒图（`reason:"provider_rejected"`）时的降级说明行——不说「模型不
/// 支持图片输入」（这是猜的，且经常是错的：真实原因可能是格式/尺寸/张数问题），改说
/// 「拒绝了图片输入」并附厂商原文摘要，让用户/模型看得到真相而不是被安慰式话术糊弄。
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

/// resume 载回历史图片、但该图片没能随消息发出去时的说明行——措辞按 `reason` 分支。
/// t12-img 第四轮返工（P3-F）：此前不管 `source_missing`/`source_changed` 还是
/// `too_many_images` 一律复用「附件已不可用（原文件缺失或内容已变化）」这句话——超 8 张
/// 上限那条路径里文件既没缺失也没变，这句话是假的，且会被 resume 收尾 `save_conversation`
/// 固化进会话文本、模型此后每次都读到一句撒谎的说明。
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

/// resume 载回历史消息后调用：conversation.json 落盘时 `data_base64` 被
/// `skip_serializing` 跳过，反序列化永远补空串——这批"图片"元信息还在，但数据已经
/// 不在这份内存里了。逐条按 `source_path` 重读 + 复用 `load_one_image` 的前置约束
/// （先 stat 判 `MAX_IMAGE_BYTES` 再读、魔数校验、内容指纹核对——t12-img 第三轮 opus
/// 审 P2-3：重读此前完全不受 `--image` 首次加载的任何一条约束），成功则续接正常
/// 出线；失败（路径不存在 / 内容已变化 / 超限）则剥掉该图 + 追加"附件已不可用"说明 +
/// 记一条 `attachment.dropped`（reason 区分 `source_missing` / `source_changed`）。
/// 只处理"落盘再载回"产生的空 `data_base64`——本轮刚 `load_images` 出来的图片
/// `data_base64` 非空，直接跳过，不重读。**先按 `MAX_IMAGES_PER_TURN` 截尾、再重读**
/// （t12-img 第四轮返工 P3-G：此前截尾排在重读循环之后，第 9…N 张会先被完整读盘+
/// base64 编码才被丢掉——一个 conversation.json 列了 N 张逼近 10MB 的历史图，就是
/// N×10MB 读盘 + N×13.3MB 分配换来的全部作废；现在超出上限的直接进
/// `too_many_images`，一个字节都不读）；截尾之后再对保留的那批逐条重读，同样按
/// `MAX_IMAGES_PER_TURN` 硬顶——`--image` 首次加载已有这条约束，重读这里补齐
/// （P2-3）。失败（路径不存在 / 内容已变化 / 超限）则剥掉该图 + 追加对应说明 + 记一条
/// `attachment.dropped`（reason 区分 `source_missing` / `source_changed` /
/// `too_many_images`）。
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
                    // t12-img 第四轮返工（P3-C）：把刚算出来的指纹回填进
                    // `ImageBlock.sha256`——老 conversation.json（无 sha256 字段）第一次
                    // resume 成功之后就自愈升级成有指纹的，此后每次重读都走 P2-2 的
                    // 内容指纹比对，不会一直退回长度核对（否则「同长度换内容」这个洞对
                    // 这份会话永远敞着）。
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

/// 剥掉一张重读失败/超限的历史图片：记一条 `attachment.dropped` + 追加一行说明
/// （说明文案按 `reason` 分支，见 `stale_notice`——P3-F）。
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

/// 按记录的 `source_path` 重读一张历史图片；成功返回 `(新编码的 base64, 算出的
/// sha256 十六进制指纹)`，失败返回事件 reason（`source_missing` | `source_changed`）。
/// 前置校验序列与 `load_one_image` 对齐：先 `metadata()` 判 `MAX_IMAGE_BYTES` 再决定
/// 要不要整读（防一个几百 MB 的陈旧路径被整读进内存才判超限——P2-3），再魔数嗅探，
/// 最后核对内容——有记录的 `sha256` 时优先比哈希（P2-2：同长度换内容此前检测不到）；
/// 老 conversation.json 没有这个字段时退回原来的字节数核对（向后兼容，t12-img 第四轮
/// 返工 P3-C：这一路径会 `debug_log` 一行，调用方把返回的指纹回填进 `ImageBlock`
/// 让老会话自愈升级）。
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

/// 同 `degrade_unsupported_images`，但剥图原因可参数化（T19：出线自愈——厂商实际
/// 拿 400 拒了带图请求——用 `"provider_rejected"`，与「配置/种子表本就判定不支持」
/// 的 `"provider_no_image_support"` 在 journal 里区分开）。`provider_error`（T19 P2-1）：
/// `provider_rejected` 场景下厂商原文截断摘要，随事件 payload 记一份、也拼进说明行；其它
/// 原因传 `None`。`emit_event`（T19 P2-2）：同一 provider 实例运行时覆盖已生效后的后续
/// 轮次仍要剥图+追加说明（wire 副本是临时的，每轮都得剥），但不该对同一张图片重复记
/// `attachment.dropped`——调用方传 `false` 抑制事件、只做剥图+文案。
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
        // P3-1：GLM 官方域名家族的 id 可能是 "bigmodel" 而不含 glm/zhipu/zai 子串——
        // 复用 provider_family 之外再认这个别名，不能被 fail-closed 误判成不支持图片。
        assert!(default_supports_images("bigmodel"));
        // qwen 家族现在也走 provider_family 的分组（Qwen => false），与原子串猜行为一致。
        assert!(!default_supports_images("qwen"));
    }

    #[test]
    fn load_one_image_rejects_oversize_before_reading_full_file_into_memory() {
        // P2-3：先 stat 判大小再决定要不要整读——1.5 GiB 稀疏文件只应触发一次 metadata()
        // 调用就被拒，不应把整个文件读进内存。稀疏文件在多数文件系统上实际占用块很小，
        // 用耗时而不是 RSS 断言（更可移植），预期是微秒级 stat，不是读 1.5 GiB 的量级。
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
            // P3-2：50ms 挂钟阈值在负载高的 CI 机器 / 不支持稀疏文件的卷上会假红
            // （且要建 1.5 GiB 稀疏文件）；放宽到 500ms 仍能挡住"整读 1.5 GiB 才拒绝"
            // 这种量级的回归（那至少是百毫秒到秒级），不追求区分微秒级 stat。
            elapsed < std::time::Duration::from_millis(500),
            "先 stat 判大小应在毫秒级内拒绝，不应读完 1.5 GiB：实测 {elapsed:?}"
        );
    }
}

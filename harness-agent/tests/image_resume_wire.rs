//! t12-img 双路审 P1-1 回归：resume 载回历史图片后，`data_base64` 因落盘时
//! `skip_serializing` 必为空。修前会把这条"空 data URI"的历史图片原样重发给
//! provider；修后必须按 `source_path` 重读+重新魔数校验，成功续接真数据，
//! 失败（源文件缺失/内容已变）则剥图+说明+`attachment.dropped`。
//! 用真实 `OpenAiCompatibleProvider` + wiremock 抓 wire body，不是纸上推理。

use std::sync::{Arc, Mutex};

use myagent::config::SearchChoice;
use myagent::events::OutputMode;
use myagent::exec::sandbox::FsWriteFence;
use myagent::fs_scope::FsReadScope;
use myagent::goal::NetworkPolicy;
use myagent::image::ImageBlock;
use myagent::journal::{save_conversation, RunPaths, SavedConversation};
use myagent::orchestrator::{resume_solo_with_judge_and_fs_scope, ControlInputKind};
use myagent::provider::openai_compatible::{OpenAiCompatibleConfig, OpenAiCompatibleProvider};
use myagent::provider::ChatMessage;
use myagent::shell::PermissionPolicy;
use serde_json::Value;
use wiremock::matchers::method;
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

struct Capture(Arc<Mutex<Vec<Value>>>);

impl Respond for Capture {
    fn respond(&self, req: &Request) -> ResponseTemplate {
        self.0
            .lock()
            .unwrap()
            .push(serde_json::from_slice(&req.body).unwrap());
        ResponseTemplate::new(200)
            .insert_header("content-type", "text/event-stream")
            .set_body_string(
                "data: {\"choices\":[{\"delta\":{\"content\":\"ok\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n",
            )
    }
}

fn provider(base: &str) -> OpenAiCompatibleProvider {
    OpenAiCompatibleProvider::new(OpenAiCompatibleConfig {
        provider_id: "glm".into(),
        api_key: "sk".into(),
        base_url: format!("{base}/v1"),
        // T19：GLM 现在按型号判 supports_images，"glm-5.2" 是纯文本型号（判 false）——
        // 这批测试的意图是练"provider 支持图片"的 resume 重读路径，换成真实视觉型号。
        model: "glm-4.5v".into(),
        timeout_secs: 5,
        native_search_enabled: false,
        ..Default::default()
    })
    .unwrap()
}

const PNG_MAGIC: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];

fn png_bytes(payload: &[u8]) -> Vec<u8> {
    let mut b = PNG_MAGIC.to_vec();
    b.extend_from_slice(payload);
    b
}

/// 两条 user 消息都会出线（历史那条 + 本轮追加的续聊）——取那条曾经带图的历史消息
/// （文本以 "look at this" 开头），不是最新一轮的续聊消息。
async fn historical_image_bearing_message(cap: &Arc<Mutex<Vec<Value>>>) -> Value {
    let last = cap.lock().unwrap().last().cloned().unwrap();
    last["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| {
            m["role"] == "user"
                && m["content"]
                    .as_str()
                    .is_some_and(|c| c.starts_with("look at this"))
        })
        .cloned()
        .unwrap()
}

#[tokio::test]
async fn resume_reloads_stale_image_from_source_path_and_sends_real_data() {
    let ws = tempfile::tempdir().unwrap();
    let cap = Arc::new(Mutex::new(Vec::new()));
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(Capture(cap.clone()))
        .mount(&server)
        .await;

    let bytes = png_bytes(b"REAL-IMAGE-BYTES");
    let shot = ws.path().join("shot.png");
    std::fs::write(&shot, &bytes).unwrap();

    let run_id = "resume_stale_image";
    let paths = RunPaths::new(ws.path(), run_id);
    paths.create_dirs().unwrap();
    save_conversation(
        &paths.conversation_path,
        &SavedConversation {
            run_id: run_id.to_string(),
            provider: "glm".to_string(),
            model: "glm-5.2".to_string(),
            messages: vec![
                ChatMessage::system("system prompt"),
                ChatMessage::user_with_images(
                    "look at this",
                    vec![ImageBlock {
                        media_type: "image/png".into(),
                        // 落盘时 `data_base64` 永远被 skip_serializing 跳过，反序列化补空串——
                        // 这里直接构造出"已落盘再载回"的真实形状，不是凑数据。
                        data_base64: String::new(),
                        source_path: Some(shot.clone()),
                        bytes: bytes.len(),
                        // 老 conversation.json 没有这个字段（P2-2 之前写的）——退回长度核对。
                        sha256: None,
                    }],
                ),
                ChatMessage::assistant("I see a screenshot.", None, vec![]),
            ],
        },
    )
    .unwrap();

    resume_solo_with_judge_and_fs_scope(
        provider(&server.uri()),
        Box::new(myagent::judge::NoopJudge),
        ws.path(),
        ws.path().to_path_buf(),
        run_id.to_string(),
        Some("and now what do you see?".into()),
        OutputMode::Silent,
        PermissionPolicy::Allow,
        NetworkPolicy::On,
        FsReadScope::Workspace,
        Vec::new(),
        FsWriteFence::Off,
        1,
        ControlInputKind::Sentinel,
        false,
        false,
        SearchChoice::Ddg,
        Default::default(),
        0,
        0,
        None,
        Vec::new(),
        Vec::new(),
    )
    .await
    .unwrap();

    let last = cap.lock().unwrap().last().cloned().unwrap();
    let body_str = last.to_string();
    assert!(
        !body_str.contains("base64,\""),
        "wire 上不应出现空 data URI（无 base64 payload 就紧跟结尾引号）：{body_str}"
    );

    let msg = last["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["role"] == "user" && m["content"].is_array())
        .cloned()
        .expect("历史带图 user 消息必须仍带 image_url 块（数据已重读成功）");
    let content = msg["content"].as_array().unwrap();
    let image_part = content
        .iter()
        .find(|p| p["type"] == "image_url")
        .expect("重读成功的图片必须仍出线");
    use base64::Engine;
    let expected = base64::engine::general_purpose::STANDARD.encode(&bytes);
    assert_eq!(
        image_part["image_url"]["url"],
        format!("data:image/png;base64,{expected}"),
        "重读回来的图片必须是磁盘上的真实字节，不是空 payload"
    );
}

#[tokio::test]
async fn resume_backfills_sha256_for_old_conversation_without_it() {
    // t12-img 第四轮返工（P3-C）：老 conversation.json（无 `sha256` 字段）第一次 resume
    // 成功重读后，必须把刚算出来的指纹回填进落盘的 `ImageBlock.sha256`——否则这份会话
    // 此后每次 resume 都退回长度核对，P2-2 修的「同长度换内容检测不到」这个洞对它永远
    // 敞着。断言直接读磁盘上 resume 收尾落盘的 conversation.json，不是看 wire。
    let ws = tempfile::tempdir().unwrap();
    let cap = Arc::new(Mutex::new(Vec::new()));
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(Capture(cap.clone()))
        .mount(&server)
        .await;

    let bytes = png_bytes(b"BACKFILL-SHA256-PAYLOAD");
    let shot = ws.path().join("shot.png");
    std::fs::write(&shot, &bytes).unwrap();

    use sha2::{Digest, Sha256};
    let expected_sha256: String = Sha256::digest(&bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();

    let run_id = "resume_backfill_sha256";
    let paths = RunPaths::new(ws.path(), run_id);
    paths.create_dirs().unwrap();
    save_conversation(
        &paths.conversation_path,
        &SavedConversation {
            run_id: run_id.to_string(),
            provider: "glm".to_string(),
            model: "glm-5.2".to_string(),
            messages: vec![
                ChatMessage::system("system prompt"),
                ChatMessage::user_with_images(
                    "look at this",
                    vec![ImageBlock {
                        media_type: "image/png".into(),
                        data_base64: String::new(),
                        source_path: Some(shot.clone()),
                        bytes: bytes.len(),
                        // 老 conversation.json：没有这个字段（P2-2 之前写的）。
                        sha256: None,
                    }],
                ),
                ChatMessage::assistant("I see a screenshot.", None, vec![]),
            ],
        },
    )
    .unwrap();

    resume_solo_with_judge_and_fs_scope(
        provider(&server.uri()),
        Box::new(myagent::judge::NoopJudge),
        ws.path(),
        ws.path().to_path_buf(),
        run_id.to_string(),
        Some("and now what do you see?".into()),
        OutputMode::Silent,
        PermissionPolicy::Allow,
        NetworkPolicy::On,
        FsReadScope::Workspace,
        Vec::new(),
        FsWriteFence::Off,
        1,
        ControlInputKind::Sentinel,
        false,
        false,
        SearchChoice::Ddg,
        Default::default(),
        0,
        0,
        None,
        Vec::new(),
        Vec::new(),
    )
    .await
    .unwrap();

    let saved: myagent::journal::SavedConversation<ChatMessage> =
        myagent::journal::load_conversation(&paths.conversation_path).unwrap();
    let image = saved
        .messages
        .iter()
        .flat_map(|m| m.images.iter())
        .find(|img| img.source_path.as_deref() == Some(shot.as_path()))
        .expect("落盘的历史消息必须仍带这张图片的元信息");
    assert_eq!(
        image.sha256.as_deref(),
        Some(expected_sha256.as_str()),
        "第一次 resume 重读成功后，落盘的 sha256 必须自愈回填，不能一直是 None"
    );
}

#[tokio::test]
async fn resume_drops_stale_image_when_source_file_missing_and_appends_notice() {
    let ws = tempfile::tempdir().unwrap();
    let cap = Arc::new(Mutex::new(Vec::new()));
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(Capture(cap.clone()))
        .mount(&server)
        .await;

    // 源文件从未落地（模拟"路径不存在"）：/tmp 下一个不存在的名字。
    let missing = ws.path().join("gone.png");

    let run_id = "resume_missing_image";
    let paths = RunPaths::new(ws.path(), run_id);
    paths.create_dirs().unwrap();
    save_conversation(
        &paths.conversation_path,
        &SavedConversation {
            run_id: run_id.to_string(),
            provider: "glm".to_string(),
            model: "glm-5.2".to_string(),
            messages: vec![
                ChatMessage::system("system prompt"),
                ChatMessage::user_with_images(
                    "look at this",
                    vec![ImageBlock {
                        media_type: "image/png".into(),
                        data_base64: String::new(),
                        source_path: Some(missing),
                        bytes: 42,
                        sha256: None,
                    }],
                ),
                ChatMessage::assistant("I see a screenshot.", None, vec![]),
            ],
        },
    )
    .unwrap();

    resume_solo_with_judge_and_fs_scope(
        provider(&server.uri()),
        Box::new(myagent::judge::NoopJudge),
        ws.path(),
        ws.path().to_path_buf(),
        run_id.to_string(),
        Some("and now what do you see?".into()),
        OutputMode::Silent,
        PermissionPolicy::Allow,
        NetworkPolicy::On,
        FsReadScope::Workspace,
        Vec::new(),
        FsWriteFence::Off,
        1,
        ControlInputKind::Sentinel,
        false,
        false,
        SearchChoice::Ddg,
        Default::default(),
        0,
        0,
        None,
        Vec::new(),
        Vec::new(),
    )
    .await
    .unwrap();

    let msg = historical_image_bearing_message(&cap).await;
    assert!(
        msg["content"].is_string(),
        "剥掉图片后该条历史消息应回落成纯文本 content：{msg}"
    );
    let text = msg["content"].as_str().unwrap();
    assert!(
        text.contains("附件已不可用"),
        "剥图后必须追加中文说明：{text}"
    );
    assert!(!msg.to_string().contains("base64"));

    let events_path = paths.events_path.clone();
    let journal = std::fs::read_to_string(events_path).unwrap();
    assert!(
        journal.contains("attachment.dropped"),
        "必须记一条 attachment.dropped"
    );
    assert!(
        journal.contains("source_missing"),
        "reason 必须区分 source_missing：{journal}"
    );
}

#[tokio::test]
async fn resume_drops_stale_image_when_source_file_content_changed() {
    let ws = tempfile::tempdir().unwrap();
    let cap = Arc::new(Mutex::new(Vec::new()));
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(Capture(cap.clone()))
        .mount(&server)
        .await;

    let shot = ws.path().join("shot.png");
    // 记录时的 bytes(=99999) 与 media_type 与磁盘上现在的真实内容不符——模拟"内容已变化"。
    std::fs::write(&shot, png_bytes(b"DIFFERENT-NOW")).unwrap();

    let run_id = "resume_changed_image";
    let paths = RunPaths::new(ws.path(), run_id);
    paths.create_dirs().unwrap();
    save_conversation(
        &paths.conversation_path,
        &SavedConversation {
            run_id: run_id.to_string(),
            provider: "glm".to_string(),
            model: "glm-5.2".to_string(),
            messages: vec![
                ChatMessage::system("system prompt"),
                ChatMessage::user_with_images(
                    "look at this",
                    vec![ImageBlock {
                        media_type: "image/png".into(),
                        data_base64: String::new(),
                        source_path: Some(shot),
                        bytes: 99999,
                        sha256: None,
                    }],
                ),
                ChatMessage::assistant("I see a screenshot.", None, vec![]),
            ],
        },
    )
    .unwrap();

    resume_solo_with_judge_and_fs_scope(
        provider(&server.uri()),
        Box::new(myagent::judge::NoopJudge),
        ws.path(),
        ws.path().to_path_buf(),
        run_id.to_string(),
        Some("and now?".into()),
        OutputMode::Silent,
        PermissionPolicy::Allow,
        NetworkPolicy::On,
        FsReadScope::Workspace,
        Vec::new(),
        FsWriteFence::Off,
        1,
        ControlInputKind::Sentinel,
        false,
        false,
        SearchChoice::Ddg,
        Default::default(),
        0,
        0,
        None,
        Vec::new(),
        Vec::new(),
    )
    .await
    .unwrap();

    let msg = historical_image_bearing_message(&cap).await;
    assert!(msg["content"].is_string());
    assert!(!msg.to_string().contains("base64"));

    let journal = std::fs::read_to_string(&paths.events_path).unwrap();
    assert!(journal.contains("attachment.dropped"));
    assert!(
        journal.contains("source_changed"),
        "reason 必须区分 source_changed：{journal}"
    );
}

// ---------- t12-img 第三轮 opus 审 P2-2：同长度替换检测不到 ----------

#[tokio::test]
async fn resume_detects_same_length_content_swap_via_sha256() {
    // P2-2：`reload_one_image` 此前只比字节数 + media_type——同长度换内容检测不到，
    // 历史图片会被静默掉包（会话文本仍写着原话，journal 一个字不记）。
    // `ImageBlock.sha256` 首次加载时算好，重读优先比哈希：字节数相同、内容不同也必须
    // 判 `source_changed`。
    let ws = tempfile::tempdir().unwrap();
    let cap = Arc::new(Mutex::new(Vec::new()));
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(Capture(cap.clone()))
        .mount(&server)
        .await;

    let shot = ws.path().join("shot.png");
    let original = png_bytes(b"ORIGINAL-PAYLOAD");
    std::fs::write(&shot, &original).unwrap();

    use sha2::{Digest, Sha256};
    let original_sha256: String = Sha256::digest(&original)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();

    let run_id = "resume_same_length_swap";
    let paths = RunPaths::new(ws.path(), run_id);
    paths.create_dirs().unwrap();
    save_conversation(
        &paths.conversation_path,
        &SavedConversation {
            run_id: run_id.to_string(),
            provider: "glm".to_string(),
            model: "glm-5.2".to_string(),
            messages: vec![
                ChatMessage::system("system prompt"),
                ChatMessage::user_with_images(
                    "look at this",
                    vec![ImageBlock {
                        media_type: "image/png".into(),
                        data_base64: String::new(),
                        source_path: Some(shot.clone()),
                        bytes: original.len(),
                        sha256: Some(original_sha256),
                    }],
                ),
                ChatMessage::assistant("I see a screenshot.", None, vec![]),
            ],
        },
    )
    .unwrap();

    // 同长度替换：只翻转最后一个字节，长度必然与 original 相同。
    let mut swapped = original.clone();
    let last = swapped.len() - 1;
    swapped[last] ^= 0xFF;
    assert_eq!(
        swapped.len(),
        original.len(),
        "夹具必须同长度才能验证这条修复"
    );
    std::fs::write(&shot, &swapped).unwrap();

    resume_solo_with_judge_and_fs_scope(
        provider(&server.uri()),
        Box::new(myagent::judge::NoopJudge),
        ws.path(),
        ws.path().to_path_buf(),
        run_id.to_string(),
        Some("and now?".into()),
        OutputMode::Silent,
        PermissionPolicy::Allow,
        NetworkPolicy::On,
        FsReadScope::Workspace,
        Vec::new(),
        FsWriteFence::Off,
        1,
        ControlInputKind::Sentinel,
        false,
        false,
        SearchChoice::Ddg,
        Default::default(),
        0,
        0,
        None,
        Vec::new(),
        Vec::new(),
    )
    .await
    .unwrap();

    let msg = historical_image_bearing_message(&cap).await;
    assert!(
        msg["content"].is_string(),
        "同长度内容被掉包后必须剥图回落成纯文本：{msg}"
    );
    assert!(!msg.to_string().contains("base64"));

    let journal = std::fs::read_to_string(&paths.events_path).unwrap();
    assert!(journal.contains("attachment.dropped"));
    assert!(
        journal.contains("source_changed"),
        "同长度替换也要判定 source_changed：{journal}"
    );
}

// ---------- t12-img 第三轮 opus 审 P2-3：重读不走 --image 的任何前置约束 ----------

#[tokio::test]
async fn resume_rejects_oversize_source_quickly_without_reading_full_file() {
    // P2-3：`reload_one_image` 此前直接整读——600 MiB 稀疏文件 + 记录的 `bytes` 必然
    // 不匹配也要等整读完才拒。修后必须先 `metadata()` 判 `MAX_IMAGE_BYTES` 再决定要不要
    // 读，拒绝耗时应远小于整读同一文件一次的耗时。
    //
    // t12-img 第四轮返工（P2-B）：绝对阈值（曾用 500ms）在这个断言上没有牙——本机测过
    // 修复前整读 600 MiB 稀疏文件只要 ~212ms，早就落在任何「宽松到防 CI 抖动」的绝对
    // 阈值以内，删掉 `metadata()` 预检这条回归照样测不出来。改成自校准：先在本测试里把
    // 同一个文件整读一遍量出 `full`，再断言重读耗时 `< full / 10`——本机实测两者相差
    // 两个数量级（fix 前 212ms vs fix 后 ~2ms），负载高低同时影响分子分母，比值稳定。
    let ws = tempfile::tempdir().unwrap();
    let cap = Arc::new(Mutex::new(Vec::new()));
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(Capture(cap.clone()))
        .mount(&server)
        .await;

    let huge = ws.path().join("huge.png");
    let f = std::fs::File::create(&huge).unwrap();
    f.set_len(629_145_600).unwrap(); // 600 MiB，稀疏（不占实际磁盘块）
    drop(f);
    {
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new().write(true).open(&huge).unwrap();
        f.write_all(&PNG_MAGIC).unwrap();
    }

    // 自校准基线：整读同一个文件一次，量出这台机器 / 这次负载下「读 600 MiB」的真实耗时。
    let full_started = std::time::Instant::now();
    let full_bytes = std::fs::read(&huge).unwrap();
    let full = full_started.elapsed();
    assert_eq!(
        full_bytes.len(),
        629_145_600,
        "sanity：整读应读到全部 600 MiB"
    );
    drop(full_bytes);

    let run_id = "resume_oversize_source";
    let paths = RunPaths::new(ws.path(), run_id);
    paths.create_dirs().unwrap();
    save_conversation(
        &paths.conversation_path,
        &SavedConversation {
            run_id: run_id.to_string(),
            provider: "glm".to_string(),
            model: "glm-5.2".to_string(),
            messages: vec![
                ChatMessage::system("system prompt"),
                ChatMessage::user_with_images(
                    "look at this",
                    vec![ImageBlock {
                        media_type: "image/png".into(),
                        data_base64: String::new(),
                        source_path: Some(huge),
                        bytes: 42,
                        sha256: None,
                    }],
                ),
                ChatMessage::assistant("I see a screenshot.", None, vec![]),
            ],
        },
    )
    .unwrap();

    let start = std::time::Instant::now();
    resume_solo_with_judge_and_fs_scope(
        provider(&server.uri()),
        Box::new(myagent::judge::NoopJudge),
        ws.path(),
        ws.path().to_path_buf(),
        run_id.to_string(),
        Some("and now?".into()),
        OutputMode::Silent,
        PermissionPolicy::Allow,
        NetworkPolicy::On,
        FsReadScope::Workspace,
        Vec::new(),
        FsWriteFence::Off,
        1,
        ControlInputKind::Sentinel,
        false,
        false,
        SearchChoice::Ddg,
        Default::default(),
        0,
        0,
        None,
        Vec::new(),
        Vec::new(),
    )
    .await
    .unwrap();
    let elapsed = start.elapsed();
    assert!(
        elapsed < full / 10,
        "先 stat 判大小应远快于整读同一文件一次：整读 {full:?} vs 重读拒绝 {elapsed:?}\
         （应 < 整读 / 10，自校准防 CI 机器负载假红）"
    );

    let msg = historical_image_bearing_message(&cap).await;
    assert!(msg["content"].is_string(), "超限图片必须被剥掉：{msg}");
    assert!(!msg.to_string().contains("base64"));
    let journal = std::fs::read_to_string(&paths.events_path).unwrap();
    assert!(journal.contains("attachment.dropped"));
}

#[tokio::test]
async fn resume_drops_image_that_grew_past_max_bytes_on_disk() {
    // P2-3：`--image` 首次加载有 `MAX_IMAGE_BYTES`(10MB) 硬顶；重读此前完全不受这条
    // 约束——一张 12MB 的图会被照收整发。
    let ws = tempfile::tempdir().unwrap();
    let cap = Arc::new(Mutex::new(Vec::new()));
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(Capture(cap.clone()))
        .mount(&server)
        .await;

    let big = ws.path().join("big.png");
    let mut bytes = PNG_MAGIC.to_vec();
    bytes.resize(12 * 1024 * 1024, 0xAB); // 12 MB，超过 10 MB 硬顶
    std::fs::write(&big, &bytes).unwrap();

    let run_id = "resume_grown_past_cap";
    let paths = RunPaths::new(ws.path(), run_id);
    paths.create_dirs().unwrap();
    save_conversation(
        &paths.conversation_path,
        &SavedConversation {
            run_id: run_id.to_string(),
            provider: "glm".to_string(),
            model: "glm-5.2".to_string(),
            messages: vec![
                ChatMessage::system("system prompt"),
                ChatMessage::user_with_images(
                    "look at this",
                    vec![ImageBlock {
                        media_type: "image/png".into(),
                        data_base64: String::new(),
                        source_path: Some(big),
                        bytes: bytes.len(),
                        sha256: None,
                    }],
                ),
                ChatMessage::assistant("I see a screenshot.", None, vec![]),
            ],
        },
    )
    .unwrap();

    resume_solo_with_judge_and_fs_scope(
        provider(&server.uri()),
        Box::new(myagent::judge::NoopJudge),
        ws.path(),
        ws.path().to_path_buf(),
        run_id.to_string(),
        Some("and now?".into()),
        OutputMode::Silent,
        PermissionPolicy::Allow,
        NetworkPolicy::On,
        FsReadScope::Workspace,
        Vec::new(),
        FsWriteFence::Off,
        1,
        ControlInputKind::Sentinel,
        false,
        false,
        SearchChoice::Ddg,
        Default::default(),
        0,
        0,
        None,
        Vec::new(),
        Vec::new(),
    )
    .await
    .unwrap();

    let msg = historical_image_bearing_message(&cap).await;
    assert!(
        msg["content"].is_string(),
        "超过 10MB 的重读图片不该原样整发：{msg}"
    );
    let body = msg.to_string();
    assert!(
        body.len() < 1_000_000,
        "12MB 的 base64 payload 不该出现在 wire 上：body 长度 {}",
        body.len()
    );
    let journal = std::fs::read_to_string(&paths.events_path).unwrap();
    assert!(journal.contains("attachment.dropped"));
}

#[tokio::test]
async fn resume_caps_reloaded_images_at_max_images_per_turn() {
    // P2-3：`--image` 首次加载有 `MAX_IMAGES_PER_TURN`(8) 硬顶；重读此前完全不受这条
    // 约束——20 张历史图片会被全部重读全部保留。
    let ws = tempfile::tempdir().unwrap();
    let cap = Arc::new(Mutex::new(Vec::new()));
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(Capture(cap.clone()))
        .mount(&server)
        .await;

    let mut images = Vec::new();
    for i in 0..20 {
        let path = ws.path().join(format!("shot{i}.png"));
        let bytes = png_bytes(format!("payload-{i}").as_bytes());
        std::fs::write(&path, &bytes).unwrap();
        images.push(ImageBlock {
            media_type: "image/png".into(),
            data_base64: String::new(),
            source_path: Some(path),
            bytes: bytes.len(),
            sha256: None,
        });
    }

    let run_id = "resume_too_many_images";
    let paths = RunPaths::new(ws.path(), run_id);
    paths.create_dirs().unwrap();
    save_conversation(
        &paths.conversation_path,
        &SavedConversation {
            run_id: run_id.to_string(),
            provider: "glm".to_string(),
            model: "glm-5.2".to_string(),
            messages: vec![
                ChatMessage::system("system prompt"),
                ChatMessage::user_with_images("look at these", images),
                ChatMessage::assistant("I see screenshots.", None, vec![]),
            ],
        },
    )
    .unwrap();

    resume_solo_with_judge_and_fs_scope(
        provider(&server.uri()),
        Box::new(myagent::judge::NoopJudge),
        ws.path(),
        ws.path().to_path_buf(),
        run_id.to_string(),
        Some("and now?".into()),
        OutputMode::Silent,
        PermissionPolicy::Allow,
        NetworkPolicy::On,
        FsReadScope::Workspace,
        Vec::new(),
        FsWriteFence::Off,
        1,
        ControlInputKind::Sentinel,
        false,
        false,
        SearchChoice::Ddg,
        Default::default(),
        0,
        0,
        None,
        Vec::new(),
        Vec::new(),
    )
    .await
    .unwrap();

    let last = cap.lock().unwrap().last().cloned().unwrap();
    let msg = last["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["role"] == "user" && m["content"].to_string().contains("look at these"))
        .cloned()
        .expect("历史带图 user 消息必须存在（哪怕图片被剥完，文本仍在）");
    let image_count = match &msg["content"] {
        Value::Array(parts) => parts.iter().filter(|p| p["type"] == "image_url").count(),
        _ => 0,
    };
    assert_eq!(
        image_count, 8,
        "20 张历史图片重读后必须只保留 8 张（MAX_IMAGES_PER_TURN）：{msg}"
    );

    let journal = std::fs::read_to_string(&paths.events_path).unwrap();
    let dropped_count = journal.matches("attachment.dropped").count();
    assert_eq!(
        dropped_count, 12,
        "超出 8 张上限的 12 张必须逐张记 attachment.dropped：{journal}"
    );
}

#[tokio::test]
async fn resume_truncates_overflow_before_rereading_never_touches_their_source_files() {
    // t12-img 第四轮返工（P3-G）：8 张上限必须在重读循环**之前**截断，超出的直接
    // `too_many_images`，一个字节都不读。用「前 8 张真实存在、后 12 张 source_path
    // 压根没有文件」来证明这件事——如果实现退回"重读完再裁"，后 12 张会先被尝试
    // `std::fs::metadata`/`read`、文件不存在只会得到 `source_missing`，reason 就不是
    // `too_many_images`；只有真正"进循环前先截断、后 12 张从没被摸过"才会让它们清一色
    // 是 `too_many_images`。
    let ws = tempfile::tempdir().unwrap();
    let cap = Arc::new(Mutex::new(Vec::new()));
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(Capture(cap.clone()))
        .mount(&server)
        .await;

    let mut images = Vec::new();
    for i in 0..8 {
        let path = ws.path().join(format!("real{i}.png"));
        let bytes = png_bytes(format!("payload-{i}").as_bytes());
        std::fs::write(&path, &bytes).unwrap();
        images.push(ImageBlock {
            media_type: "image/png".into(),
            data_base64: String::new(),
            source_path: Some(path),
            bytes: bytes.len(),
            sha256: None,
        });
    }
    for i in 0..12 {
        // 这些路径永远不落盘——如果重读循环真的碰了它们，只会拿到 source_missing。
        let path = ws.path().join(format!("never-created-{i}.png"));
        images.push(ImageBlock {
            media_type: "image/png".into(),
            data_base64: String::new(),
            source_path: Some(path),
            bytes: 1,
            sha256: None,
        });
    }

    let run_id = "resume_overflow_never_touched";
    let paths = RunPaths::new(ws.path(), run_id);
    paths.create_dirs().unwrap();
    save_conversation(
        &paths.conversation_path,
        &SavedConversation {
            run_id: run_id.to_string(),
            provider: "glm".to_string(),
            model: "glm-5.2".to_string(),
            messages: vec![
                ChatMessage::system("system prompt"),
                ChatMessage::user_with_images("look at these", images),
                ChatMessage::assistant("I see screenshots.", None, vec![]),
            ],
        },
    )
    .unwrap();

    resume_solo_with_judge_and_fs_scope(
        provider(&server.uri()),
        Box::new(myagent::judge::NoopJudge),
        ws.path(),
        ws.path().to_path_buf(),
        run_id.to_string(),
        Some("and now?".into()),
        OutputMode::Silent,
        PermissionPolicy::Allow,
        NetworkPolicy::On,
        FsReadScope::Workspace,
        Vec::new(),
        FsWriteFence::Off,
        1,
        ControlInputKind::Sentinel,
        false,
        false,
        SearchChoice::Ddg,
        Default::default(),
        0,
        0,
        None,
        Vec::new(),
        Vec::new(),
    )
    .await
    .unwrap();

    let last = cap.lock().unwrap().last().cloned().unwrap();
    let msg = last["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["role"] == "user" && m["content"].to_string().contains("look at these"))
        .cloned()
        .expect("历史带图 user 消息必须存在（哪怕图片被剥完，文本仍在）");
    let image_count = match &msg["content"] {
        Value::Array(parts) => parts.iter().filter(|p| p["type"] == "image_url").count(),
        _ => 0,
    };
    assert_eq!(image_count, 8, "前 8 张真实图片必须全部保留：{msg}");

    let journal = std::fs::read_to_string(&paths.events_path).unwrap();
    let too_many = journal.matches("\"reason\":\"too_many_images\"").count();
    let source_missing = journal.matches("\"reason\":\"source_missing\"").count();
    assert_eq!(
        too_many, 12,
        "从没被创建过的后 12 张必须清一色 too_many_images（证明截断在重读之前发生，\
         它们从未被 metadata()/read() 碰过）：{journal}"
    );
    assert_eq!(
        source_missing, 0,
        "若这里出现 source_missing，说明重读循环真的尝试打开了那些不存在的文件——\
         截断发生在了重读之后（P3-G 回归）：{journal}"
    );

    // t12-img 第四轮返工（P3-F）：too_many_images 的说明行必须说真话——「超出每轮 8 张
    // 上限」，不能复用「原文件缺失或内容已变化」（这里文件既没缺失也没变，只是撞了
    // 每轮上限；那句假话还会被 resume 收尾 `save_conversation` 固化进会话文本）。
    let content_str = msg["content"].to_string();
    assert!(
        content_str.contains("超出每轮 8 张上限"),
        "too_many_images 的说明行必须提到「超出每轮 8 张上限」：{content_str}"
    );
    assert!(
        !content_str.contains("原文件缺失或内容已变化"),
        "too_many_images 不该复用「原文件缺失或内容已变化」这句假话：{content_str}"
    );
}

// ---------- t12-img 第三轮 opus 审 P3-7：resume 的降级顺序 ----------

#[tokio::test]
async fn resume_degrades_before_reloading_when_provider_does_not_support_images() {
    // P3-7：`degrade_unsupported_images` 此前排在 `reload_stale_images` 之后——续聊换
    // 成不支持图片的 provider 时，历史图片会先被尝试重读（哪怕明知这条 provider 根本
    // 不收图片），重读失败时用户看到的 reason 是 `source_missing` 而不是更贴切的
    // `provider_no_image_support`。修后应先降级（不支持图片直接剥，不白读盘），
    // reason 必须是 `provider_no_image_support`。
    let ws = tempfile::tempdir().unwrap();
    let cap = Arc::new(Mutex::new(Vec::new()));
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(Capture(cap.clone()))
        .mount(&server)
        .await;

    // 源文件不存在——如果代码先重读，会报 source_missing；先降级则根本不会尝试读盘。
    let missing = ws.path().join("shot.png");

    let run_id = "resume_degrade_before_reload";
    let paths = RunPaths::new(ws.path(), run_id);
    paths.create_dirs().unwrap();
    save_conversation(
        &paths.conversation_path,
        &SavedConversation {
            run_id: run_id.to_string(),
            provider: "glm".to_string(),
            model: "glm-5.2".to_string(),
            messages: vec![
                ChatMessage::system("system prompt"),
                ChatMessage::user_with_images(
                    "look at this",
                    vec![ImageBlock {
                        media_type: "image/png".into(),
                        data_base64: String::new(),
                        source_path: Some(missing),
                        bytes: 3,
                        sha256: None,
                    }],
                ),
                ChatMessage::assistant("I see a screenshot.", None, vec![]),
            ],
        },
    )
    .unwrap();

    // 这条 provider 显式声明不支持图片（不是靠魔数/家族猜的边缘情形，直接覆盖）。
    let unsupported = OpenAiCompatibleProvider::new(OpenAiCompatibleConfig {
        provider_id: "glm".into(),
        api_key: "sk".into(),
        base_url: format!("{}/v1", server.uri()),
        model: "glm-5.2".into(),
        timeout_secs: 5,
        native_search_enabled: false,
        supports_images_override: Some(false),
        ..Default::default()
    })
    .unwrap();

    resume_solo_with_judge_and_fs_scope(
        unsupported,
        Box::new(myagent::judge::NoopJudge),
        ws.path(),
        ws.path().to_path_buf(),
        run_id.to_string(),
        Some("and now?".into()),
        OutputMode::Silent,
        PermissionPolicy::Allow,
        NetworkPolicy::On,
        FsReadScope::Workspace,
        Vec::new(),
        FsWriteFence::Off,
        1,
        ControlInputKind::Sentinel,
        false,
        false,
        SearchChoice::Ddg,
        Default::default(),
        0,
        0,
        None,
        Vec::new(),
        Vec::new(),
    )
    .await
    .unwrap();

    let journal = std::fs::read_to_string(&paths.events_path).unwrap();
    assert!(journal.contains("attachment.dropped"));
    assert!(
        journal.contains("provider_no_image_support"),
        "不支持图片的 provider 续聊，reason 必须是 provider_no_image_support，不是 source_missing：{journal}"
    );
    assert!(
        !journal.contains("\"reason\":\"source_missing\""),
        "不该先尝试重读再报 source_missing：{journal}"
    );
}

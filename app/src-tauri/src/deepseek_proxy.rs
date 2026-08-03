//! DeepSeek 借壳兼容代理（thinking 回传 400 垫片）。
//!
//! 兼容垫片 for DeepSeek `/anthropic` thinking-passback：claude CLI 2.1.156 在 agentic
//! 回放时剥掉 deepseek 返回的 thinking 块（签名是假的、claude 不认），而 deepseek thinking
//! mode 要求原样回传 → 400 `content[].thinking must be passed back`。本代理对出站请求做
//! 幂等改写：给含 tool_use 但无 thinking 块的 assistant message 补一个空 thinking 块。
//!
//! 删除条件（前向兼容 · 自失效垫片，上游修好后摘除，触发二选一）：claude 开始在 agentic
//! 回放里带回 thinking 块（注入计数长期为 0 佐证），或 deepseek 放宽 thinking-passback 契约。
//! 删法：删本模块 + lib.rs deepseek 分支 ANTHROPIC_BASE_URL 改回直连 deepseek。
//! kill switch：env `AGENTLOOM_DEEPSEEK_PROXY=0` → ensure_proxy 返 None → 回退直连（应急）。

use std::collections::HashMap;
use std::io::{Read, Write};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};

/// 累计注入条数（= 「上游是否已自修」信号；长期为 0 说明 claude 已自己回放 thinking → 可删垫片）。
static INJECTED_TOTAL: AtomicUsize = AtomicUsize::new(0);

/// 累计注入条数（供日志/诊断）。
pub fn injected_total() -> usize {
    INJECTED_TOTAL.load(Ordering::Relaxed)
}

/// 对请求 body 做幂等改写，返回本次注入的 thinking 块条数。
/// 每个 role==assistant 且 content 为数组的 message：先删除所有 redacted_thinking 块（deepseek
/// 不支持、透传会 400），再若含 tool_use 块且不含真 thinking 块、则在 content 最前插一个
/// `{"type":"thinking","thinking":"","signature":""}`。其它（user 消息、纯文本 assistant、
/// 已有真 thinking 的、content 为 string）一律不动。
pub fn inject_thinking_if_missing(body: &mut serde_json::Value) -> usize {
    let mut injected = 0usize;
    let Some(messages) = body.get_mut("messages").and_then(|m| m.as_array_mut()) else {
        return 0;
    };
    for msg in messages.iter_mut() {
        if msg.get("role").and_then(|r| r.as_str()) != Some("assistant") {
            continue;
        }
        let Some(content) = msg.get_mut("content").and_then(|c| c.as_array_mut()) else {
            continue;
        };
        // 1) strip redacted_thinking（前向兼容兜底；当前 claude 不回放、纯防御）
        content.retain(|b| b.get("type").and_then(|t| t.as_str()) != Some("redacted_thinking"));
        // 2) 补空 thinking 块
        let has_tool = content
            .iter()
            .any(|b| b.get("type").and_then(|t| t.as_str()) == Some("tool_use"));
        let has_thinking = content
            .iter()
            .any(|b| b.get("type").and_then(|t| t.as_str()) == Some("thinking"));
        if has_tool && !has_thinking {
            content.insert(
                0,
                serde_json::json!({"type": "thinking", "thinking": "", "signature": ""}),
            );
            injected += 1;
        }
    }
    INJECTED_TOTAL.fetch_add(injected, Ordering::Relaxed);
    injected
}

#[derive(Debug, PartialEq, Eq)]
enum ProxyDecision {
    Reuse(u16),
    StartNew,
}

#[derive(Default)]
struct ProxyRegistry {
    ports_by_upstream: HashMap<String, u16>,
}

impl ProxyRegistry {
    fn resolve<F>(&self, upstream: &str, mut is_alive: F) -> ProxyDecision
    where
        F: FnMut(u16) -> bool,
    {
        match self.ports_by_upstream.get(upstream).copied() {
            Some(port) if is_alive(port) => ProxyDecision::Reuse(port),
            _ => ProxyDecision::StartNew,
        }
    }

    fn register(&mut self, upstream: &str, port: u16) {
        self.ports_by_upstream.insert(upstream.to_string(), port);
    }
}

/// 代理端口 registry（按 upstream 分桶；线程死掉/端口失活时 ensure_proxy 会重起该 upstream）。
static PROXY_REGISTRY: OnceLock<Mutex<ProxyRegistry>> = OnceLock::new();

fn proxy_registry() -> &'static Mutex<ProxyRegistry> {
    PROXY_REGISTRY.get_or_init(|| Mutex::new(ProxyRegistry::default()))
}

/// hop-by-hop header（两个方向都不透传）。
fn is_hop_by_hop(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    matches!(
        n.as_str(),
        "host"
            | "content-length"
            | "accept-encoding"
            | "connection"
            | "transfer-encoding"
            | "keep-alive"
            | "te"
            | "trailer"
            | "upgrade"
    ) || n.starts_with("proxy-")
}

/// 确保代理在跑，返回端口。env AGENTLOOM_DEEPSEEK_PROXY=0 → None（回退直连）。
/// 同 upstream 已有端口且存活 → 复用；否则（首次/线程已死）重起；bind 失败 → None。
pub fn ensure_proxy(upstream: &str) -> Option<u16> {
    if std::env::var("AGENTLOOM_DEEPSEEK_PROXY").as_deref() == Ok("0") {
        return None;
    }
    let mut guard = proxy_registry().lock().ok()?;
    match guard.resolve(upstream, |port| {
        std::net::TcpStream::connect(("127.0.0.1", port)).is_ok()
    }) {
        ProxyDecision::Reuse(port) => return Some(port),
        ProxyDecision::StartNew => {}
    }
    match start_server(upstream.to_string()) {
        Ok(port) => {
            guard.register(upstream, port);
            Some(port)
        }
        Err(e) => {
            eprintln!("[deepseek-proxy] 启动失败，回退直连：{e}");
            None
        }
    }
}

/// 绑 127.0.0.1:0、起 accept loop（thread-per-request）、返回端口。
fn start_server(upstream_base: String) -> Result<u16, String> {
    let server = tiny_http::Server::http("127.0.0.1:0").map_err(|e| e.to_string())?;
    let port = server
        .server_addr()
        .to_ip()
        .ok_or_else(|| crate::ui_msg::al_err("proxy.noPort", &[]))?
        .port();
    std::thread::spawn(move || {
        // blocking client：默认无总超时（SSE 长连）；显式禁所有自动解压（no_* 非 feature-gated）
        // → 即使将来 reqwest 压缩 feature 被开也不会解压破坏 SSE 透传。
        let client = match reqwest::blocking::Client::builder()
            .no_gzip()
            .no_brotli()
            .no_zstd()
            .no_deflate()
            .build()
        {
            Ok(c) => c,
            Err(e) => {
                eprintln!("[deepseek-proxy] client 构建失败：{e}");
                return;
            }
        };
        for req in server.incoming_requests() {
            let client = client.clone();
            let base = upstream_base.clone();
            // 每请求一线程：一条 SSE 占住整个 handler，不能串行（项目支持 per-session 并行）。
            std::thread::spawn(move || handle_request(req, &base, &client));
        }
    });
    Ok(port)
}

/// 处理单个请求：读 body → 改写 → 转发 → 逐块 flush 流式回传。失败 fail-open，不拖垮 app。
fn handle_request(
    req: tiny_http::Request,
    upstream_base: &str,
    client: &reqwest::blocking::Client,
) {
    let mut req = req;
    let mut raw = Vec::new();
    if req.as_reader().read_to_end(&mut raw).is_err() {
        let _ =
            req.respond(tiny_http::Response::from_string("proxy read error").with_status_code(502));
        return;
    }
    // 改写（非 JSON / 失败 → 透传原 body）
    let sent: Vec<u8> = match serde_json::from_slice::<serde_json::Value>(&raw) {
        Ok(mut v) => {
            let n = inject_thinking_if_missing(&mut v);
            if n > 0 {
                eprintln!(
                    "[deepseek-proxy] 补 thinking 块 ×{n}（累计 {}）",
                    injected_total()
                );
            }
            serde_json::to_vec(&v).unwrap_or_else(|_| raw.clone())
        }
        Err(_) => raw.clone(),
    };
    // 转发：upstream_base + 原 path（含 query，如 /v1/messages?beta=true）
    // accept-encoding=identity：要求上游不压缩响应（解压本身已在 builder no_* 关掉，此处双保险）。
    let url = format!("{}{}", upstream_base, req.url());
    let mut rb = client
        .post(&url)
        .body(sent)
        .header("accept-encoding", "identity");
    for h in req.headers() {
        let name = h.field.as_str().as_str();
        if is_hop_by_hop(name) {
            continue;
        }
        rb = rb.header(name, h.value.as_str());
    }
    match rb.send() {
        Ok(mut resp) => {
            let status = resp.status().as_u16();
            // 响应 header：去 hop-by-hop + content-length/transfer-encoding/content-encoding
            let mut header_lines = String::new();
            for (k, v) in resp.headers().iter() {
                let kn = k.as_str();
                if is_hop_by_hop(kn) || kn.eq_ignore_ascii_case("content-encoding") {
                    continue;
                }
                if let Ok(vs) = v.to_str() {
                    header_lines.push_str(&format!("{kn}: {vs}\r\n"));
                }
            }
            // 用 into_writer 拿底层连接，绕开 tiny_http chunked 的 8KiB 内部缓冲（破坏 SSE）。
            // chunked 编码 + 0\r\n\r\n 终止符（自带 body 结束标记）——不能用 close-delimited：
            // into_writer 的 Box<dyn Write> drop 不保证关 TCP socket（tiny_http keep-alive），
            // 客户端读完整 body 会等不到 EOF 卡死（实测 30s 超时）。逐 read 写一个 chunk + flush = 真流式。
            let mut w = req.into_writer();
            let head =
                format!("HTTP/1.1 {status} OK\r\n{header_lines}Transfer-Encoding: chunked\r\n\r\n");
            if w.write_all(head.as_bytes()).is_err() || w.flush().is_err() {
                return;
            }
            let mut buf = [0u8; 8192];
            loop {
                match resp.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        let chunk_header = format!("{n:x}\r\n");
                        if w.write_all(chunk_header.as_bytes()).is_err()
                            || w.write_all(&buf[..n]).is_err()
                            || w.write_all(b"\r\n").is_err()
                            || w.flush().is_err()
                        {
                            return;
                        }
                    }
                    Err(_) => return,
                }
            }
            let _ = w.write_all(b"0\r\n\r\n");
            let _ = w.flush();
        }
        Err(e) => {
            let _ = req.respond(
                tiny_http::Response::from_string(format!("upstream error: {e}"))
                    .with_status_code(502),
            );
        }
    }
}

#[cfg(test)]
mod registry {
    use super::*;

    #[test]
    fn registry_same_upstream_reuses() {
        let upstream = "https://one.example/anthropic";
        let mut registry = ProxyRegistry::default();
        registry.register(upstream, 41001);

        assert_eq!(
            registry.resolve(upstream, |_| true),
            ProxyDecision::Reuse(41001)
        );
    }

    #[test]
    fn registry_distinct_upstream_distinct() {
        let first = "https://one.example/anthropic";
        let second = "https://two.example/anthropic";
        let mut registry = ProxyRegistry::default();
        registry.register(first, 41001);
        registry.register(second, 41002);

        assert_eq!(
            registry.resolve(first, |_| true),
            ProxyDecision::Reuse(41001)
        );
        assert_eq!(
            registry.resolve(second, |_| true),
            ProxyDecision::Reuse(41002)
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn body(messages: serde_json::Value) -> serde_json::Value {
        json!({ "model": "deepseek-chat", "messages": messages })
    }

    #[test]
    fn injects_when_tool_use_without_thinking() {
        let mut b = body(json!([
            {"role": "user", "content": "hi"},
            {"role": "assistant", "content": [{"type": "tool_use", "id": "t1", "name": "Bash", "input": {}}]},
            {"role": "user", "content": [{"type": "tool_result", "tool_use_id": "t1", "content": "ok"}]}
        ]));
        assert_eq!(inject_thinking_if_missing(&mut b), 1);
        let blocks = b["messages"][1]["content"].as_array().unwrap();
        assert_eq!(blocks[0]["type"], "thinking");
        assert_eq!(blocks[0]["thinking"], "");
        assert_eq!(blocks[1]["type"], "tool_use");
    }

    #[test]
    fn noop_when_thinking_already_present() {
        let mut b = body(json!([
            {"role": "assistant", "content": [
                {"type": "thinking", "thinking": "x", "signature": "s"},
                {"type": "tool_use", "id": "t1", "name": "Bash", "input": {}}
            ]}
        ]));
        assert_eq!(inject_thinking_if_missing(&mut b), 0);
        assert_eq!(b["messages"][0]["content"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn noop_for_plain_text_assistant() {
        let mut b = body(json!([
            {"role": "assistant", "content": [{"type": "text", "text": "hello"}]}
        ]));
        assert_eq!(inject_thinking_if_missing(&mut b), 0);
    }

    #[test]
    fn ignores_user_messages() {
        let mut b = body(json!([
            {"role": "user", "content": [{"type": "tool_use", "id": "t1", "name": "x", "input": {}}]}
        ]));
        assert_eq!(inject_thinking_if_missing(&mut b), 0);
    }

    #[test]
    fn noop_for_string_content() {
        let mut b = body(json!([
            {"role": "assistant", "content": "plain string"}
        ]));
        assert_eq!(inject_thinking_if_missing(&mut b), 0);
    }

    #[test]
    fn counts_each_assistant_independently() {
        let mut b = body(json!([
            {"role": "assistant", "content": [{"type": "tool_use", "id": "a", "name": "x", "input": {}}]},
            {"role": "user", "content": [{"type": "tool_result", "tool_use_id": "a", "content": "ok"}]},
            {"role": "assistant", "content": [{"type": "tool_use", "id": "b", "name": "x", "input": {}}]}
        ]));
        assert_eq!(inject_thinking_if_missing(&mut b), 2);
    }

    #[test]
    fn strips_redacted_thinking_and_injects() {
        let mut b = body(json!([
            {"role": "assistant", "content": [
                {"type": "redacted_thinking", "data": "blob"},
                {"type": "tool_use", "id": "t1", "name": "Bash", "input": {}}
            ]}
        ]));
        assert_eq!(inject_thinking_if_missing(&mut b), 1);
        let blocks = b["messages"][0]["content"].as_array().unwrap();
        assert!(blocks.iter().all(|x| x["type"] != "redacted_thinking"));
        assert_eq!(blocks[0]["type"], "thinking");
        assert_eq!(blocks[1]["type"], "tool_use");
    }

    #[test]
    fn injects_once_for_multiple_tool_use() {
        let mut b = body(json!([
            {"role": "assistant", "content": [
                {"type": "tool_use", "id": "a", "name": "x", "input": {}},
                {"type": "tool_use", "id": "b", "name": "y", "input": {}}
            ]}
        ]));
        assert_eq!(inject_thinking_if_missing(&mut b), 1);
        assert_eq!(b["messages"][0]["content"][0]["type"], "thinking");
    }

    use std::net::TcpListener;
    use std::time::{Duration, Instant};

    /// 裸 TCP mock upstream（thread-per-connection，完全控制逐块 flush + sleep）。
    /// handler(请求 body) → (status, 响应 chunks[(bytes, 发出前 sleep)])。
    /// 响应 Connection:close 无 content-length，逐块 flush，写完关连接。返回 mock 端口。
    fn start_mock_upstream<F>(handler: F) -> u16
    where
        F: Fn(Vec<u8>) -> (u16, Vec<(Vec<u8>, Duration)>) + Send + Sync + 'static,
    {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let handler = std::sync::Arc::new(handler);
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let handler = handler.clone();
                std::thread::spawn(move || {
                    use std::io::BufRead;
                    let mut reader = std::io::BufReader::new(stream.try_clone().unwrap());
                    let mut content_len = 0usize;
                    loop {
                        let mut line = String::new();
                        if reader.read_line(&mut line).unwrap_or(0) == 0 {
                            return;
                        }
                        if line == "\r\n" {
                            break;
                        }
                        if let Some(v) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                            content_len = v.trim().parse().unwrap_or(0);
                        }
                    }
                    let mut req_body = vec![0u8; content_len];
                    reader.read_exact(&mut req_body).ok();
                    let (status, chunks) = handler(req_body);
                    let _ = write!(
                        stream,
                        "HTTP/1.1 {status} OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n"
                    );
                    let _ = stream.flush();
                    for (data, delay) in chunks {
                        if !delay.is_zero() {
                            std::thread::sleep(delay);
                        }
                        let _ = stream.write_all(&data);
                        let _ = stream.flush();
                    }
                });
            }
        });
        port
    }

    fn proxy_to(upstream_port: u16) -> u16 {
        start_server(format!("http://127.0.0.1:{upstream_port}")).unwrap()
    }

    #[test]
    fn proxy_injects_thinking_into_forwarded_body() {
        let seen = std::sync::Arc::new(Mutex::new(Vec::<u8>::new()));
        let seen2 = seen.clone();
        let up = start_mock_upstream(move |body| {
            *seen2.lock().unwrap() = body;
            (200, vec![(b"data: ok\n\n".to_vec(), Duration::ZERO)])
        });
        let proxy = proxy_to(up);

        let req_body = serde_json::to_vec(&json!({
            "model": "deepseek-chat",
            "messages": [
                {"role": "user", "content": "hi"},
                {"role": "assistant", "content": [{"type": "tool_use", "id": "t1", "name": "Bash", "input": {}}]},
                {"role": "user", "content": [{"type": "tool_result", "tool_use_id": "t1", "content": "ok"}]}
            ]
        }))
        .unwrap();

        let resp = reqwest::blocking::Client::new()
            .post(format!("http://127.0.0.1:{proxy}/v1/messages?beta=true"))
            .body(req_body)
            .send()
            .unwrap();
        assert_eq!(resp.status().as_u16(), 200);

        std::thread::sleep(Duration::from_millis(50));
        let forwarded: serde_json::Value = serde_json::from_slice(&seen.lock().unwrap()).unwrap();
        let blocks = forwarded["messages"][1]["content"].as_array().unwrap();
        assert_eq!(
            blocks[0]["type"], "thinking",
            "代理应给 assistant tool_use 补 thinking 块"
        );
    }

    #[test]
    fn proxy_streams_first_chunk_before_upstream_finishes() {
        // mock：首块 0 延迟 → 第二块前 sleep 800ms。整段缓冲会等到 ~800ms 才给客户端首字节。
        let up = start_mock_upstream(|_b| {
            (
                200,
                vec![
                    (b"data: first\n\n".to_vec(), Duration::ZERO),
                    (b"data: second\n\n".to_vec(), Duration::from_millis(800)),
                ],
            )
        });
        let proxy = proxy_to(up);

        let t = Instant::now(); // 从发请求前计时（否则整段缓冲也假绿）
        let mut resp = reqwest::blocking::Client::new()
            .post(format!("http://127.0.0.1:{proxy}/v1/messages"))
            .body(serde_json::to_vec(&json!({"messages": []})).unwrap())
            .send()
            .unwrap();
        let mut buf = [0u8; 64];
        let n = resp.read(&mut buf).unwrap();
        let elapsed = t.elapsed();
        assert!(n > 0, "应读到首块");
        assert!(
            elapsed < Duration::from_millis(500),
            "首块应在上游第二块的 800ms sleep 前到达（实际 {elapsed:?}）—— 整段缓冲会 ~800ms"
        );
    }

    #[test]
    fn proxy_handles_requests_concurrently() {
        // mock 每请求 sleep 300ms（mock 本身 thread-per-connection 并发）。
        let up = start_mock_upstream(|_b| {
            (
                200,
                vec![(b"data: ok\n\n".to_vec(), Duration::from_millis(300))],
            )
        });
        let proxy = proxy_to(up);

        let t = Instant::now();
        let handles: Vec<_> = (0..4)
            .map(|_| {
                std::thread::spawn(move || {
                    let _ = reqwest::blocking::Client::new()
                        .post(format!("http://127.0.0.1:{proxy}/v1/messages"))
                        .body(serde_json::to_vec(&json!({"messages": []})).unwrap())
                        .send()
                        .unwrap()
                        .bytes();
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        let elapsed = t.elapsed();
        assert!(
            elapsed < Duration::from_millis(800),
            "4 并发请求应并行完成（实际 {elapsed:?}），证 thread-per-request 不串行（串行约 1200ms）"
        );
    }
}

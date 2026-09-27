#![allow(dead_code)]

use std::collections::HashMap;
use std::fmt;

use base64::{engine::general_purpose::STANDARD, Engine as _};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

use crate::remote_crypto::{
    derive_connect_token, derive_k_pair, generate_key_32, generate_x25519_keypair, open, seal,
    wrap_key, CryptoError, EnvelopeMeta,
};

pub(crate) const PAIRING_LIFETIME_SECS: u64 = 300;
const ACCESS_LIFETIME_MS: u64 = 3_600_000;
pub(crate) const REFRESH_LIFETIME_MS: u64 = 2_592_000_000;
/// S1i1 §9.6：轮换后 prev 别名窗口——48h，与 fixtures/wire-v1.json 的 `ttl_kat cap="prev"`
/// （172_800_000ms）同源，跟 §9.4 registry 快照 prev 别名窗口一致，不另立新常量。journal 的
/// `response_expires`（原样重放回执的有效期）§9.6 未单独钉死数值，本单按注释里写明的依据取同值：
/// 一旦这个窗口过了，relay 侧 registry 快照也不会再带 prev 别名，继续用 journal 重放旧回执已经
/// 没有协议意义，两个窗口没有理由不同步。
pub(crate) const PREV_ALIAS_LIFETIME_MS: u64 = 172_800_000;

pub(crate) struct PairingSession {
    pub room_id: String,
    pub desktop_secret: [u8; 32],
    pub desktop_public: [u8; 32],
    pub pairing_token: String,
    pub expires_at_secs: u64,
    used: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct QrPayload {
    pub v: u32,
    pub relay_url: String,
    pub room: String,
    pub pairing_token: String,
    pub desktop_pub: String,
}

impl PairingSession {
    pub(crate) fn begin(relay_url: &str, room_id: &str, now_secs: u64) -> (Self, QrPayload) {
        let (desktop_secret, desktop_public) = generate_x25519_keypair();
        // A full 32-byte CSPRNG value is encoded as lowercase hex (256 bits).
        let pairing_token = encode_hex(generate_key_32().as_ref());
        let expires_at_secs = now_secs.saturating_add(PAIRING_LIFETIME_SECS);

        let session = Self {
            room_id: room_id.to_owned(),
            desktop_secret,
            desktop_public,
            pairing_token: pairing_token.clone(),
            expires_at_secs,
            used: false,
        };
        let qr_payload = QrPayload {
            v: 1,
            relay_url: relay_url.to_owned(),
            room: room_id.to_owned(),
            pairing_token,
            desktop_pub: STANDARD.encode(desktop_public),
        };
        (session, qr_payload)
    }
}

pub(crate) struct HelloFrame {
    pub remote_pub: [u8; 32],
    pub token_ct_b64: String,
    pub token_n_b64: String,
}

pub(crate) struct AcceptOutcome {
    pub k_room_wrapped_ct: String,
    pub k_room_wrapped_n: String,
    pub capability_token: String,
    pub refresh_token: String,
    pub device_record: DeviceRecord,
}

pub(crate) struct DeviceRecord {
    pub device_id: String,
    pub k_pair: Zeroizing<[u8; 32]>,
    pub token_hash: String,
    pub refresh_hash: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PairingError {
    NonContributory,
    TokenMismatch,
    Expired,
    AlreadyUsed,
    DecryptFailed,
    BadPayload,
    NotFound,
    Revoked,
}

impl fmt::Display for PairingError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NonContributory => formatter.write_str("non-contributory Diffie-Hellman result"),
            Self::TokenMismatch => formatter.write_str("token mismatch"),
            Self::Expired => formatter.write_str("token expired"),
            Self::AlreadyUsed => formatter.write_str("pairing token already used"),
            Self::DecryptFailed => formatter.write_str("pairing payload decryption failed"),
            Self::BadPayload => formatter.write_str("invalid pairing payload"),
            Self::NotFound => formatter.write_str("device not found"),
            Self::Revoked => formatter.write_str("device revoked"),
        }
    }
}

impl std::error::Error for PairingError {}

pub(crate) fn handle_hello(
    session: &mut PairingSession,
    hello: &HelloFrame,
    k_room: &[u8; 32],
    now_secs: u64,
) -> Result<AcceptOutcome, PairingError> {
    let k_pair = authenticate_hello(session, hello, now_secs)?;
    Ok(finalize_hello(k_pair, k_room))
}

fn authenticate_hello(
    session: &mut PairingSession,
    hello: &HelloFrame,
    now_secs: u64,
) -> Result<Zeroizing<[u8; 32]>, PairingError> {
    let k_pair = derive_k_pair(
        &session.desktop_secret,
        &hello.remote_pub,
        &session.pairing_token,
    )
    .map_err(map_derive_error)?;

    let decrypted_token = open(
        &k_pair,
        &pairing_envelope_meta(&session.room_id),
        &hello.token_ct_b64,
        &hello.token_n_b64,
    )
    .map_err(|_| PairingError::DecryptFailed)?;

    if now_secs >= session.expires_at_secs {
        return Err(PairingError::Expired);
    }
    if session.used {
        return Err(PairingError::AlreadyUsed);
    }
    if !constant_time_eq(&decrypted_token, session.pairing_token.as_bytes()) {
        return Err(PairingError::TokenMismatch);
    }

    session.used = true;
    Ok(k_pair)
}

fn finalize_hello(k_pair: Zeroizing<[u8; 32]>, k_room: &[u8; 32]) -> AcceptOutcome {
    let (k_room_wrapped_ct, k_room_wrapped_n) = wrap_key(&k_pair, k_room);

    // Capability and refresh tokens are independent 32-byte CSPRNG values encoded as hex.
    let capability_token = generate_token_hex();
    let refresh_token = generate_token_hex();
    let device_record = DeviceRecord {
        device_id: generate_device_id(),
        k_pair,
        token_hash: sha256_hex(&capability_token),
        refresh_hash: sha256_hex(&refresh_token),
    };

    AcceptOutcome {
        k_room_wrapped_ct,
        k_room_wrapped_n,
        capability_token,
        refresh_token,
        device_record,
    }
}

/// PairingSlot uses whole unix seconds, while the registry wire uses absolute unix milliseconds.
/// Keep the only lifetime conversion here so a seconds timestamp can never be copied into a frame.
pub(crate) fn pairing_access_expires_at_ms(now_ms: u64) -> Result<i64, String> {
    let lifetime_ms = PAIRING_LIFETIME_SECS.saturating_mul(1_000);
    i64::try_from(now_ms.saturating_add(lifetime_ms))
        .map_err(|_| "pairing expiry exceeds SQLite INTEGER range".to_owned())
}

pub(crate) fn device_access_expires_at_ms(now_ms: u64) -> Result<i64, String> {
    i64::try_from(now_ms.saturating_add(ACCESS_LIFETIME_MS))
        .map_err(|_| "access expiry exceeds SQLite INTEGER range".to_owned())
}

pub(crate) fn device_refresh_until_ms(now_ms: u64) -> Result<i64, String> {
    i64::try_from(now_ms.saturating_add(REFRESH_LIFETIME_MS))
        .map_err(|_| "refresh expiry exceeds SQLite INTEGER range".to_owned())
}

/// S1i1 §9.6：轮换那一刻起算的 prev 别名窗口终点，供 journal 的 `prev_expires_at`/
/// `response_expires` 复用。
pub(crate) fn refresh_prev_alias_expires_at_ms(now_ms: u64) -> Result<i64, String> {
    i64::try_from(now_ms.saturating_add(PREV_ALIAS_LIFETIME_MS))
        .map_err(|_| "prev alias expiry exceeds SQLite INTEGER range".to_owned())
}

pub(crate) struct DeviceTokens {
    pub device_id: String,
    pub access_token_hash: String,
    pub access_expires_at_ms: u64,
    pub refresh_token_hash: String,
    pub revoked: bool,
}

pub(crate) struct TokenBook {
    devices: HashMap<String, DeviceTokens>,
}

struct RefreshCandidate {
    access_token: String,
    refresh_token: String,
    access_token_hash: String,
    refresh_token_hash: String,
    access_expires_at_ms: u64,
}

/// S1i1 §9.6：`TokenBook::matches_current_refresh` 的只读探测结果——调用方（gateway 侧 refresh
/// 编排）拿它决定走轮换分支还是转去查 journal 的 prev 别名，*不*生成候选令牌、不修改任何状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RefreshTokenMatch {
    /// 命中当前 refresh hash——可以安全调用 `store::refresh_device_tokens` 轮换。
    Current,
    /// 设备存在且未吊销，但哈希不匹配当前——调用方应转去核对 journal 的 prev_refresh_hash。
    Mismatch,
    /// 设备未知或已吊销——不管 journal 是否命中都该整体拒绝（吊销必须 fail-closed，
    /// prev 别名不能成为吊销后的后门）。
    Unavailable,
}

/// S1i1 §2a/§2b：`store::refresh_device_tokens` 成功轮换后交给调用方（gateway 侧编排）的产物——
/// 只带调用方真正需要的东西（新哈希用于 registry 快照的 current，旧 generation/hash/prev 过期时刻
/// 用于 registry 快照的 prev 别名，回执密文体用于立即回帧或挂进 outbox 等 ack），故意不带明文
/// 令牌本身（那两个值已经被封进 `response_ct`/`response_n`，TokenBook 也已经在函数内部原子提交
/// 过了，调用方不需要、也不应该再摸一次明文）。
pub(crate) struct RotatedTokens {
    pub generation: i64,
    pub access_token_hash: String,
    pub access_expires_at_ms: i64,
    pub refresh_until_ms: i64,
    pub prev_generation: i64,
    pub prev_access_hash: String,
    pub prev_expires_at_ms: i64,
    pub response_ct: String,
    pub response_n: String,
}

impl TokenBook {
    pub(crate) fn new() -> Self {
        Self {
            devices: HashMap::new(),
        }
    }

    pub(crate) fn insert(
        &mut self,
        device_id: String,
        access_token: &str,
        refresh_token: &str,
        now_ms: u64,
    ) {
        self.devices.insert(
            device_id.clone(),
            DeviceTokens {
                device_id,
                access_token_hash: sha256_hex(access_token),
                access_expires_at_ms: now_ms.saturating_add(ACCESS_LIFETIME_MS),
                refresh_token_hash: sha256_hex(refresh_token),
                revoked: false,
            },
        );
    }

    pub(crate) fn verify_access(
        &self,
        device_id: &str,
        access_token: &str,
        now_ms: u64,
    ) -> Result<(), PairingError> {
        let device = self.devices.get(device_id).ok_or(PairingError::NotFound)?;
        if device.revoked {
            return Err(PairingError::Revoked);
        }
        if now_ms >= device.access_expires_at_ms {
            return Err(PairingError::Expired);
        }

        let candidate_hash = sha256_hex(access_token);
        if !constant_time_eq(
            candidate_hash.as_bytes(),
            device.access_token_hash.as_bytes(),
        ) {
            return Err(PairingError::TokenMismatch);
        }
        Ok(())
    }

    /// S1i1 §9.6：只读探测——不生成候选令牌、不修改任何状态。`prepare_refresh` 每次都会
    /// 掏两把 CSPRNG 生成一对抛弃候选，refresh 编排需要先知道"这次会不会命中当前"才能决定
    /// 要不要查配额，这个方法就是那次判定，避免在没决定轮换前就白算候选令牌。
    pub(crate) fn matches_current_refresh(
        &self,
        device_id: &str,
        refresh_token: &str,
    ) -> RefreshTokenMatch {
        let Some(device) = self.devices.get(device_id) else {
            return RefreshTokenMatch::Unavailable;
        };
        if device.revoked {
            return RefreshTokenMatch::Unavailable;
        }
        let candidate_hash = sha256_hex(refresh_token);
        if constant_time_eq(
            candidate_hash.as_bytes(),
            device.refresh_token_hash.as_bytes(),
        ) {
            RefreshTokenMatch::Current
        } else {
            RefreshTokenMatch::Mismatch
        }
    }

    fn prepare_refresh(
        &self,
        device_id: &str,
        refresh_token: &str,
        now_ms: u64,
    ) -> Result<RefreshCandidate, PairingError> {
        let device = self.devices.get(device_id).ok_or(PairingError::NotFound)?;
        if device.revoked {
            return Err(PairingError::Revoked);
        }

        let candidate_hash = sha256_hex(refresh_token);
        if !constant_time_eq(
            candidate_hash.as_bytes(),
            device.refresh_token_hash.as_bytes(),
        ) {
            return Err(PairingError::TokenMismatch);
        }

        let new_access_token = generate_token_hex();
        let new_refresh_token = generate_token_hex();
        Ok(RefreshCandidate {
            access_token_hash: sha256_hex(&new_access_token),
            refresh_token_hash: sha256_hex(&new_refresh_token),
            access_expires_at_ms: now_ms.saturating_add(ACCESS_LIFETIME_MS),
            access_token: new_access_token,
            refresh_token: new_refresh_token,
        })
    }

    fn commit_refresh(&mut self, device_id: &str, candidate: &RefreshCandidate) {
        let device = self
            .devices
            .get_mut(device_id)
            .expect("refresh candidate was prepared for an existing device");
        device.access_token_hash = candidate.access_token_hash.clone();
        device.access_expires_at_ms = candidate.access_expires_at_ms;
        device.refresh_token_hash = candidate.refresh_token_hash.clone();
    }

    pub(crate) fn revoke(&mut self, device_id: &str) {
        if let Some(device) = self.devices.get_mut(device_id) {
            device.revoked = true;
        }
    }
}

impl Default for TokenBook {
    fn default() -> Self {
        Self::new()
    }
}

fn map_derive_error(error: CryptoError) -> PairingError {
    match error {
        CryptoError::NonContributory => PairingError::NonContributory,
        CryptoError::BadBase64 | CryptoError::BadLength | CryptoError::DecryptFailed => {
            PairingError::BadPayload
        }
    }
}

fn pairing_envelope_meta(room_id: &str) -> EnvelopeMeta {
    // AAD ruling: this exact metadata must be checked against the relay-side Web client
    // implementation when that client is built（此裁定待 relay 端 Web 客户端实现时对表）.
    EnvelopeMeta {
        v: 1,
        room: room_id.to_owned(),
        epoch: 0,
        kind: "control".to_owned(),
        session: None,
        command_id: None,
    }
}

/// M0 v1.6（T5d-a.1 F2）：pair.done 确认体的 AAD 绑定。kind 用 "pair-confirm" 与 hello 用的
/// "control" 区分；session 位复用来装 device_id，把它一起绑进 AAD——同一房间不同设备、同设备
/// 不同配对 session 产生的确认体互不可用。此裁定待 relay/Web 端对表（同 pairing_envelope_meta
/// 的 provisional 注记）。
pub(crate) fn pair_done_confirm_meta(room_id: &str, device_id: &str) -> EnvelopeMeta {
    EnvelopeMeta {
        v: 1,
        room: room_id.to_owned(),
        epoch: 0,
        kind: "pair-confirm".to_owned(),
        session: Some(device_id.to_owned()),
        command_id: None,
    }
}

pub(crate) fn pair_ready_meta(room_id: &str, device_id: &str) -> EnvelopeMeta {
    EnvelopeMeta {
        v: 1,
        room: room_id.to_owned(),
        epoch: 0,
        kind: "pair-ready".to_owned(),
        session: Some(device_id.to_owned()),
        command_id: None,
    }
}

pub(crate) fn pair_accept_tokens_meta(room_id: &str, device_id: &str) -> EnvelopeMeta {
    EnvelopeMeta {
        v: 1,
        room: room_id.to_owned(),
        epoch: 0,
        kind: "pair-accept-tokens".to_owned(),
        session: Some(device_id.to_owned()),
        command_id: None,
    }
}

#[derive(Serialize)]
struct PairAcceptTokens<'a> {
    capability_token: &'a str,
    refresh_token: &'a str,
}

pub(crate) fn seal_pair_accept_tokens(
    k_pair: &[u8; 32],
    room_id: &str,
    device_id: &str,
    capability_token: &str,
    refresh_token: &str,
) -> (String, String) {
    let plaintext = Zeroizing::new(
        serde_json::to_vec(&PairAcceptTokens {
            capability_token,
            refresh_token,
        })
        .expect("serializing string-only pair.accept tokens cannot fail"),
    );
    seal(
        k_pair,
        &pair_accept_tokens_meta(room_id, device_id),
        plaintext.as_slice(),
    )
}

pub(crate) fn seal_pair_ready(
    k_pair: &[u8; 32],
    room_id: &str,
    device_id: &str,
) -> (String, String) {
    seal(
        k_pair,
        &pair_ready_meta(room_id, device_id),
        device_id.as_bytes(),
    )
}

/// 远端侧参考实现（也是测试要用的「扮演真远端」工具）：解出 K_room 后据此密封 pair.done 的
/// 确认体。plaintext 就是 device_id 自身——AAD 已经把 room+device_id 绑死，plaintext 校验是防
/// 御性冗余，不是唯一绑定来源。
pub(crate) fn seal_pair_done_confirm(
    k_room: &[u8; 32],
    room_id: &str,
    device_id: &str,
) -> (String, String) {
    seal(
        k_room,
        &pair_done_confirm_meta(room_id, device_id),
        device_id.as_bytes(),
    )
}

/// 桌面侧：用 SentAccept 暂存的 K_room 验证 pair.done 确认体。open 失败（缺字段前调用方就该
/// 短路、伪造密文、用错 key seal）一律 false——恶意 relay 没有 K_room（accept 里的 K_room 是
/// 用远端专属 K_pair 包裹的，relay 解不开），产不出能通过这道验证的密文。
pub(crate) fn verify_pair_done_confirm(
    k_room: &[u8; 32],
    room_id: &str,
    device_id: &str,
    confirm_ct: &str,
    confirm_n: &str,
) -> bool {
    match open(
        k_room,
        &pair_done_confirm_meta(room_id, device_id),
        confirm_ct,
        confirm_n,
    ) {
        Ok(plaintext) => plaintext == device_id.as_bytes(),
        Err(_) => false,
    }
}

/// S1i1 §9.5 末句（第 236 行）AAD 口径：refresh 请求/回执 meta 统一走 §1 既有 EnvelopeMeta
/// UTF-8 拼串，`session` 装 device_id、`command_id` 装 request_id。两个 kind 的字面量与
/// fixtures/wire-v1.json 的 `aad_kat_token_refresh`/`aad_kat_token_refresh_ok` 逐字符对齐。
pub(crate) fn token_refresh_meta(room_id: &str, device_id: &str, request_id: &str) -> EnvelopeMeta {
    EnvelopeMeta {
        v: 1,
        room: room_id.to_owned(),
        epoch: 0,
        kind: "token.refresh".to_owned(),
        session: Some(device_id.to_owned()),
        command_id: Some(request_id.to_owned()),
    }
}

pub(crate) fn token_refresh_ok_meta(
    room_id: &str,
    device_id: &str,
    request_id: &str,
) -> EnvelopeMeta {
    EnvelopeMeta {
        v: 1,
        room: room_id.to_owned(),
        epoch: 0,
        kind: "token.refresh.ok".to_owned(),
        session: Some(device_id.to_owned()),
        command_id: Some(request_id.to_owned()),
    }
}

#[derive(Deserialize)]
struct RefreshRequestPlaintext {
    refresh_token: String,
}

fn is_hex64(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// S1i1 §2e：解密 `token.refresh`/`token.refresh.forward` 请求密文体（该设备的 K_pair，
/// §1 第 38 行/§9.5 第 234 行：令牌面帧族只含哈希/元数据/K_pair 密文体），明文形状
/// `{"refresh_token":"<hex64>"}` 与 `aad_kat_token_refresh` KAT 的 plaintext 逐字节一致。
/// 解密失败、JSON 形状不对、或 `refresh_token` 不是合法 hex64 一律 `PairingError::BadPayload`——
/// 调用方按"一次无效"计入 §9.6 第 251 行的连续无效计数，不额外区分子原因（避免给攻击者
/// 泄露"是密文坏还是明文形状坏"的 oracle）。
pub(crate) fn open_token_refresh_request(
    k_pair: &[u8; 32],
    room_id: &str,
    device_id: &str,
    request_id: &str,
    ct: &str,
    n: &str,
) -> Result<String, PairingError> {
    let meta = token_refresh_meta(room_id, device_id, request_id);
    let plaintext = open(k_pair, &meta, ct, n).map_err(map_derive_error)?;
    let parsed: RefreshRequestPlaintext =
        serde_json::from_slice(&plaintext).map_err(|_| PairingError::BadPayload)?;
    if !is_hex64(&parsed.refresh_token) {
        return Err(PairingError::BadPayload);
    }
    Ok(parsed.refresh_token)
}

#[derive(Serialize)]
struct RefreshResponseTokens<'a> {
    capability_token: &'a str,
    refresh_token: &'a str,
}

/// S1i1 §2a/§2c：密封 `token.refresh.ok` 回执密文体——明文形状 `{"capability_token","refresh_token"}`
/// 与 `aad_kat_token_refresh_ok` KAT 的 plaintext 逐字节一致（同 `PairAcceptTokens` 沿用的字段名，
/// `capability_token` 是这份代码里"access token"的既有叫法，两处不重新发明新名字）。
pub(crate) fn seal_token_refresh_ok(
    k_pair: &[u8; 32],
    room_id: &str,
    device_id: &str,
    request_id: &str,
    access_token: &str,
    refresh_token: &str,
) -> (String, String) {
    let meta = token_refresh_ok_meta(room_id, device_id, request_id);
    let plaintext = Zeroizing::new(
        serde_json::to_vec(&RefreshResponseTokens {
            capability_token: access_token,
            refresh_token,
        })
        .expect("serializing string-only refresh response tokens cannot fail"),
    );
    seal(k_pair, &meta, plaintext.as_slice())
}

fn generate_token_hex() -> String {
    encode_hex(generate_key_32().as_ref())
}

fn generate_device_id() -> String {
    let random = generate_key_32();
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&random[..16]);
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex = encode_hex(&bytes);
    format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}

fn sha256_hex(value: &str) -> String {
    encode_hex(&Sha256::digest(value.as_bytes()))
}

/// S1i1 §9.6：常量时间比较 `refresh_token` 的 sha256 是否等于给定 hash——refresh journal 的
/// prev 命中判定复用它，不在 remote_pairing 模块之外重新散落哈希/常量时间比较逻辑。
pub(crate) fn refresh_token_hash_matches(refresh_token: &str, hash: &str) -> bool {
    constant_time_eq(sha256_hex(refresh_token).as_bytes(), hash.as_bytes())
}

/// Relay owner credentials are hashed as their 64-byte lowercase ASCII hex representation, not
/// as the decoded 32 random bytes. Keep this wrapper as the single desktop/relay contract entry.
pub(crate) fn desktop_credential_hash(credential: &str) -> String {
    sha256_hex(credential)
}

fn encode_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for &byte in bytes {
        encoded.push(HEX[(byte >> 4) as usize] as char);
        encoded.push(HEX[(byte & 0x0f) as usize] as char);
    }
    encoded
}

/// Best-effort constant-time equality for equal-length values. Length is not secret, so a
/// length mismatch may return early; byte equality never short-circuits.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }

    let mut diff = 0_u8;
    for (&left, &right) in a.iter().zip(b.iter()) {
        diff |= left ^ right;
    }
    diff == 0
}

/// T5e2（remote control M0 §5·W1 ADR）：配对/设备的存储装配——真密钥（K_room、逐设备
/// K_pair）落钥匙串（`KeyStore` trait，key 名见下），设备清单/令牌哈希落 app 数据库
/// （`db::remote_devices`）。本模块负责把纯逻辑结果搬进正确的存储，并装配需要跨 DB 与
/// `TokenBook` 保持一致的操作；不改 `PairingSession`/`handle_hello` 的协议逻辑。
pub(crate) mod store;

/// T5d-a.1 gateway 组合入口：先认证 hello，只有认证成功后才解析或生成 K_room；设备落库
/// 仍由 pair.done 闭环负责。返回 K_room 原文供 gateway 暂存在 SentAccept 中验证确认体。
pub(crate) fn remote_pairing_authenticate_hello(
    key_store: &dyn crate::keychain::KeyStore,
    session: &mut PairingSession,
    hello: &HelloFrame,
    now_secs: u64,
) -> Result<(AcceptOutcome, [u8; 32]), String> {
    let room_id = session.room_id.clone();
    let k_pair = authenticate_hello(session, hello, now_secs).map_err(|e| e.to_string())?;
    let k_room = store::resolve_k_room(key_store, &room_id)?;
    let outcome = finalize_hello(k_pair, &k_room);
    Ok((outcome, k_room))
}

/// T5e2 的 eager-persist 组合薄封装，保留给既有存储测试。T5d-a 的 gateway 闭环不能调用
/// 它：网络路径必须先用纯内存 `handle_hello` 生成 `AcceptOutcome`，等收到 `pair.done` 后才调
/// `store::persist_pairing_outcome`，否则会重新引入 ghost device。
pub(crate) fn remote_pairing_handle_hello(
    conn: &rusqlite::Connection,
    key_store: &dyn crate::keychain::KeyStore,
    session: &mut PairingSession,
    hello: &HelloFrame,
    now_secs: u64,
    now_ms: u64,
) -> Result<AcceptOutcome, String> {
    let room_id = session.room_id.clone();
    let k_pair = authenticate_hello(session, hello, now_secs).map_err(|e| e.to_string())?;
    let k_room = store::resolve_k_room(key_store, &room_id)?;
    let outcome = finalize_hello(k_pair, &k_room);
    store::persist_pairing_outcome(conn, key_store, &room_id, &outcome, now_secs, now_ms)?;
    Ok(outcome)
}

/// T5e2：房间 id 与设备 id 用同一枚 CSPRNG 生成器，只是长度不同——房间 id 取 16 字节
/// 编成 32 个 hex 字符（不带连字符），匹配 `remote_gateway::is_valid_room_id` 的
/// `len() == 32` 校验。
pub(crate) fn generate_room_id() -> String {
    let random = generate_key_32();
    encode_hex(&random[..16])
}

/// §9.5：pairing token 的 64-byte ASCII hex 原文作 IKM，派生 connect token 后再哈希其
/// 64-byte 小写 hex 文本。只返回哈希，connect token 不持久化也不进日志。
pub(crate) fn pairing_connect_token_hash(pairing_token_hex: &str) -> Result<String, String> {
    Ok(sha256_hex(&derive_connect_token(pairing_token_hex)))
}

#[cfg(test)]
mod tests;

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
pub(crate) mod store {
    #[cfg(test)]
    use super::ACCESS_LIFETIME_MS;
    use super::{
        device_access_expires_at_ms, device_refresh_until_ms, refresh_prev_alias_expires_at_ms,
        seal_token_refresh_ok, AcceptOutcome, DeviceTokens, RotatedTokens, TokenBook,
    };
    use crate::db;
    use crate::keychain::KeyStore;
    use crate::remote_crypto::generate_key_32;
    use base64::{engine::general_purpose::STANDARD, Engine as _};
    use rusqlite::Connection;
    use std::collections::HashMap;
    use zeroize::Zeroizing;

    fn k_pair_key_id(device_id: &str) -> String {
        format!("remote-kpair-{device_id}")
    }

    fn k_room_key_id(room_id: &str) -> String {
        format!("remote-kroom-{room_id}")
    }

    fn desktop_credential_key_id(room_id: &str) -> String {
        format!("remote-desktop-credential-{room_id}")
    }

    fn valid_desktop_credential(value: &str) -> bool {
        value.len() == 64
            && value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    }

    /// Resolve the room-scoped desktop owner credential. Existing entries are never overwritten:
    /// malformed keychain state fails closed, while absence creates one 256-bit CSPRNG value.
    pub(crate) fn resolve_desktop_credential(
        key_store: &dyn KeyStore,
        room_id: &str,
    ) -> Result<Zeroizing<String>, String> {
        let key_id = desktop_credential_key_id(room_id);
        if let Some(existing) = key_store.get(&key_id)? {
            if !valid_desktop_credential(&existing) {
                return Err(format!(
                    "keychain entry {key_id} is not a lowercase hex64 desktop credential"
                ));
            }
            return Ok(Zeroizing::new(existing));
        }
        create_desktop_credential(key_store, room_id)
    }

    /// Create a credential only for a brand-new room. Refusing an existing entry makes it
    /// impossible for a recovery path to rotate a live room's credential accidentally.
    pub(crate) fn create_desktop_credential(
        key_store: &dyn KeyStore,
        room_id: &str,
    ) -> Result<Zeroizing<String>, String> {
        let key_id = desktop_credential_key_id(room_id);
        if key_store.get(&key_id)?.is_some() {
            return Err(format!(
                "desktop credential already exists for room {room_id}"
            ));
        }
        let credential = Zeroizing::new(super::encode_hex(generate_key_32().as_ref()));
        key_store.set(&key_id, credential.as_str())?;
        Ok(credential)
    }

    fn decode_32(value: &str) -> Option<[u8; 32]> {
        STANDARD.decode(value).ok()?.try_into().ok()
    }

    /// S1i1 §1（现状锚点第 26 行）：refresh 请求/回执密文体用该设备自己的 K_pair——取法与
    /// `revoke_device` 删除的 key 名同源（`remote-kpair-<device_id>`）。`Ok(None)` = 钥匙串里
    /// 没有这个设备（未配对/已被 `revoke_device` 删除），调用方按"无效"处理，不是内部错误。
    pub(crate) fn load_k_pair(
        key_store: &dyn KeyStore,
        device_id: &str,
    ) -> Result<Option<Zeroizing<[u8; 32]>>, String> {
        let key_id = k_pair_key_id(device_id);
        let Some(existing) = key_store.get(&key_id)? else {
            return Ok(None);
        };
        let bytes = decode_32(&existing)
            .ok_or_else(|| format!("keychain entry {key_id} 不是合法的 32 字节 K_pair"))?;
        Ok(Some(Zeroizing::new(bytes)))
    }

    /// 取得房间的 K_room：钥匙串已有 `remote-kroom-<room_id>` 则解出复用；否则生成新的
    /// 32B CSPRNG 值、立即存入钥匙串再返回。T5d-a.1 的 gateway 闭环会在认证 hello 成功后调用它，
    /// 再用 K_room finalize；这里留下的只是幂等房间级共享密钥，
    /// 不包含待定设备身份，因此不构成 ghost device。同一房间的第二台及以后设备会复用它，
    /// 只是各自用自己的 K_pair 包裹一份。
    pub(crate) fn resolve_k_room(
        key_store: &dyn KeyStore,
        room_id: &str,
    ) -> Result<[u8; 32], String> {
        let key_id = k_room_key_id(room_id);
        if let Some(existing) = key_store.get(&key_id)? {
            return decode_32(&existing)
                .ok_or_else(|| format!("keychain entry {key_id} 不是合法的 32 字节 K_room"));
        }
        let generated = generate_key_32();
        key_store.set(&key_id, &STANDARD.encode(generated.as_ref()))?;
        Ok(*generated)
    }

    /// `handle_hello` 成功后的落地：`DeviceRecord` 落 `remote_devices` 表
    /// + 设备 K_pair 入钥匙串（key 名 `remote-kpair-<device_id>`；`access_expires_at = now_ms + 1h`，与
    /// `TokenBook::insert` 的 `ACCESS_LIFETIME_MS` 同源常量，两处口径不会漂）。对
    /// `room_id` 再确认一次 K_room 纯粹是防御性幂等兜底（gateway 的 hello 路径已经把它
    /// 存进钥匙串了，这里不会真的生成新值）。
    pub(crate) fn persist_pairing_outcome(
        conn: &Connection,
        key_store: &dyn KeyStore,
        room_id: &str,
        outcome: &AcceptOutcome,
        created_at_secs: u64,
        now_ms: u64,
    ) -> Result<(), String> {
        resolve_k_room(key_store, room_id)?;

        let device = &outcome.device_record;
        let access_expires_at_ms = device_access_expires_at_ms(now_ms)?;
        db::insert_remote_device(
            conn,
            &device.device_id,
            Some(room_id),
            "",
            &device.token_hash,
            &device.refresh_hash,
            access_expires_at_ms,
            created_at_secs as i64,
        )
        .map_err(|e| e.to_string())?;

        // DB 先行可避免 insert 失败留下孤儿 K_pair；反向的钥匙串失败会留下不可用设备行。
        key_store.set(
            &k_pair_key_id(&device.device_id),
            &STANDARD.encode(device.k_pair.as_slice()),
        )
    }

    /// S1i1 §9.6 第 250 行（桌面轮换全流程）·refresh 的唯一组合写入口：先在内存中只读校验并
    /// 生成候选令牌，领代（`next_registry_generation_in_transaction`）→ 同一个 DB 事务里写新
    /// access_hash/access_expires/refresh_hash/refresh_until + journal 全九字段 → 提交，成功后
    /// 才把候选提交进内存 TokenBook。事务失败 = 全无副作用（TokenBook 不动、钥匙串不动，
    /// outbox 入队是调用方的事，本函数不碰）。
    ///
    /// 调用方须先用 `TokenBook::matches_current_refresh` 确认命中"当前"再调用本函数——本函数
    /// 内部会用同一个 `refresh_token` 再跑一次 `prepare_refresh`（候选令牌只在这里真正生成，
    /// 探测阶段不会白算）。`prev_generation` 由调用方传入（读自轮换前的设备行 `generation` 列，
    /// TokenBook 不携带 registry 列，读一次即可，不必在本函数内部再查一遍 DB）；
    /// `prev_access_hash` 同理——S1i1 R5-4 返工：此前这里直接读内存 TokenBook 的
    /// `device.access_token_hash`，跟调用方手里已经查过的 DB 行 `row.token_hash` 是两个各自独立
    /// 的真相源（虽然在所有正确调用路径下二者恒等——TokenBook 的写入永远紧跟在本函数自己的 DB
    /// 事务提交之后，见函数末尾 `book.commit_refresh`，不存在第三条写入路径），改成跟
    /// `prev_generation` 一样由调用方传入 DB 行读到的权威值，不留"两个应该相等但各自独立"的
    /// 影子状态。
    /// `room_id`/`k_pair`/`request_id` 用于同一事务里密封回执密文体（journal 要求"重放同一份
    /// 回执"，密封必须在生成新候选令牌之后、写库之前完成，才能把 `response_ct/n` 一并存进
    /// journal）。
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn refresh_device_tokens(
        conn: &Connection,
        book: &mut TokenBook,
        device_id: &str,
        room_id: &str,
        prev_generation: i64,
        prev_access_hash: &str,
        k_pair: &[u8; 32],
        request_id: &str,
        refresh_token: &str,
        now_ms: u64,
    ) -> Result<RotatedTokens, String> {
        let candidate = book
            .prepare_refresh(device_id, refresh_token, now_ms)
            .map_err(|e| e.to_string())?;

        // 轮换前快照——prepare_refresh 只读，book 此刻仍是"旧"状态；journal 的 prev_refresh_hash
        // 字段直接取自这里，不必为此另外查一次 DB（access hash 已经由调用方传入，见上面函数
        // 文档）。
        let device = book
            .devices
            .get(device_id)
            .ok_or_else(|| format!("remote device {device_id} disappeared mid refresh"))?;
        let prev_access_hash = prev_access_hash.to_owned();
        let prev_refresh_hash = device.refresh_token_hash.clone();

        let prev_expires_at_ms = refresh_prev_alias_expires_at_ms(now_ms)?;
        let (response_ct, response_n) = seal_token_refresh_ok(
            k_pair,
            room_id,
            device_id,
            request_id,
            &candidate.access_token,
            &candidate.refresh_token,
        );

        let tx = conn.unchecked_transaction().map_err(|e| e.to_string())?;
        let generation =
            db::next_registry_generation_in_transaction(&tx, room_id).map_err(|e| e.to_string())?;
        let refresh_until_ms = device_refresh_until_ms(now_ms)?;
        let access_expires_at_ms = i64::try_from(candidate.access_expires_at_ms)
            .map_err(|_| "access expiry exceeds SQLite INTEGER range".to_owned())?;

        let changed = db::update_remote_device_tokens(
            &tx,
            device_id,
            &candidate.access_token_hash,
            &candidate.refresh_token_hash,
            access_expires_at_ms,
        )
        .map_err(|e| e.to_string())?;
        if !changed {
            return Err(format!(
                "remote device {device_id} is missing or revoked; token refresh was not persisted"
            ));
        }
        if !db::set_remote_device_registry_in_transaction(
            &tx,
            device_id,
            room_id,
            generation,
            refresh_until_ms,
        )
        .map_err(|e| e.to_string())?
        {
            return Err(format!(
                "remote device {device_id} disappeared during refresh registry write"
            ));
        }
        let journal = db::RemoteRefreshJournal {
            request_id: request_id.to_owned(),
            generation,
            prev_generation,
            prev_access_hash: prev_access_hash.clone(),
            prev_refresh_hash,
            response_ct: response_ct.clone(),
            response_n: response_n.clone(),
            prev_expires_at: prev_expires_at_ms,
            // §9.6 没有单独钉死回执重放窗口的数值；本单按 refresh_prev_alias_expires_at_ms
            // 同源取值（依据见该函数与 PREV_ALIAS_LIFETIME_MS 上的注释），两个窗口刻意保持
            // 同步，不另立一个数值不同的新常量。
            response_expires: prev_expires_at_ms,
        };
        if !db::store_refresh_journal(&tx, device_id, &journal).map_err(|e| e.to_string())? {
            return Err(format!(
                "remote device {device_id} disappeared during refresh journal write"
            ));
        }
        tx.commit().map_err(|e| e.to_string())?;

        book.commit_refresh(device_id, &candidate);

        Ok(RotatedTokens {
            generation,
            access_token_hash: candidate.access_token_hash.clone(),
            access_expires_at_ms,
            refresh_until_ms,
            prev_generation,
            prev_access_hash,
            prev_expires_at_ms,
            response_ct,
            response_n,
        })
    }

    /// 从 `remote_devices` 未吊销行重建 `TokenBook`（进程重启后用它重放访问/刷新校验）。
    /// 直接构造 `TokenBook { devices }`——`store` 是 `remote_pairing` 的子模块，按 Rust
    /// 隐私规则可见其祖先模块定义的私有字段，不必给 `TokenBook` 另开一个仅供本模块用的
    /// 公开构造器（那样会碰到纯逻辑区）。
    pub(crate) fn load_token_book(conn: &Connection) -> Result<TokenBook, String> {
        let rows = db::list_remote_devices(conn).map_err(|e| e.to_string())?;
        let mut devices = HashMap::new();
        for row in rows {
            if row.revoked_at.is_some() {
                continue;
            }
            let Some(access_expires_at_ms) = u64::try_from(row.access_expires_at)
                .ok()
                .filter(|expires_at| *expires_at > 0)
            else {
                eprintln!(
                    "remote_pairing::store::load_token_book: skipping device {}: non-positive access expiry",
                    row.device_id
                );
                continue;
            };
            devices.insert(
                row.device_id.clone(),
                DeviceTokens {
                    device_id: row.device_id,
                    access_token_hash: row.token_hash,
                    access_expires_at_ms,
                    refresh_token_hash: row.refresh_hash,
                    revoked: false,
                },
            );
        }
        Ok(TokenBook { devices })
    }

    /// 吊销一个设备：db 标记 + 钥匙串删 K_pair。K_pair 删除失败按既有先例
    /// （lib.rs `delete_agent_with_store`）降级为 best-effort——不让钥匙串偶发故障挡住
    /// 吊销本身生效（db 标记才是「这个设备还能不能登录」的权威判据）。K_room 轮换重包裹
    /// （撤销后房间密钥理应换新、对剩余设备重新包裹下发）留 M2，这里不做。
    pub(crate) fn revoke_device(
        conn: &Connection,
        key_store: &dyn KeyStore,
        device_id: &str,
        now_secs: i64,
    ) -> Result<(), String> {
        db::revoke_remote_device(conn, device_id, now_secs).map_err(|e| e.to_string())?;
        if let Err(e) = key_store.delete(&k_pair_key_id(device_id)) {
            eprintln!(
                "remote_pairing::store::revoke_device: keychain delete failed for {device_id}: {e}"
            );
        }
        Ok(())
    }

    /// DB revoke 成功后立即同步当前进程的 TokenBook；不存在的内存设备按 no-op 处理。
    pub(crate) fn revoke_device_and_sync(
        conn: &Connection,
        key_store: &dyn KeyStore,
        book: &mut TokenBook,
        device_id: &str,
        now_secs: i64,
    ) -> Result<(), String> {
        revoke_device(conn, key_store, device_id, now_secs)?;
        book.revoke(device_id);
        Ok(())
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use crate::keychain::FakeKeyStore;
        use crate::remote_pairing::{
            sha256_hex, token_refresh_ok_meta, DeviceRecord, PairingError,
        };
        use rusqlite::Connection;
        use zeroize::Zeroizing;

        const NOW_SECS: u64 = 1_700_000_000;
        const NOW_MS: u64 = 1_700_000_000_000;
        const ROOM_ID: &str = "room-store-test";

        fn mem() -> Connection {
            let conn = Connection::open_in_memory().unwrap();
            db::init_schema(&conn).unwrap();
            conn
        }

        /// 测试专用：解出 `token.refresh.ok` 回执密文体——不是给生产代码复用的公开 API，
        /// 只是让测试能从 `RotatedTokens::response_ct/n`（不携带明文）里拿到新令牌明文，
        /// 顺带把真 seal/open 内核在这条路径上跑一遍（不是手工复刻一份等价断言）。
        fn decrypt_refresh_ok(
            k_pair: &[u8; 32],
            room_id: &str,
            device_id: &str,
            request_id: &str,
            ct: &str,
            n: &str,
        ) -> (String, String) {
            let meta = token_refresh_ok_meta(room_id, device_id, request_id);
            let plaintext = crate::remote_crypto::open(k_pair, &meta, ct, n).unwrap();
            let value: serde_json::Value = serde_json::from_slice(&plaintext).unwrap();
            (
                value["capability_token"].as_str().unwrap().to_owned(),
                value["refresh_token"].as_str().unwrap().to_owned(),
            )
        }

        fn sample_outcome(device_id: &str) -> AcceptOutcome {
            AcceptOutcome {
                k_room_wrapped_ct: "ct".into(),
                k_room_wrapped_n: "n".into(),
                capability_token: "cap".into(),
                refresh_token: "refresh".into(),
                device_record: DeviceRecord {
                    device_id: device_id.to_string(),
                    k_pair: Zeroizing::new([7_u8; 32]),
                    token_hash: format!("hash-{device_id}"),
                    refresh_hash: format!("refresh-hash-{device_id}"),
                },
            }
        }

        #[test]
        fn resolve_k_room_generates_once_and_reuses_afterwards() {
            let store = FakeKeyStore::default();

            let first = resolve_k_room(&store, ROOM_ID).unwrap();
            let second = resolve_k_room(&store, ROOM_ID).unwrap();

            assert_eq!(first, second, "同一房间第二次必须拿到同一把 K_room");
            assert!(store.get(&k_room_key_id(ROOM_ID)).unwrap().is_some());
        }

        #[test]
        fn resolve_k_room_differs_across_rooms() {
            let store = FakeKeyStore::default();

            let room_a = resolve_k_room(&store, "room-a").unwrap();
            let room_b = resolve_k_room(&store, "room-b").unwrap();

            assert_ne!(room_a, room_b);
        }

        #[test]
        fn desktop_credential_is_generated_once_per_room() {
            let store = FakeKeyStore::default();

            let first = resolve_desktop_credential(&store, ROOM_ID).unwrap();
            let second = resolve_desktop_credential(&store, ROOM_ID).unwrap();

            assert_eq!(first.as_str(), second.as_str());
            assert_eq!(first.len(), 64);
            assert!(first
                .bytes()
                .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f')));
        }

        #[test]
        fn desktop_credentials_are_isolated_by_room() {
            let store = FakeKeyStore::default();

            let room_a = resolve_desktop_credential(&store, "room-a").unwrap();
            let room_b = resolve_desktop_credential(&store, "room-b").unwrap();

            assert_ne!(room_a.as_str(), room_b.as_str());
        }

        #[test]
        fn persist_pairing_outcome_writes_keychain_and_db_row() {
            let conn = mem();
            let store = FakeKeyStore::default();
            let outcome = sample_outcome("dev-1");

            persist_pairing_outcome(&conn, &store, ROOM_ID, &outcome, NOW_SECS, NOW_MS).unwrap();

            let stored_k_pair = store.get(&k_pair_key_id("dev-1")).unwrap().unwrap();
            assert_eq!(
                STANDARD.decode(stored_k_pair).unwrap(),
                outcome.device_record.k_pair.to_vec()
            );
            let rows = db::list_remote_devices(&conn).unwrap();
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].device_id, "dev-1");
            assert_eq!(rows[0].room_id.as_deref(), Some(ROOM_ID));
            assert_eq!(rows[0].token_hash, "hash-dev-1");
            assert_eq!(
                rows[0].access_expires_at,
                (NOW_MS + ACCESS_LIFETIME_MS) as i64
            );
            assert_eq!(rows[0].revoked_at, None);
        }

        #[test]
        fn persist_pairing_outcome_db_failure_does_not_leave_orphan_k_pair() {
            let conn = mem();
            let store = FakeKeyStore::default();
            let outcome = sample_outcome("dev-duplicate");
            db::insert_remote_device(
                &conn,
                "dev-duplicate",
                None,
                "existing",
                "existing-token-hash",
                "existing-refresh-hash",
                (NOW_MS + ACCESS_LIFETIME_MS) as i64,
                NOW_SECS as i64,
            )
            .unwrap();

            assert!(
                persist_pairing_outcome(&conn, &store, ROOM_ID, &outcome, NOW_SECS, NOW_MS,)
                    .is_err()
            );
            assert_eq!(store.get(&k_pair_key_id("dev-duplicate")).unwrap(), None);
        }

        #[test]
        fn refresh_rotation_survives_token_book_reload_without_reviving_old_refresh() {
            let conn = mem();
            let k_pair = [7_u8; 32];
            db::insert_remote_device(
                &conn,
                "dev-refresh",
                Some(ROOM_ID),
                "",
                &sha256_hex("access-old"),
                &sha256_hex("refresh-old"),
                (NOW_MS + ACCESS_LIFETIME_MS) as i64,
                NOW_SECS as i64,
            )
            .unwrap();
            // 领代计数器与行上手工写的 generation=1 对齐——真实流程里两者永远同步产生
            // （见 `process_pair_done_with_registry`），测试手工摆状态时得自己保持这份同步，
            // 否则 `next_registry_generation_in_transaction` 会把 1 重新发一遍。
            db::bump_registry_counter_to(&conn, ROOM_ID, 1).unwrap();
            assert!(db::set_remote_device_registry(
                &conn,
                "dev-refresh",
                ROOM_ID,
                1,
                (NOW_MS + super::super::REFRESH_LIFETIME_MS) as i64,
            )
            .unwrap());
            let mut book = load_token_book(&conn).unwrap();

            let rotated = refresh_device_tokens(
                &conn,
                &mut book,
                "dev-refresh",
                ROOM_ID,
                1,
                &sha256_hex("access-old"),
                &k_pair,
                "req-1",
                "refresh-old",
                NOW_MS + 10,
            )
            .unwrap();
            assert_eq!(rotated.generation, 2, "领代必须严格高于轮换前的 1");
            assert_eq!(rotated.prev_generation, 1);
            let (new_access, new_refresh) = decrypt_refresh_ok(
                &k_pair,
                ROOM_ID,
                "dev-refresh",
                "req-1",
                &rotated.response_ct,
                &rotated.response_n,
            );
            let mut reloaded = load_token_book(&conn).unwrap();

            // 旧 refresh token 命中的已经是"prev"而不是"current"——store::refresh_device_tokens
            // 只负责命中当前分支的组合写，prev 命中交给上层编排（本单 §2c），这里只断言
            // TokenBook 已经不再认它是当前令牌。
            assert!(reloaded
                .prepare_refresh("dev-refresh", "refresh-old", NOW_MS + 11)
                .is_err());
            assert_eq!(
                reloaded.verify_access("dev-refresh", &new_access, NOW_MS + 11),
                Ok(())
            );
            let row_after_first_rotation = db::get_remote_device(&conn, "dev-refresh")
                .unwrap()
                .unwrap();
            let generation_2 = row_after_first_rotation.generation.unwrap();
            assert_eq!(generation_2, 2);
            let rotated_again = refresh_device_tokens(
                &conn,
                &mut reloaded,
                "dev-refresh",
                ROOM_ID,
                generation_2,
                &row_after_first_rotation.token_hash,
                &k_pair,
                "req-2",
                &new_refresh,
                NOW_MS + 11,
            )
            .unwrap();
            assert_eq!(rotated_again.generation, 3);
            assert_eq!(rotated_again.prev_generation, 2);
        }

        #[test]
        fn refresh_db_rejection_leaves_in_memory_tokens_unchanged() {
            let conn = mem();
            let k_pair = [7_u8; 32];
            db::insert_remote_device(
                &conn,
                "dev-db-rejected",
                Some(ROOM_ID),
                "",
                &sha256_hex("access-old"),
                &sha256_hex("refresh-old"),
                (NOW_MS + ACCESS_LIFETIME_MS) as i64,
                NOW_SECS as i64,
            )
            .unwrap();
            db::bump_registry_counter_to(&conn, ROOM_ID, 1).unwrap();
            assert!(db::set_remote_device_registry(
                &conn,
                "dev-db-rejected",
                ROOM_ID,
                1,
                (NOW_MS + super::super::REFRESH_LIFETIME_MS) as i64,
            )
            .unwrap());
            let mut book = load_token_book(&conn).unwrap();
            let before = book.devices.get("dev-db-rejected").unwrap();
            let before_access_hash = before.access_token_hash.clone();
            let before_refresh_hash = before.refresh_token_hash.clone();
            let before_expires_at = before.access_expires_at_ms;
            db::revoke_remote_device(&conn, "dev-db-rejected", (NOW_SECS + 5) as i64).unwrap();

            assert!(refresh_device_tokens(
                &conn,
                &mut book,
                "dev-db-rejected",
                ROOM_ID,
                1,
                &sha256_hex("access-old"),
                &k_pair,
                "req-1",
                "refresh-old",
                NOW_MS + 10,
            )
            .is_err());

            let after = book.devices.get("dev-db-rejected").unwrap();
            assert_eq!(after.access_token_hash, before_access_hash);
            assert_eq!(after.refresh_token_hash, before_refresh_hash);
            assert_eq!(after.access_expires_at_ms, before_expires_at);
            assert_eq!(
                book.verify_access("dev-db-rejected", "access-old", NOW_MS + 11),
                Ok(())
            );
            assert!(book
                .prepare_refresh("dev-db-rejected", "refresh-old", NOW_MS + 11)
                .is_ok());
            assert_eq!(
                db::load_refresh_journal(&conn, "dev-db-rejected").unwrap(),
                None,
                "DB 拒绝的轮换不得留下 journal 半成品"
            );
        }

        #[test]
        fn refresh_device_tokens_writes_registry_columns_and_journal_in_one_transaction() {
            let conn = mem();
            let k_pair = [9_u8; 32];
            db::insert_remote_device(
                &conn,
                "dev-tx",
                Some(ROOM_ID),
                "",
                &sha256_hex("access-old"),
                &sha256_hex("refresh-old"),
                (NOW_MS + ACCESS_LIFETIME_MS) as i64,
                NOW_SECS as i64,
            )
            .unwrap();
            db::bump_registry_counter_to(&conn, ROOM_ID, 5).unwrap();
            assert!(db::set_remote_device_registry(
                &conn,
                "dev-tx",
                ROOM_ID,
                5,
                (NOW_MS + super::super::REFRESH_LIFETIME_MS) as i64,
            )
            .unwrap());
            let mut book = load_token_book(&conn).unwrap();

            let rotated = refresh_device_tokens(
                &conn,
                &mut book,
                "dev-tx",
                ROOM_ID,
                5,
                &sha256_hex("access-old"),
                &k_pair,
                "req-tx",
                "refresh-old",
                NOW_MS,
            )
            .unwrap();

            let row = db::get_remote_device(&conn, "dev-tx").unwrap().unwrap();
            assert_eq!(row.generation, Some(6));
            assert_eq!(row.token_hash, rotated.access_token_hash);
            let journal = db::load_refresh_journal(&conn, "dev-tx").unwrap().unwrap();
            assert_eq!(journal.request_id, "req-tx");
            assert_eq!(journal.generation, 6);
            assert_eq!(journal.prev_generation, 5);
            assert_eq!(journal.prev_access_hash, sha256_hex("access-old"));
            assert_eq!(journal.prev_refresh_hash, sha256_hex("refresh-old"));
            assert_eq!(journal.response_ct, rotated.response_ct);
            assert_eq!(journal.response_n, rotated.response_n);
            assert_eq!(journal.prev_expires_at, rotated.prev_expires_at_ms);
            assert_eq!(journal.response_expires, rotated.prev_expires_at_ms);
        }

        #[test]
        fn load_token_book_rebuilds_only_unrevoked_devices() {
            let conn = mem();
            let store = FakeKeyStore::default();
            persist_pairing_outcome(
                &conn,
                &store,
                ROOM_ID,
                &sample_outcome("dev-active"),
                NOW_SECS,
                NOW_MS,
            )
            .unwrap();
            persist_pairing_outcome(
                &conn,
                &store,
                ROOM_ID,
                &sample_outcome("dev-revoked"),
                NOW_SECS,
                NOW_MS,
            )
            .unwrap();
            db::revoke_remote_device(&conn, "dev-revoked", NOW_SECS as i64).unwrap();

            let book = load_token_book(&conn).unwrap();

            assert_eq!(
                book.verify_access("dev-active", "irrelevant", NOW_MS),
                Err(PairingError::TokenMismatch),
                "设备应存在于重建后的 TokenBook 里（校验会走到 TokenMismatch 而非 NotFound）"
            );
            assert_eq!(
                book.verify_access("dev-revoked", "irrelevant", NOW_MS),
                Err(PairingError::NotFound),
                "已吊销设备不该出现在重建后的 TokenBook 里"
            );
        }

        #[test]
        fn load_token_book_skips_a_bad_device_without_losing_good_devices() {
            let conn = mem();
            for (device_id, expires_at) in [
                ("dev-good-a", (NOW_MS + 1_000) as i64),
                ("dev-bad", 0),
                ("dev-good-b", (NOW_MS + 2_000) as i64),
            ] {
                conn.execute(
                    "INSERT INTO remote_devices \
                     (device_id, name, token_hash, refresh_hash, access_expires_at, created_at) \
                     VALUES (?1, '', ?2, ?3, ?4, ?5)",
                    (
                        device_id,
                        sha256_hex(&format!("access-{device_id}")),
                        sha256_hex(&format!("refresh-{device_id}")),
                        expires_at,
                        NOW_SECS as i64,
                    ),
                )
                .unwrap();
            }

            let book = load_token_book(&conn).unwrap();

            assert_eq!(book.devices.len(), 2);
            assert!(book.devices.contains_key("dev-good-a"));
            assert!(book.devices.contains_key("dev-good-b"));
            assert!(!book.devices.contains_key("dev-bad"));
        }

        #[test]
        fn load_token_book_loads_all_valid_devices() {
            let conn = mem();
            for (offset, device_id) in ["dev-good-a", "dev-good-b"].into_iter().enumerate() {
                db::insert_remote_device(
                    &conn,
                    device_id,
                    None,
                    "",
                    &sha256_hex(&format!("access-{device_id}")),
                    &sha256_hex(&format!("refresh-{device_id}")),
                    (NOW_MS + 1_000 + offset as u64) as i64,
                    NOW_SECS as i64,
                )
                .unwrap();
            }

            let book = load_token_book(&conn).unwrap();

            assert_eq!(book.devices.len(), 2);
            assert!(book.devices.contains_key("dev-good-a"));
            assert!(book.devices.contains_key("dev-good-b"));
        }

        #[test]
        fn revoke_device_marks_db_and_deletes_keychain_entry() {
            let conn = mem();
            let store = FakeKeyStore::default();
            persist_pairing_outcome(
                &conn,
                &store,
                ROOM_ID,
                &sample_outcome("dev-1"),
                NOW_SECS,
                NOW_MS,
            )
            .unwrap();
            assert!(store.get(&k_pair_key_id("dev-1")).unwrap().is_some());

            revoke_device(&conn, &store, "dev-1", (NOW_SECS + 10) as i64).unwrap();

            let rows = db::list_remote_devices(&conn).unwrap();
            assert_eq!(rows[0].revoked_at, Some((NOW_SECS + 10) as i64));
            assert!(store.get(&k_pair_key_id("dev-1")).unwrap().is_none());
        }

        #[test]
        fn revoke_device_and_sync_invalidates_loaded_and_reloaded_token_books() {
            let conn = mem();
            let store = FakeKeyStore::default();
            let mut outcome = sample_outcome("dev-revoke-sync");
            outcome.device_record.token_hash = sha256_hex("access-active");
            outcome.device_record.refresh_hash = sha256_hex("refresh-active");
            persist_pairing_outcome(&conn, &store, ROOM_ID, &outcome, NOW_SECS, NOW_MS).unwrap();
            let mut book = load_token_book(&conn).unwrap();

            revoke_device_and_sync(
                &conn,
                &store,
                &mut book,
                "dev-revoke-sync",
                (NOW_SECS + 10) as i64,
            )
            .unwrap();

            assert_eq!(
                book.verify_access("dev-revoke-sync", "access-active", NOW_MS + 11),
                Err(PairingError::Revoked)
            );
            let reloaded = load_token_book(&conn).unwrap();
            assert_eq!(
                reloaded.verify_access("dev-revoke-sync", "access-active", NOW_MS + 11),
                Err(PairingError::NotFound)
            );
        }
    }
}

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

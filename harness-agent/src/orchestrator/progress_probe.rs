use std::path::Path;

fn stable_hash_bytes(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

pub(crate) fn file_content_hash(path: &Path) -> Option<u64> {
    std::fs::read(path)
        .ok()
        .map(|bytes| stable_hash_bytes(&bytes))
}

pub(crate) fn shell_command_for_progress(args: &str) -> Option<String> {
    serde_json::from_str::<serde_json::Value>(args)
        .ok()
        .and_then(|v| v.get("command").and_then(|c| c.as_str()).map(String::from))
}

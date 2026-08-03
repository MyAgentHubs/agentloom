//! 小工具：字符边界安全的字符串截断。
//!
//! `String::truncate(n)` 按**字节**位置截断——若 `n` 落在多字节 UTF-8 字符
//! 中间会 panic（`assertion failed: self.is_char_boundary(new_len)`）。
//! 任何可能含非 ASCII 用户内容（中文/emoji/…）的摘要/预览类截断都不能直接用
//! `truncate`，一律走这里。

/// 把 `s` 截到不超过 `max_bytes` 字节，若截断点落在字符中间则往回退到最近的
/// 字符边界（绝不 panic、绝不切碎多字节字符）。若 `s.len() <= max_bytes` 则
/// 不做任何改动。
pub(crate) fn truncate_at_char_boundary(s: &mut String, max_bytes: usize) {
    if s.len() <= max_bytes {
        return;
    }
    let mut boundary = max_bytes;
    while boundary > 0 && !s.is_char_boundary(boundary) {
        boundary -= 1;
    }
    s.truncate(boundary);
}

#[cfg(test)]
mod tests {
    use super::truncate_at_char_boundary;

    #[test]
    fn no_op_when_already_within_limit() {
        let mut s = "hello".to_string();
        truncate_at_char_boundary(&mut s, 80);
        assert_eq!(s, "hello");
    }

    #[test]
    fn exact_boundary_length_is_untouched() {
        let mut s = "a".repeat(80);
        truncate_at_char_boundary(&mut s, 80);
        assert_eq!(s.len(), 80);
    }

    #[test]
    fn one_byte_over_ascii_boundary_truncates_by_one() {
        let mut s = "a".repeat(81);
        truncate_at_char_boundary(&mut s, 80);
        assert_eq!(s.len(), 80);
    }

    #[test]
    fn one_byte_under_limit_is_untouched() {
        let mut s = "a".repeat(79);
        truncate_at_char_boundary(&mut s, 80);
        assert_eq!(s.len(), 79);
    }

    #[test]
    fn pure_ascii_truncates_exactly_at_max_bytes() {
        let mut s = "x".repeat(200);
        truncate_at_char_boundary(&mut s, 64);
        assert_eq!(s.len(), 64);
    }

    #[test]
    fn all_multibyte_chars_backs_off_to_char_boundary() {
        // 每个中文字符占 3 字节；80 不是 3 的倍数，因此按字节裁到 80 会切在
        // 字符中间——必须回退到最近边界（78）而不是 panic。
        let mut s = "中".repeat(40); // 120 字节
        truncate_at_char_boundary(&mut s, 80);
        assert!(s.len() <= 80);
        assert!(s.is_char_boundary(s.len()));
        assert_eq!(s.len(), 78); // 26 个完整汉字 = 78 字节
        assert_eq!(s.chars().count(), 26);
    }

    #[test]
    fn empty_string_is_untouched() {
        let mut s = String::new();
        truncate_at_char_boundary(&mut s, 80);
        assert_eq!(s, "");
    }

    #[test]
    fn max_bytes_zero_truncates_to_empty() {
        let mut s = "hello".to_string();
        truncate_at_char_boundary(&mut s, 0);
        assert_eq!(s, "");
    }
}

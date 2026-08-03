/// 从可能裹着 markdown 代码围栏 / 前导散文的文本里抽出 JSON 对象切片。
/// 不验证 JSON 合法性——只"剥壳"，把切片交给调用方严格解析。
/// 规则：① trim；② 若被代码围栏（```或```json … ```）包裹，剥首尾围栏；
/// ③ 若仍不以 '{' 开头，取第一个 '{' 到最后一个 '}'（闭区间）；④ 都不满足则原样返回。
pub(crate) fn extract_json_object(raw: &str) -> &str {
    let mut s = raw.trim();

    if s.starts_with("```") {
        if let Some(newline) = s.find('\n') {
            s = &s[newline + 1..];
        } else {
            s = &s[3..];
        }

        s = s.trim_end();
        if s.ends_with("```") {
            s = &s[..s.len() - 3];
        }
        s = s.trim();
    }

    if !s.starts_with('{') {
        if let (Some(start), Some(end)) = (s.find('{'), s.rfind('}')) {
            if start <= end {
                s = &s[start..=end];
            }
        }
    }

    s
}

#[cfg(test)]
mod tests {
    #[test]
    fn fenced_json_lang() {
        let input = "```json\n{ \"tasks\": [] }\n```";

        let extracted = super::extract_json_object(input);

        assert_eq!(extracted.trim(), "{ \"tasks\": [] }");
    }

    #[test]
    fn fenced_no_lang() {
        let input = "```\n{\"a\":1}\n```";

        let extracted = super::extract_json_object(input);

        assert_eq!(extracted.trim(), "{\"a\":1}");
    }

    #[test]
    fn bare_object_unchanged() {
        let input = "{\"a\":1}";

        let extracted = super::extract_json_object(input);

        assert_eq!(extracted.trim(), "{\"a\":1}");
    }

    #[test]
    fn prose_preamble() {
        let input = "Here is the plan:\n{\"a\":1}";

        let extracted = super::extract_json_object(input);

        assert_eq!(extracted.trim(), "{\"a\":1}");
    }

    #[test]
    fn garbage_still_fails_serde() {
        let input = "sorry, I cannot";

        let extracted = super::extract_json_object(input);

        assert!(serde_json::from_str::<serde_json::Value>(extracted).is_err());
    }

    #[test]
    fn parse_worklist_accepts_fenced() {
        let input = "```json\n{\"tasks\":[{\"id\":\"t1\",\"intent\":\"x\",\"files_scope\":[\"src/lib.rs\"],\"acceptance_cmd\":\"cargo test\",\"max_turns\":8}]}\n```";

        let tasks = crate::plan::contract::parse_worklist(input).expect("worklist parses");

        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].id, "t1");
    }
}

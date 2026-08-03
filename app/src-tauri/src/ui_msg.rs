pub fn al_err(code: &str, params: &[(&str, String)]) -> String {
    if params.is_empty() {
        return format!("AL_ERR:{code}");
    }

    let params: serde_json::Map<String, serde_json::Value> = params
        .iter()
        .map(|(key, value)| ((*key).to_string(), serde_json::Value::String(value.clone())))
        .collect();
    format!(
        "AL_ERR:{code}:{}",
        serde_json::to_string(&params).expect("string-only AL_ERR params must serialize")
    )
}

#[cfg(test)]
mod tests {
    use super::al_err;

    #[test]
    fn renders_envelope_without_params() {
        assert_eq!(
            al_err("landing.noEvidence", &[]),
            "AL_ERR:landing.noEvidence"
        );
    }

    #[test]
    fn renders_envelope_with_params() {
        assert_eq!(
            al_err("landing.protectedPath", &[("paths", "docs/a.md".into())]),
            r#"AL_ERR:landing.protectedPath:{"paths":"docs/a.md"}"#,
        );
    }

    #[test]
    fn escapes_quotes_newlines_and_chinese_in_json_params() {
        assert_eq!(
            al_err(
                "landing.protectedPath",
                &[("paths", "含中文\n\"quoted\"".into())],
            ),
            "AL_ERR:landing.protectedPath:{\"paths\":\"含中文\\n\\\"quoted\\\"\"}",
        );
    }
}

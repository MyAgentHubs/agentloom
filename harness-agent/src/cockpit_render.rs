/// 从结构化 Criterion 渲成模型一看就懂、不会照跑的话（不暴露 cmd:/contains:/judge: 内部记号）。
pub fn render_criterion_for_model(c: &crate::goal::Criterion) -> String {
    match &c.verifier {
        crate::goal::Verifier::Verifiable {
            check_cmd,
            success: crate::goal::SuccessRule::ExitZero,
            ..
        } => format!("验收检查（须 exit 0）: {check_cmd}"),
        crate::goal::Verifier::Verifiable {
            check_cmd,
            success: crate::goal::SuccessRule::StdoutContains(needle),
            ..
        } => format!("验收检查（输出须含 \"{needle}\"）: {check_cmd}"),
        crate::goal::Verifier::Judgmental { rubric } => {
            format!("验收（人工/评判标准）: {rubric}")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::goal::parse_criteria;

    #[test]
    fn renders_cmd_criterion_as_plain_acceptance_no_leak() {
        let c = &parse_criteria(&["cmd: cargo test".into()]).unwrap()[0];
        let s = render_criterion_for_model(c);
        assert!(s.contains("验收检查") && s.contains("cargo test"));
        assert!(!s.contains("cmd:"));
    }
}

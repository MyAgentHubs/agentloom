//! 刀2.1 · Lead Decision Loop 的结构化动作契约（spec §5）。
//! lead one-shot 每回合只吐一个 LeadAction（6 动作）；本模块负责「文本 → 结构化动作」的解析 + 校验（parser 在 T2）。
//! 复刻 lead_draft::parse_driver_draft 的围栏模式。provider 无关（留缝·刀2.x 接 codex/deepseek 当 lead 只换 spawn·不换 parse）。

/// lead 每回合的一个动作（内部 tag enum·变体字段平铺到顶层·与 spec §5 JSON 一致）。
/// rationale 进 decision_ledger·给用户可审计·校验强制非空（见 T2 validate）。
#[allow(dead_code)] // Plan 2 lead_step 接线（clippy --lib 不认 #[cfg(test)] 调用者）
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum LeadAction {
    /// 直接回复（缺省动作）。刀2.1 中 reply = 转一次真 Normal 流式 send（Plan 3 接线）·payload 只需 rationale。
    Reply {
        #[serde(default)]
        rationale: String,
    },
    /// 派 1 个 worker 干一个 task。scope_files = lead 声明的已存在文件写改范围。
    DispatchWorker {
        #[serde(default)]
        rationale: String,
        task: String,
        #[serde(default)]
        scope_files: Vec<String>,
        #[serde(default)]
        agent_hint: Option<String>,
        #[serde(default)]
        goal_title: Option<String>,
    },
    /// 提议一条验证命令（用户确认后才 run）。
    ProposeVerifier {
        #[serde(default)]
        rationale: String,
        cmd: String,
    },
    /// 关口交回用户（askQ·带推荐项）。
    AskUser {
        #[serde(default)]
        rationale: String,
        question: String,
        #[serde(default)]
        options: Vec<String>,
        #[serde(default)]
        recommended: Option<String>,
    },
    /// 收工（带 evidence 引用·无证据条只能 unresolved/waived）。
    Finish {
        #[serde(default)]
        rationale: String,
        #[serde(default)]
        evidence_refs: Vec<String>,
    },
    /// 把本目标改动 ff 落地到当前分支（不 push）。
    Commit {
        #[serde(default)]
        rationale: String,
    },
    /// 把当前分支推到远端（必要时先落地）。
    Push {
        #[serde(default)]
        rationale: String,
    },
    /// 开 PR（必要时先落地 + push）。
    CreatePr {
        #[serde(default)]
        rationale: String,
        #[serde(default)]
        title: Option<String>,
        #[serde(default)]
        body: Option<String>,
    },
    /// Local 发布到 GitHub（建远程 + 推）。
    Publish {
        #[serde(default)]
        rationale: String,
        #[serde(default)]
        repo_name: Option<String>,
        #[serde(default)]
        private: Option<bool>,
    },
}

/// 解析失败分类（复刻 lead_draft::DraftParseError·三档）。
#[allow(dead_code)] // Plan 2 lead_step 接线
#[derive(Debug, Clone, PartialEq)]
pub enum LeadActionParseError {
    /// final_text 非合法 JSON。
    NotJson(String),
    /// JSON 合法但不符 LeadAction schema（未知 action / 缺 required / 类型错）。
    SchemaMismatch(String),
    /// schema 合法但语义非法（如 dispatch_worker.task 为空 / rationale 空）。
    SemanticInvalid(String),
}

impl LeadActionParseError {
    /// 重试时回注下一次 prompt 的可执行提示（防模型盲重试不收敛）。
    #[allow(dead_code)] // Plan 2 lead_step 接线
    pub fn retry_hint(&self) -> String {
        match self {
            LeadActionParseError::NotJson(e) => {
                format!("上次输出不是合法 JSON（{e}）。请只输出一个 JSON 对象·不要加解释文字。")
            }
            LeadActionParseError::SchemaMismatch(e) => format!(
                "上次 JSON 不符 LeadAction schema（{e}）。action 必须是 reply/dispatch_worker/propose_verifier/ask_user/finish/commit/push/create_pr/publish 之一·且带该动作的必需字段。"
            ),
            LeadActionParseError::SemanticInvalid(e) => {
                format!("上次动作语义非法（{e}）。请修正后重出。")
            }
        }
    }
}

impl LeadAction {
    /// 取该动作的 rationale（所有变体都有·进 ledger 用·校验强制非空）。
    /// pub：Plan 2 lead_step 落 decision_ledger 时直接取（终审两路建议·免 match-everywhere）。
    pub fn rationale(&self) -> &str {
        match self {
            LeadAction::Reply { rationale }
            | LeadAction::DispatchWorker { rationale, .. }
            | LeadAction::ProposeVerifier { rationale, .. }
            | LeadAction::AskUser { rationale, .. }
            | LeadAction::Finish { rationale, .. }
            | LeadAction::Commit { rationale }
            | LeadAction::Push { rationale }
            | LeadAction::CreatePr { rationale, .. }
            | LeadAction::Publish { rationale, .. } => rationale,
        }
    }
}

/// 剥 markdown ``` 围栏（本地副本·复刻 lead_draft::strip_code_fences·避免跨模块耦合）。
fn strip_code_fences(s: &str) -> String {
    s.lines()
        .filter(|line| !line.trim_start().starts_with("```"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// 解析 lead one-shot 输出 → LeadAction（围栏：剥 ``` → JSON → schema → 语义校验）。
#[allow(dead_code)] // Plan 2 lead_step 接线
pub fn parse_lead_action(final_text: &str) -> Result<LeadAction, LeadActionParseError> {
    let cleaned = strip_code_fences(final_text);
    let cleaned = cleaned.trim();
    let value: serde_json::Value =
        serde_json::from_str(cleaned).map_err(|e| LeadActionParseError::NotJson(e.to_string()))?;
    let action: LeadAction = serde_json::from_value(value)
        .map_err(|e| LeadActionParseError::SchemaMismatch(e.to_string()))?;
    validate_lead_action(&action)?;
    Ok(action)
}

/// 语义校验（只挡会让下游崩的空字段·逐变体校验）。
fn validate_lead_action(a: &LeadAction) -> Result<(), LeadActionParseError> {
    let bad = |m: String| Err(LeadActionParseError::SemanticInvalid(m));
    // rationale 是可审计契约（进 decision_ledger）→ 所有动作强制非空。
    if a.rationale().trim().is_empty() {
        return bad("rationale 为空（所有动作须给一句理由·进 decision_ledger 可审计）".into());
    }
    match a {
        LeadAction::DispatchWorker { task, .. } if task.trim().is_empty() => {
            bad("dispatch_worker 的 task 为空".into())
        }
        LeadAction::ProposeVerifier { cmd, .. } if cmd.trim().is_empty() => {
            bad("propose_verifier 的 cmd 为空".into())
        }
        LeadAction::AskUser { question, .. } if question.trim().is_empty() => {
            bad("ask_user 的 question 为空".into())
        }
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lead_action_serde_roundtrip_all_variants() {
        let cases = vec![
            LeadAction::Reply {
                rationale: "用户在问问题·直接回".into(),
            },
            LeadAction::DispatchWorker {
                rationale: "要改 README".into(),
                task: "把今天的 AI 新闻写进 README".into(),
                scope_files: vec!["README.md".into()],
                agent_hint: None,
                goal_title: None,
            },
            LeadAction::ProposeVerifier {
                rationale: "跑测试".into(),
                cmd: "npm test".into(),
            },
            LeadAction::AskUser {
                rationale: "动代码前确认".into(),
                question: "我打算让 worker 改 README·行吗".into(),
                options: vec!["行".into(), "先放着".into()],
                recommended: Some("行".into()),
            },
            LeadAction::Finish {
                rationale: "都做完了".into(),
                evidence_refs: vec!["artifact:a1".into()],
            },
            LeadAction::Commit {
                rationale: "落地本目标改动".into(),
            },
            LeadAction::Push {
                rationale: "推到远端".into(),
            },
            LeadAction::CreatePr {
                rationale: "开 PR".into(),
                title: Some("加登录校验".into()),
                body: Some("本 PR 加登录校验".into()),
            },
            LeadAction::Publish {
                rationale: "发布到 GitHub".into(),
                repo_name: Some("foo".into()),
                private: Some(true),
            },
        ];
        for a in cases {
            let json = serde_json::to_string(&a).unwrap();
            if matches!(a, LeadAction::DispatchWorker { .. }) {
                let value: serde_json::Value = serde_json::from_str(&json).unwrap();
                let object = value.as_object().expect("dispatch_worker 应序列化为对象");
                assert_eq!(
                    object.len(),
                    6,
                    "dispatch_worker JSON 只应包含 action/rationale/task/scope_files/agent_hint/goal_title: {json}"
                );
            }
            let back: LeadAction = serde_json::from_str(&json).unwrap();
            assert_eq!(a, back, "round-trip 不一致：{json}");
        }
        let reply_json = serde_json::to_string(&LeadAction::Reply {
            rationale: "x".into(),
        })
        .unwrap();
        assert!(
            reply_json.contains(r#""action":"reply""#),
            "tag 应为 action=reply: {reply_json}"
        );
        assert!(
            reply_json.contains(r#""rationale":"x""#),
            "rationale 应平铺到顶层: {reply_json}"
        );
    }

    #[test]
    fn parse_lead_action_valid_all_five_kinds() {
        assert!(matches!(
            parse_lead_action(r#"{"action":"reply","rationale":"答一下"}"#).unwrap(),
            LeadAction::Reply { .. }
        ));
        match parse_lead_action(
            r#"{"action":"dispatch_worker","rationale":"改码","task":"改 README","scope_files":["README.md"]}"#,
        )
        .unwrap()
        {
            LeadAction::DispatchWorker {
                task,
                scope_files,
                ..
            } => {
                assert_eq!(task, "改 README");
                assert_eq!(scope_files, vec!["README.md".to_string()]);
            }
            _ => panic!("应解析为 DispatchWorker"),
        }
        match parse_lead_action(
            r#"{"action":"propose_verifier","rationale":"跑测试","cmd":"npm test"}"#,
        )
        .unwrap()
        {
            LeadAction::ProposeVerifier { cmd, .. } => assert_eq!(cmd, "npm test"),
            _ => panic!("应解析为 ProposeVerifier"),
        }
        match parse_lead_action(
            r#"{"action":"ask_user","rationale":"动码确认","question":"改 README 行吗","recommended":"行"}"#,
        )
        .unwrap()
        {
            LeadAction::AskUser {
                question,
                recommended,
                ..
            } => {
                assert_eq!(question, "改 README 行吗");
                assert_eq!(recommended.as_deref(), Some("行"));
            }
            _ => panic!("应解析为 AskUser"),
        }
        match parse_lead_action(
            r#"{"action":"finish","rationale":"完事","evidence_refs":["artifact:a1"]}"#,
        )
        .unwrap()
        {
            LeadAction::Finish { evidence_refs, .. } => {
                assert_eq!(evidence_refs, vec!["artifact:a1".to_string()])
            }
            _ => panic!("应解析为 Finish"),
        }
    }

    #[test]
    fn parse_lead_action_parses_delivery_actions() {
        match parse_lead_action(r#"{"action":"commit","rationale":"落地"}"#).unwrap() {
            LeadAction::Commit { rationale } => assert_eq!(rationale, "落地"),
            _ => panic!("应解析为 Commit"),
        }
        match parse_lead_action(r#"{"action":"push","rationale":"推"}"#).unwrap() {
            LeadAction::Push { rationale } => assert_eq!(rationale, "推"),
            _ => panic!("应解析为 Push"),
        }
        match parse_lead_action(r#"{"action":"create_pr","rationale":"开 PR","title":"x"}"#)
            .unwrap()
        {
            LeadAction::CreatePr { title, body, .. } => {
                assert_eq!(title.as_deref(), Some("x"));
                assert_eq!(body, None, "不带 body 应默认 None");
            }
            _ => panic!("应解析为 CreatePr"),
        }
        match parse_lead_action(
            r#"{"action":"publish","rationale":"发布","repo_name":"foo","private":true}"#,
        )
        .unwrap()
        {
            LeadAction::Publish {
                repo_name, private, ..
            } => {
                assert_eq!(repo_name.as_deref(), Some("foo"));
                assert_eq!(private, Some(true));
            }
            _ => panic!("应解析为 Publish"),
        }
    }

    #[test]
    fn parse_lead_action_rejects_create_pr_empty_rationale() {
        assert!(matches!(
            parse_lead_action(r#"{"action":"create_pr","rationale":"  "}"#),
            Err(LeadActionParseError::SemanticInvalid(_))
        ));
    }

    #[test]
    fn retry_hint_lists_delivery_actions() {
        let hint = LeadActionParseError::SchemaMismatch("x".into()).retry_hint();
        assert!(hint.contains("commit"), "hint 应列 commit: {hint}");
        assert!(hint.contains("push"), "hint 应列 push: {hint}");
        assert!(hint.contains("create_pr"), "hint 应列 create_pr: {hint}");
        assert!(hint.contains("publish"), "hint 应列 publish: {hint}");
    }

    #[test]
    fn parse_lead_action_strips_markdown_fences() {
        let fenced = "```json\n{\"action\":\"reply\",\"rationale\":\"x\"}\n```";
        assert!(matches!(
            parse_lead_action(fenced).unwrap(),
            LeadAction::Reply { .. }
        ));
    }

    #[test]
    fn parse_lead_action_classifies_errors() {
        assert!(matches!(
            parse_lead_action("这不是 json"),
            Err(LeadActionParseError::NotJson(_))
        ));
        assert!(matches!(
            parse_lead_action(r#"{"foo":"bar"}"#),
            Err(LeadActionParseError::SchemaMismatch(_))
        ));
        assert!(matches!(
            parse_lead_action(r#"{"action":"bogus","rationale":"x"}"#),
            Err(LeadActionParseError::SchemaMismatch(_))
        ));
        assert!(matches!(
            parse_lead_action(r#"{"action":"dispatch_worker","rationale":"x","task":"  "}"#),
            Err(LeadActionParseError::SemanticInvalid(_))
        ));
        assert!(matches!(
            parse_lead_action(r#"{"action":"propose_verifier","rationale":"x","cmd":""}"#),
            Err(LeadActionParseError::SemanticInvalid(_))
        ));
        assert!(matches!(
            parse_lead_action(r#"{"action":"ask_user","rationale":"x","question":""}"#),
            Err(LeadActionParseError::SemanticInvalid(_))
        ));
    }

    #[test]
    fn parse_lead_action_requires_nonempty_rationale() {
        assert!(matches!(
            parse_lead_action(r#"{"action":"reply"}"#),
            Err(LeadActionParseError::SemanticInvalid(_))
        ));
        assert!(matches!(
            parse_lead_action(r#"{"action":"reply","rationale":"   "}"#),
            Err(LeadActionParseError::SemanticInvalid(_))
        ));
    }

    #[test]
    fn parse_error_retry_hint_is_actionable() {
        let hint = parse_lead_action(r#"{"action":"dispatch_worker","rationale":"x","task":""}"#)
            .unwrap_err()
            .retry_hint();
        assert!(hint.contains("task"), "hint 应点明问题字段 task: {hint}");
        assert!(!hint.trim().is_empty());
        let schema_hint = parse_lead_action(r#"{"action":"bogus","rationale":"x"}"#)
            .unwrap_err()
            .retry_hint();
        assert!(
            schema_hint.contains("reply") && schema_hint.contains("dispatch_worker"),
            "hint 应列合法 action: {schema_hint}"
        );
    }

    #[test]
    fn parse_dispatch_worker_with_goal_title() {
        match parse_lead_action(
            r#"{"action":"dispatch_worker","rationale":"加登录","task":"t","scope_files":["a.ts"],"goal_title":"加登录校验"}"#,
        )
        .unwrap()
        {
            LeadAction::DispatchWorker { goal_title, .. } => {
                assert_eq!(goal_title.as_deref(), Some("加登录校验"));
            }
            _ => panic!("应解析为 DispatchWorker"),
        }
    }

    #[test]
    fn parse_dispatch_worker_without_goal_title_defaults_none() {
        match parse_lead_action(
            r#"{"action":"dispatch_worker","rationale":"改码","task":"t","scope_files":["a.ts"]}"#,
        )
        .unwrap()
        {
            LeadAction::DispatchWorker { goal_title, .. } => {
                assert_eq!(goal_title, None, "不带 goal_title 的旧 JSON 应默认 None");
            }
            _ => panic!("应解析为 DispatchWorker"),
        }
    }
}

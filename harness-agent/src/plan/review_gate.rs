//! 确定性评审闸：计划进执行前的体检（spec §3.1）。弱 LLM 拆得烂的兜底。

use std::collections::{HashMap, HashSet};

use crate::goal::Verifier;
use crate::plan::contract::{AcceptanceKind, PlanTask};
use crate::plan::paths::{normalize_scope_path, path_contains, paths_overlap};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReviewVerdict {
    Ok,
    Bounce { reasons: Vec<String> },
}

/// 逐任务 + 跨任务确定性检查。返回所有不合格原因（空 = 通过）。
pub fn review_worklist(tasks: &[PlanTask]) -> ReviewVerdict {
    let mut reasons = Vec::new();

    // B1：空 worklist。
    if tasks.is_empty() {
        return ReviewVerdict::Bounce {
            reasons: vec!["worklist 为空：目标没拆出任何原子任务".to_string()],
        };
    }

    // B2：id 非空 + 全局唯一。
    let mut seen: HashSet<&str> = HashSet::new();
    for t in tasks {
        if t.id.trim().is_empty() {
            reasons.push("有任务 id 为空".to_string());
        } else if !t
            .id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        {
            reasons.push(format!(
                "task id 含非法字符（只许字母/数字/_/-·防 child run 路径越界）：{}",
                t.id
            ));
        } else if !seen.insert(t.id.as_str()) {
            reasons.push(format!("task id 重复：{}", t.id));
        }
    }

    for t in tasks {
        if t.intent.trim().is_empty() {
            reasons.push(format!("task {}: intent 为空", t.id));
        }
        if t.max_turns == 0 {
            reasons.push(format!("task {}: max_turns 必须 > 0", t.id));
        }
        if t.files_scope.is_empty() {
            reasons.push(format!("task {}: files_scope 不能为空", t.id));
        }
        for p in &t.files_scope {
            if let Err(e) = normalize_scope_path(p) {
                reasons.push(format!("task {}: files_scope {}", t.id, e));
            }
        }
        for p in &t.forbidden_scope {
            if let Err(e) = normalize_scope_path(p) {
                reasons.push(format!("task {}: forbidden_scope {}", t.id, e));
            }
        }
        // 两道验收：行为道必有；change-required 结构道(按名鞭子)必有。
        // 各道都需可跑、已批、非空；只读由运行时 baseline observer 判 PolicyFailure（非 review 阶段静态猜·R9）。
        check_lane(t, "acceptance", &t.acceptance, &mut reasons);
        match (&t.artifact_check, t.acceptance_kind) {
            (Some(art), _) => check_lane(t, "artifact_check", art, &mut reasons),
            (None, AcceptanceKind::ChangeRequired) => reasons.push(format!(
                "task {}: change-required 任务必须带 artifact_check（fail-to-pass 结构检查·驱动 + 没干探针）",
                t.id
            )),
            (None, AcceptanceKind::Invariant) => {}
        }
        // 红线吞白名单（前缀感知）：每个 files_scope 都被某条 forbidden 覆盖。
        let norm_forbidden: Vec<String> = t
            .forbidden_scope
            .iter()
            .filter_map(|p| normalize_scope_path(p).ok())
            .collect();
        let norm_files: Vec<String> = t
            .files_scope
            .iter()
            .filter_map(|p| normalize_scope_path(p).ok())
            .collect();
        if !norm_files.is_empty()
            && norm_files
                .iter()
                .all(|f| norm_forbidden.iter().any(|fb| path_contains(fb, f)))
        {
            reasons.push(format!(
                "task {}: forbidden_scope 把 files_scope 整个吞了",
                t.id
            ));
        }
    }

    let ids: HashSet<&str> = tasks.iter().map(|t| t.id.as_str()).collect();
    for t in tasks {
        for d in &t.depends_on {
            if !ids.contains(d.as_str()) {
                reasons.push(format!("task {}: depends_on 悬挂引用 '{}'", t.id, d));
            }
        }
    }

    if has_cycle(tasks) {
        reasons.push("worklist depends_on 成环（含自依赖）".to_string());
    }

    // 无依赖路径的任务写文件不许重叠（漏标依赖探测器·F5·前缀感知）。
    let reach = reachability(tasks);
    for (i, a) in tasks.iter().enumerate() {
        let a_norm: Vec<String> = a
            .files_scope
            .iter()
            .filter_map(|p| normalize_scope_path(p).ok())
            .collect();
        for b in tasks.iter().skip(i + 1) {
            let linked = reach.contains(&(a.id.clone(), b.id.clone()))
                || reach.contains(&(b.id.clone(), a.id.clone()));
            if linked {
                continue;
            }
            let b_norm: Vec<String> = b
                .files_scope
                .iter()
                .filter_map(|p| normalize_scope_path(p).ok())
                .collect();
            if a_norm
                .iter()
                .any(|x| b_norm.iter().any(|y| paths_overlap(x, y)))
            {
                reasons.push(format!(
                    "task {} 与 {} 无依赖关系却写重叠路径（漏标依赖或真冲突·加依赖排序）",
                    a.id, b.id
                ));
            }
        }
    }

    if reasons.is_empty() {
        ReviewVerdict::Ok
    } else {
        ReviewVerdict::Bounce { reasons }
    }
}

fn check_lane(t: &PlanTask, lane: &str, c: &crate::goal::Criterion, reasons: &mut Vec<String>) {
    match &c.verifier {
        Verifier::Verifiable { check_cmd, .. } => {
            if !c.is_executable_verifiable() {
                reasons.push(format!("task {}: {lane} 未批准", t.id));
            }
            if check_cmd.trim().is_empty() {
                reasons.push(format!("task {}: {lane} check_cmd 为空", t.id));
            }
        }
        Verifier::Judgmental { .. } => {
            reasons.push(format!(
                "task {}: {lane} 须是可跑的 Verifiable，不接受 Judgmental",
                t.id
            ));
        }
    }
}

fn adjacency(tasks: &[PlanTask]) -> HashMap<&str, Vec<&str>> {
    let mut adj: HashMap<&str, Vec<&str>> = HashMap::new();
    for t in tasks {
        adj.entry(t.id.as_str())
            .or_default()
            .extend(t.depends_on.iter().map(|s| s.as_str()));
    }
    adj
}

fn has_cycle(tasks: &[PlanTask]) -> bool {
    fn dfs<'a>(
        node: &'a str,
        adj: &HashMap<&'a str, Vec<&'a str>>,
        visited: &mut HashSet<&'a str>,
        on_stack: &mut HashSet<&'a str>,
    ) -> bool {
        if on_stack.contains(node) {
            return true;
        }
        if !visited.insert(node) {
            return false;
        }
        on_stack.insert(node);
        if let Some(deps) = adj.get(node) {
            for d in deps {
                if dfs(d, adj, visited, on_stack) {
                    return true;
                }
            }
        }
        on_stack.remove(node);
        false
    }

    let adj = adjacency(tasks);
    let mut visited = HashSet::new();
    let mut on_stack = HashSet::new();
    tasks
        .iter()
        .any(|t| dfs(t.id.as_str(), &adj, &mut visited, &mut on_stack))
}

/// (a,b) ∈ 集合 ⟺ a 经 depends_on 传递到达 b。
fn reachability(tasks: &[PlanTask]) -> HashSet<(String, String)> {
    let adj = adjacency(tasks);
    let mut out = HashSet::new();
    for t in tasks {
        let mut stack = vec![t.id.as_str()];
        let mut seen: HashSet<&str> = HashSet::new();
        while let Some(n) = stack.pop() {
            if let Some(deps) = adj.get(n) {
                for d in deps {
                    if seen.insert(d) {
                        out.insert((t.id.clone(), d.to_string()));
                        stack.push(d);
                    }
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plan::contract::{parse_worklist, PlanTask};

    fn tasks(json: &str) -> Vec<PlanTask> {
        parse_worklist(json).unwrap()
    }

    fn task_with_lanes(id: &str, behavior: &str, artifact: Option<&str>) -> PlanTask {
        let json = match artifact {
            Some(a) => format!(
                r#"{{ "tasks": [ {{ "id": "{id}", "intent": "x", "files_scope": ["src"],
                  "acceptance_cmd": "{behavior}", "artifact_check_cmd": "{a}", "max_turns": 5 }} ] }}"#
            ),
            None => format!(
                r#"{{ "tasks": [ {{ "id": "{id}", "intent": "x", "files_scope": ["src"],
                  "acceptance_cmd": "{behavior}", "max_turns": 5 }} ] }}"#
            ),
        };
        parse_worklist(&json).unwrap().into_iter().next().unwrap()
    }

    fn bounce_reasons(v: ReviewVerdict) -> Vec<String> {
        match v {
            ReviewVerdict::Bounce { reasons } => reasons,
            ReviewVerdict::Ok => panic!("expected Bounce"),
        }
    }

    #[test]
    fn change_required_without_artifact_is_bounced() {
        let t = vec![task_with_lanes("t1", "cargo test x", None)];
        let r = bounce_reasons(review_worklist(&t));
        assert!(
            r.iter().any(|s| s.contains("artifact")),
            "缺 artifact 必须 bounce: {r:?}"
        );
    }

    #[test]
    fn change_required_with_both_lanes_is_ok() {
        let t = vec![task_with_lanes(
            "t1",
            "cargo test x",
            Some("grep -rq foo src"),
        )];
        assert_eq!(review_worklist(&t), ReviewVerdict::Ok);
    }

    #[test]
    fn artifact_lane_that_writes_is_not_statically_bounced() {
        let t = vec![task_with_lanes("t1", "cargo test x", Some("echo x > f"))];
        assert_eq!(review_worklist(&t), ReviewVerdict::Ok);
    }

    #[test]
    fn invariant_task_without_artifact_is_ok() {
        let json = r#"{ "tasks": [ { "id": "t1", "intent": "x", "files_scope": ["src"],
          "acceptance_cmd": "cargo test x", "max_turns": 5, "acceptance_kind": "invariant" } ] }"#;
        let t = parse_worklist(json).unwrap();
        assert_eq!(review_worklist(&t), ReviewVerdict::Ok);
    }

    #[test]
    fn clean_worklist_passes() {
        let t = tasks(
            r#"{ "tasks": [
          { "id": "t1", "intent": "a", "files_scope": ["a.rs"], "acceptance_cmd": "cargo test", "artifact_check_cmd": "true", "max_turns": 5 },
          { "id": "t2", "intent": "b", "files_scope": ["b.rs"], "acceptance_cmd": "cargo test", "artifact_check_cmd": "true", "max_turns": 5, "depends_on": ["t1"] }
        ] }"#,
        );
        assert_eq!(review_worklist(&t), ReviewVerdict::Ok);
    }

    #[test]
    fn empty_worklist_bounces() {
        assert!(matches!(review_worklist(&[]), ReviewVerdict::Bounce { .. }));
    }

    #[test]
    fn duplicate_id_bounces_with_reason() {
        let t = tasks(
            r#"{ "tasks": [
          { "id": "t1", "intent": "a", "files_scope": ["a.rs"], "acceptance_cmd": "cargo test", "max_turns": 5 },
          { "id": "t1", "intent": "b", "files_scope": ["b.rs"], "acceptance_cmd": "cargo test", "max_turns": 5 }
        ] }"#,
        );
        let r = bounce_reasons(review_worklist(&t));
        assert!(r.iter().any(|s| s.contains("t1") && s.contains("重复")));
    }

    #[test]
    fn empty_files_scope_bounces() {
        let t = tasks(
            r#"{ "tasks": [ { "id": "t1", "intent": "a", "files_scope": [], "acceptance_cmd": "cargo test", "max_turns": 5 } ] }"#,
        );
        assert!(matches!(review_worklist(&t), ReviewVerdict::Bounce { .. }));
    }

    #[test]
    fn glob_or_absolute_or_parent_path_bounces() {
        for fs in [r#"["src/*.rs"]"#, r#"["/etc/x"]"#, r#"["../x"]"#] {
            let t = tasks(&format!(
                r#"{{ "tasks": [ {{ "id": "t1", "intent": "a", "files_scope": {fs}, "acceptance_cmd": "cargo test", "max_turns": 5 }} ] }}"#
            ));
            assert!(
                matches!(review_worklist(&t), ReviewVerdict::Bounce { .. }),
                "fs={fs}"
            );
        }
    }

    #[test]
    fn zero_max_turns_bounces() {
        let t = tasks(
            r#"{ "tasks": [ { "id": "t1", "intent": "a", "files_scope": ["a.rs"], "acceptance_cmd": "cargo test", "max_turns": 0 } ] }"#,
        );
        assert!(matches!(review_worklist(&t), ReviewVerdict::Bounce { .. }));
    }

    #[test]
    fn empty_acceptance_cmd_bounces() {
        let t = tasks(
            r#"{ "tasks": [ { "id": "t1", "intent": "a", "files_scope": ["a.rs"], "acceptance_cmd": "", "max_turns": 5 } ] }"#,
        );
        assert!(matches!(review_worklist(&t), ReviewVerdict::Bounce { .. }));
    }

    #[test]
    fn acceptance_cmd_mutation_guess_no_longer_bounces_review() {
        for cmd in ["grep 'Vec<T>' src/lib.rs", "echo x > a.rs", "touch a.rs"] {
            let t = tasks(&format!(
                r#"{{ "tasks": [ {{ "id": "t1", "intent": "a", "files_scope": ["a.rs"], "acceptance_cmd": "{cmd}", "artifact_check_cmd": "true", "max_turns": 5 }} ] }}"#
            ));
            assert_eq!(
                review_worklist(&t),
                ReviewVerdict::Ok,
                "cmd={cmd} 必须交给运行时 read-only observer，而不是评审阶段猜测 bounce"
            );
        }
    }

    #[test]
    fn dangling_depends_on_bounces() {
        let t = tasks(
            r#"{ "tasks": [ { "id": "t1", "intent": "a", "files_scope": ["a.rs"], "acceptance_cmd": "cargo test", "max_turns": 5, "depends_on": ["nope"] } ] }"#,
        );
        assert!(matches!(review_worklist(&t), ReviewVerdict::Bounce { .. }));
    }

    #[test]
    fn dependency_cycle_and_self_dep_bounce() {
        let cyc = tasks(
            r#"{ "tasks": [
          { "id": "t1", "intent": "a", "files_scope": ["a.rs"], "acceptance_cmd": "cargo test", "max_turns": 5, "depends_on": ["t2"] },
          { "id": "t2", "intent": "b", "files_scope": ["b.rs"], "acceptance_cmd": "cargo test", "max_turns": 5, "depends_on": ["t1"] }
        ] }"#,
        );
        assert!(matches!(
            review_worklist(&cyc),
            ReviewVerdict::Bounce { .. }
        ));
        let self_dep = tasks(
            r#"{ "tasks": [ { "id": "t1", "intent": "a", "files_scope": ["a.rs"], "acceptance_cmd": "cargo test", "max_turns": 5, "depends_on": ["t1"] } ] }"#,
        );
        assert!(matches!(
            review_worklist(&self_dep),
            ReviewVerdict::Bounce { .. }
        ));
    }

    #[test]
    fn forbidden_swallows_files_scope_bounces() {
        let t = tasks(
            r#"{ "tasks": [ { "id": "t1", "intent": "a", "files_scope": ["src/a.rs"], "forbidden_scope": ["src"], "acceptance_cmd": "cargo test", "max_turns": 5 } ] }"#,
        );
        assert!(matches!(review_worklist(&t), ReviewVerdict::Bounce { .. }));
    }

    #[test]
    fn gate_hardening_forbidden_child_inside_files_scope_does_not_swallow_allowlist() {
        let t = tasks(
            r#"{ "tasks": [ { "id": "t1", "intent": "a", "files_scope": ["src"], "forbidden_scope": ["src/secret.rs"], "acceptance_cmd": "true", "artifact_check_cmd": "true", "max_turns": 5 } ] }"#,
        );
        assert_eq!(review_worklist(&t), ReviewVerdict::Ok);
    }

    #[test]
    fn gate_hardening_reserved_scope_path_bounces() {
        for fs in [r#"[".git/config"]"#, r#"[".myagenthubs/runs/x"]"#] {
            let t = tasks(&format!(
                r#"{{ "tasks": [ {{ "id": "t1", "intent": "a", "files_scope": {fs}, "acceptance_cmd": "true", "max_turns": 5 }} ] }}"#
            ));
            let r = bounce_reasons(review_worklist(&t));
            assert!(
                r.iter().any(|s| s.contains("保留路径段")),
                "fs={fs} 应被保留路径闸打回: {r:?}"
            );
        }
    }

    #[test]
    fn independent_tasks_prefix_overlap_bounces() {
        let t = tasks(
            r#"{ "tasks": [
          { "id": "t1", "intent": "a", "files_scope": ["src"], "acceptance_cmd": "cargo test", "max_turns": 5 },
          { "id": "t2", "intent": "b", "files_scope": ["src/lib.rs"], "acceptance_cmd": "cargo test", "max_turns": 5 }
        ] }"#,
        );
        assert!(matches!(review_worklist(&t), ReviewVerdict::Bounce { .. }));
    }

    #[test]
    fn dependent_tasks_overlap_ok() {
        let t = tasks(
            r#"{ "tasks": [
          { "id": "t1", "intent": "a", "files_scope": ["a.rs"], "acceptance_cmd": "cargo test", "artifact_check_cmd": "true", "max_turns": 5 },
          { "id": "t2", "intent": "b", "files_scope": ["a.rs"], "acceptance_cmd": "cargo test", "artifact_check_cmd": "true", "max_turns": 5, "depends_on": ["t1"] }
        ] }"#,
        );
        assert_eq!(review_worklist(&t), ReviewVerdict::Ok);
    }

    #[test]
    fn unsafe_task_id_bounces() {
        for bad in ["../x", "a/b", ".."] {
            let t = tasks(&format!(
                r#"{{ "tasks": [ {{ "id": "{bad}", "intent": "a", "files_scope": ["a.rs"], "acceptance_cmd": "cargo test", "max_turns": 5 }} ] }}"#
            ));
            let r = bounce_reasons(review_worklist(&t));
            assert!(
                r.iter().any(|s| s.contains("非法字符") && s.contains(bad)),
                "id={bad:?} 应被打回"
            );
        }
    }

    #[test]
    fn safe_task_id_passes_charset() {
        let t = tasks(
            r#"{ "tasks": [ { "id": "fix-retry_2", "intent": "a", "files_scope": ["a.rs"], "acceptance_cmd": "cargo test", "artifact_check_cmd": "true", "max_turns": 5 } ] }"#,
        );
        assert_eq!(review_worklist(&t), ReviewVerdict::Ok);
    }
}

#![cfg(test)]

#[path = "source_scanner.rs"]
mod source_scanner;

#[test]
fn invoke_registry_has_no_keep_or_discard_commands() {
    let source = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/lib.rs"));
    let production = source.split("\n#[cfg(test)]\nmod tests {").next().unwrap();
    let registry = production
        .split(".invoke_handler(tauri::generate_handler![")
        .nth(1)
        .expect("invoke handler registry should exist")
        .split("])")
        .next()
        .unwrap();

    for command in ["session_keep", "session_discard"] {
        assert!(
            !registry
                .lines()
                .any(|line| line.trim().trim_end_matches(',') == command),
            "removed command must not be registered: {command}"
        );
        assert!(
            !production.contains(&format!("fn {command}(")),
            "removed command must not have a backend definition: {command}"
        );
    }
}

#[test]
fn app_sources_have_no_dangerous_user_git_write_primitives() {
    let src_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    fn collect_rust_sources(dir: &std::path::Path, files: &mut Vec<std::path::PathBuf>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                collect_rust_sources(&path, files);
            } else if path.extension().and_then(|ext| ext.to_str()) == Some("rs") {
                files.push(path);
            }
        }
    }

    let forbidden = [
        "[\"add\", \"-A\"]",
        "[\"reset\", \"--hard\"",
        "[\"clean\", \"-ffdx\"",
        "auto_commit_run",
        "discard_reset",
        "ensure_local_project_repo",
    ];
    let mut source_files = Vec::new();
    collect_rust_sources(&src_dir, &mut source_files);

    for path in source_files {
        let source = std::fs::read_to_string(&path).unwrap();
        // External test modules are still unit tests, not production sources.
        let production = if source
            .lines()
            .map(str::trim)
            .find(|line| !line.is_empty() && !line.starts_with("//"))
            == Some("#![cfg(test)]")
        {
            ""
        } else {
            source.split("\n#[cfg(test)]\nmod tests {").next().unwrap()
        };
        for pattern in forbidden {
            assert!(
                !production.contains(pattern),
                "{} contains forbidden app-side git write primitive {pattern}",
                path.display()
            );
        }
    }
}

#[test]
fn production_processes_use_shared_command_helper() {
    const TEST_MODULE_MARKER: &str = "\n#[cfg(test)]\nmod tests {";
    let src_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut source_files = Vec::new();

    fn collect_rust_sources(dir: &std::path::Path, files: &mut Vec<std::path::PathBuf>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                collect_rust_sources(&path, files);
            } else if path.extension().and_then(|ext| ext.to_str()) == Some("rs") {
                files.push(path);
            }
        }
    }

    collect_rust_sources(&src_dir, &mut source_files);
    for path in source_files {
        // proc.rs is the one production allowlist entry: it owns the raw constructor and
        // applies CREATE_NO_WINDOW on Windows. Test modules may construct local fixtures.
        if path.file_name().and_then(|name| name.to_str()) == Some("proc.rs") {
            continue;
        }
        let source = std::fs::read_to_string(&path).unwrap();
        // Match the explicit file-level cfg, never a directory/name convention.
        let production = if source
            .lines()
            .map(str::trim)
            .find(|line| !line.is_empty() && !line.starts_with("//"))
            == Some("#![cfg(test)]")
        {
            ""
        } else {
            source.split(TEST_MODULE_MARKER).next().unwrap()
        };
        // 只跳过整行注释；若按 `//` 截断，`"http://x"; Command::new(...)` 会藏掉真实调用。
        let production_without_pure_comments = production
            .lines()
            .filter(|line| !line.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            !production_without_pure_comments.contains("Command::new"),
            "{} bypasses the shared process command helper",
            path.display()
        );
    }
}

#[test]
fn production_sources_do_not_invoke_forbidden_user_git_writes() {
    source_scanner::assert_boundaries();
    let src_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    assert_no_forbidden_git_writes(&source_scanner::production_sources(&src_dir));
}

fn assert_no_forbidden_git_writes(sources: &[source_scanner::Source]) {
    // The six reviewed exceptions keep their exact fragments/reasons. Each is now
    // bound to its owning item, with one declaration AND one fragment required
    // across the entire recursive inventory, even when the owner moves modules.
    fn remove_allowlisted_once(
        sources: &[source_scanner::Source],
        compacts: &mut [String],
        kind: &str,
        name: &str,
        fragment: &str,
        reason: &str,
    ) {
        let item = source_scanner::unique_item(sources, kind, name);
        let production = &sources[item.file].production;
        let owner = production[item.range.clone()]
            .chars()
            .filter(|ch| !ch.is_whitespace())
            .collect::<String>();
        let total_hits = sources
            .iter()
            .map(|source| {
                source
                    .production
                    .chars()
                    .filter(|ch| !ch.is_whitespace())
                    .collect::<String>()
                    .match_indices(fragment)
                    .count()
            })
            .sum::<usize>();
        assert_eq!(
            total_hits, 1,
            "allowlisted git-write exception must occur exactly once in all sources: {reason}"
        );
        assert_eq!(
            owner.match_indices(fragment).count(),
            1,
            "allowlisted git-write exception changed in {kind} {name}: {reason}"
        );
        let compact = &mut compacts[item.file];
        // Mask, rather than delete, so offsets stay valid for other entries in this item.
        let start = production[..item.range.start]
            .chars()
            .filter(|ch| !ch.is_whitespace())
            .map(char::len_utf8)
            .sum::<usize>()
            + owner.find(fragment).unwrap();
        assert_eq!(&compact[start..start + fragment.len()], fragment);
        compact.replace_range(start..start + fragment.len(), &" ".repeat(fragment.len()));
    }

    let forbidden = [
        ("[\"add\",\"-A\"", "git add -A"),
        (",\"add\",\"-A\"", "prefixed git add -A"),
        ("[\"add\",\"--all\"", "git add --all"),
        (",\"add\",\"--all\"", "prefixed git add --all"),
        (".arg(\"add\").arg(\"-A\")", "chained git add -A"),
        (".arg(\"add\").arg(\"--all\")", "chained git add --all"),
        (".arg(\"add\").args([\"-A\"", "split git add -A"),
        (".arg(\"add\").args([\"--all\"", "split git add --all"),
        ("[\"commit\"", "git commit"),
        (",\"commit\"", "prefixed git commit"),
        (".arg(\"commit\")", "chained git commit"),
        ("[\"reset\",\"--hard\"", "git reset --hard"),
        (",\"reset\",\"--hard\"", "prefixed git reset --hard"),
        (
            ".arg(\"reset\").arg(\"--hard\")",
            "chained git reset --hard",
        ),
        (".arg(\"reset\").args([\"--hard\"", "split git reset --hard"),
        ("[\"clean\",\"-f", "git clean with force"),
        (",\"clean\",\"-f", "prefixed git clean with force"),
        (".arg(\"clean\").arg(\"-f", "chained git clean with force"),
        (".arg(\"clean\").args([\"-f", "split git clean with force"),
        ("[\"init\"", "git init"),
        (",\"init\"", "prefixed git init"),
        (".arg(\"init\")", "chained git init"),
    ];

    let guarded_forbidden = [
        ("[\"reset\"", "git reset"),
        (".arg(\"reset\")", "chained git reset"),
        ("[\"rm\"", "git rm"),
        (".arg(\"rm\")", "chained git rm"),
        ("[\"mv\"", "git mv"),
        (".arg(\"mv\")", "chained git mv"),
        ("[\"restore\"", "git restore"),
        (".arg(\"restore\")", "chained git restore"),
        ("[\"switch\"", "git switch"),
        (".arg(\"switch\")", "chained git switch"),
        ("[\"rebase\"", "git rebase"),
        (".arg(\"rebase\")", "chained git rebase"),
        ("[\"cherry-pick\"", "git cherry-pick"),
        (".arg(\"cherry-pick\")", "chained git cherry-pick"),
        ("[\"apply\"", "git apply"),
        (".arg(\"apply\")", "chained git apply"),
        ("[\"remote\",\"add\"", "git remote add"),
        (",\"remote\",\"add\"", "prefixed git remote add"),
        ("[\"remote\",\"remove\"", "git remote remove"),
        (",\"remote\",\"remove\"", "prefixed git remote remove"),
        ("[\"remote\",\"rm\"", "git remote rm"),
        (",\"remote\",\"rm\"", "prefixed git remote rm"),
        ("[\"remote\",\"rename\"", "git remote rename"),
        (",\"remote\",\"rename\"", "prefixed git remote rename"),
        ("[\"remote\",\"set-url\"", "git remote set-url"),
        (",\"remote\",\"set-url\"", "prefixed git remote set-url"),
        ("[\"remote\",\"prune\"", "git remote prune"),
        (",\"remote\",\"prune\"", "prefixed git remote prune"),
        ("[\"remote\",\"update\"", "git remote update"),
        (",\"remote\",\"update\"", "prefixed git remote update"),
        ("[\"config\"", "git config"),
        (".arg(\"config\")", "chained git config"),
        ("[\"config\",\"--local\"", "git config --local"),
        (",\"config\",\"--local\"", "prefixed git config --local"),
        ("[\"config\",\"--global\"", "git config --global"),
        (",\"config\",\"--global\"", "prefixed git config --global"),
        ("[\"config\",\"--system\"", "git config --system"),
        (",\"config\",\"--system\"", "prefixed git config --system"),
        ("[\"merge\"", "git merge"),
        (",\"merge\"", "prefixed git merge"),
        (".arg(\"merge\")", "chained git merge"),
        ("[\"worktree\",\"add\"", "git worktree add"),
        (",\"worktree\",\"add\"", "prefixed git worktree add"),
        ("[\"worktree\",\"remove\"", "git worktree remove"),
        (",\"worktree\",\"remove\"", "prefixed git worktree remove"),
        ("[\"worktree\",\"prune\"", "git worktree prune"),
        (",\"worktree\",\"prune\"", "prefixed git worktree prune"),
        ("[\"branch\",\"-D\"", "git branch -D"),
        (",\"branch\",\"-D\"", "prefixed git branch -D"),
        ("[\"update-ref\"", "git update-ref"),
        (",\"update-ref\"", "prefixed git update-ref"),
        (".arg(\"update-ref\")", "chained git update-ref"),
        ("[\"fetch\"", "git fetch"),
        (",\"fetch\"", "prefixed git fetch"),
        (".arg(\"fetch\")", "chained git fetch"),
        ("[\"gc\"", "git gc"),
        (",\"gc\"", "prefixed git gc"),
        (".arg(\"gc\")", "chained git gc"),
        ("[\"update-index\"", "git update-index"),
        (",\"update-index\"", "prefixed git update-index"),
        (".arg(\"update-index\")", "chained git update-index"),
        ("[\"stash\"", "git stash"),
        (",\"stash\"", "prefixed git stash"),
        (".arg(\"stash\")", "chained git stash"),
        ("[\"checkout\"", "git checkout"),
        (",\"checkout\"", "prefixed git checkout"),
        (".arg(\"checkout\")", "chained git checkout"),
    ];
    let confirmation_guarded_forbidden = [
        ("[\"push\"", "git push"),
        (",\"push\"", "prefixed git push"),
        (".arg(\"push\")", "chained git push"),
    ];

    fn assert_guarded_occurrences(
        file: &str,
        source: &str,
        compact: &str,
        compact_to_source: &[usize],
        pattern: &str,
        operation: &str,
        guard_markers: &[&str],
        guard_description: &str,
        constant_item: Option<&std::ops::Range<usize>>,
    ) {
        let mut search_from = 0;
        while let Some(relative) = compact[search_from..].find(pattern) {
            let compact_index = search_from + relative;
            let source_index = compact_to_source[compact_index];
            let function_start = constant_item
                .filter(|item| item.contains(&source_index))
                .map(|item| item.start)
                .or_else(|| {
                    source[..source_index]
                        .rmatch_indices('\n')
                        .map(|(index, _)| index + 1)
                        .chain(std::iter::once(0))
                        .find(|line_start| {
                            let line_end = source[*line_start..]
                                .find('\n')
                                .map(|offset| *line_start + offset)
                                .unwrap_or(source.len());
                            let line = source[*line_start..line_end].trim_start();
                            !line.starts_with("//")
                                && !line.starts_with("/*")
                                && line.contains("fn ")
                        })
                })
                .unwrap_or_else(|| panic!("{file} invokes {operation} outside a function"));
            let prefix = &source[function_start..source_index];
            assert!(
                guard_markers.iter().any(|marker| prefix.contains(marker)),
                "{file} contains {operation} without {guard_description} in its function"
            );
            search_from = compact_index + pattern.len();
        }
    }

    let mut compacts = sources
        .iter()
        .map(|source| {
            source
                .production
                .chars()
                .filter(|ch| !ch.is_whitespace())
                .collect::<String>()
        })
        .collect::<Vec<_>>();

    // App-owned ~/.agentloom/local/default bootstrap, not a user-selected repo.
    remove_allowlisted_once(
        sources,
        &mut compacts,
        "fn",
        "ensure_local_namespace_and_default_repo",
        ".arg(\"init\").arg(\"-q\").current_dir(local_path)",
        "initialize the app-owned local-default scaffold",
    );
    remove_allowlisted_once(
            sources,
            &mut compacts,
            "const",
            "LEAD_MCP_TOOL_NAMES",
            "\"propose_verifier\",\"commit\",\"push\",\"create_pr\",\"publish\",];",
            "MCP commit/交付工具名（经 MCP 暴露给 agent 调用·真正提交/交付走受控 handler·此串本身不是 git 写调用）",
        );
    // T5-before scaffold: these initialize and seed an app-owned session repo,
    // never a user-selected project directory.
    remove_allowlisted_once(
        sources,
        &mut compacts,
        "fn",
        "ensure_worktree_for_default_in",
        "\"commit.gpgsign=false\",\"init\",\"-q\"",
        "initialize an app-managed default session repo",
    );
    remove_allowlisted_once(
        sources,
        &mut compacts,
        "fn",
        "ensure_worktree_for_default_in",
        "\"user.name=AgentLoom\",\"commit\",\"--allow-empty\"",
        "seed the app-managed default session repo",
    );
    remove_allowlisted_once(
            sources,
            &mut compacts,
            "fn",
            "build_add_argv",
            "[SANDBOXED_UPDATE_INDEX_SUBCOMMAND,\"--add\",\"--remove\",\"--\"].into_iter().map(std::ffi::OsString::from).collect::<Vec<_>>()",
            "sandboxed mediated commit: the one sanctioned app-side staging of the agent's user-project work, run in the no-child-process seatbelt cage via run_sandboxed_git_commit; user approved, and the guard still forbids all other app-side user git writes",
        );
    remove_allowlisted_once(
            sources,
            &mut compacts,
            "fn",
            "build_commit_argv",
            "[\"-c\",author_name_config.as_str(),\"-c\",author_email_config.as_str(),\"commit\",\"--only\",\"--no-gpg-sign\",\"-m\",].into_iter().map(std::ffi::OsString::from).collect::<Vec<_>>()",
            "sandboxed mediated commit: the one sanctioned app-side commit of the agent's user-project work, run in the no-child-process seatbelt cage via run_sandboxed_git_commit; user approved, and the guard still forbids all other app-side user git writes",
        );

    let tool_names = source_scanner::unique_item(sources, "const", "LEAD_MCP_TOOL_NAMES");
    for (index, (source, compact)) in sources.iter().zip(compacts).enumerate() {
        let file = source.path.to_str().expect("UTF-8 source path");
        let production = source.production.as_str();
        let compact_guarded = production
            .chars()
            .filter(|ch| !ch.is_whitespace())
            .collect::<String>();
        let compact_to_source = production
            .char_indices()
            .flat_map(|(index, ch)| {
                (!ch.is_whitespace())
                    .then_some(std::iter::repeat(index).take(ch.len_utf8()))
                    .into_iter()
                    .flatten()
            })
            .collect::<Vec<_>>();

        for (pattern, operation) in forbidden {
            assert!(
                !compact.contains(pattern),
                "{file} contains forbidden app-side user git write: {operation} ({pattern})"
            );
        }
        for (pattern, operation) in guarded_forbidden {
            assert_guarded_occurrences(
                file,
                production,
                &compact_guarded,
                &compact_to_source,
                pattern,
                operation,
                &["assert_app_domain_path(", "is_app_domain_path("],
                "an app-domain assertion",
                None,
            );
        }
        for (pattern, operation) in confirmation_guarded_forbidden {
            assert_guarded_occurrences(
                file,
                production,
                &compact_guarded,
                &compact_to_source,
                pattern,
                operation,
                &[
                    "require_explicit_confirmation(",
                    "const LEAD_MCP_TOOL_NAMES",
                ],
                "an explicit-confirmation guard",
                (index == tool_names.file).then_some(&tool_names.range),
            );
        }
    }
}

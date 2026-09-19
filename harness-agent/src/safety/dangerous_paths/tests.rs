#![cfg(test)]

use super::*;

fn dangerous_command_scan(command: &str, cwd: &Path, workspace: &Path) -> Option<DenyReason> {
    super::dangerous_command_scan(
        command,
        cwd,
        workspace,
        crate::fs_scope::FsReadScope::Workspace,
        &[],
    )
}
use std::path::Path;

#[test]
fn scan_fd_redirect_read_only_corpus() {
    let ws = Path::new("/ws/proj");
    let commands = [
        "cd sub && ls 2>/dev/null",
        "cd sub && head -60 README.md 2>/dev/null || head README*.md",
        "cd sub && cmd 2>&1 | tail",
        "wc -l a/*.md 2>/dev/null | tail -5",
        "grep -r foo . 2>/dev/null",
        "cd sub && git status 2>/dev/null",
        "cd sub && cmd >/dev/null 2>&1",
        "cd sub && exec 3>&1",
        "cmd &>/dev/null",
        "cmd &>>/dev/null",
        "cmd &>/dev/null &",
        "echo hi &",
        "( cd sub && ls )",
        "{ ls ; }",
    ];
    let failures: Vec<_> = commands
        .iter()
        .filter_map(|command| {
            dangerous_command_scan(command, ws, ws).map(|reason| (*command, reason.rule))
        })
        .collect();
    assert!(
        failures.is_empty(),
        "{} / {} read-only commands blocked: {failures:#?}",
        failures.len(),
        commands.len()
    );
}

#[test]
fn scan_fd_redirect_dangerous_corpus() {
    let ws = Path::new("/ws/proj");
    for (command, rule) in [
        ("cd sub && echo x > out.txt", "cd_then_mutation"),
        ("cd sub && cat a >> b", "cd_then_mutation"),
        ("cd sub && rm -rf x", "cd_then_mutation"),
        ("cd sub && cmd 2> err.log", "cd_then_mutation"),
        ("cd .. && ls > /tmp/x", "cd_then_mutation"),
        ("cat x > /etc/passwd", "outside_workspace"),
        ("cmd 2>/../../outside", "outside_workspace"),
    ] {
        let reason = dangerous_command_scan(command, ws, ws)
            .unwrap_or_else(|| panic!("dangerous command allowed: {command}"));
        assert_eq!(reason.rule, rule, "{command}");
    }
    for (command, rule) in [
        ("cd sub &>/dev/null rm -rf /etc", "cd_then_mutation"),
        ("echo hi &>/dev/null rm -rf /etc", "rm_system_path"),
        ("echo hi &>/dev/null tee /etc/passwd", "outside_workspace"),
        ("echo hi &>/dev/null cd .. && rm -rf x", "cd_then_mutation"),
        ("cd sub && cmd &> out.txt", "cd_then_mutation"),
        ("cmd &>.git/config", "dangerous_config_write"),
        ("( cd .. ; rm -rf x )", "cd_then_mutation"),
        ("{ cd .. ; rm -rf x ; }", "cd_then_mutation"),
        // bash 读法抓：`&>` 合并成重定向操作符·危险目标藏在同段后续位置参数里
        ("rm &>/dev/null /etc/passwd", "outside_workspace"),
        ("rm &> /dev/null /etc/passwd", "outside_workspace"),
        ("rm -rf &>/dev/null /etc", "rm_system_path"),
        ("rm -rf &>log /etc", "rm_system_path"),
        ("rm -rf &>/dev/null .git/config", "dangerous_config_write"),
        ("rm &>/dev/null ~/.zshrc", "unresolvable_target"),
        ("rm &>>/dev/null /etc/passwd", "outside_workspace"),
        ("cp a &>/dev/null /etc/evil", "outside_workspace"),
        ("tee &>/dev/null /etc/passwd", "outside_workspace"),
        ("dd &>/dev/null of=/etc/evil", "outside_workspace"),
        ("cat &>/dev/null /etc/passwd", "outside_workspace"),
    ] {
        let reason = dangerous_command_scan(command, ws, ws)
            .unwrap_or_else(|| panic!("dangerous command allowed: {command}"));
        assert_eq!(reason.rule, rule, "{command}");
    }
    assert!(dangerous_command_scan("echo x >| /etc/passwd", ws, ws).is_some());
}

#[test]
fn scan_fd_redirect_edge_cases_preserve_path_checks() {
    let ws = Path::new("/ws/proj");
    for command in [
        "cd sub && cmd 1>/dev/null 2>>/dev/null",
        "cd sub && cmd &>/dev/null",
        "cd sub && cmd &>>/dev/null",
        "cd sub && cmd >&2",
        "cd sub && cmd 2>&-",
        "cd sub && cmd 2>&1 >'/dev/null'",
        "cd sub && cmd 2>&1>/dev/null",
        "2>/dev/null head README.md",
        "cat < /dev/null",
    ] {
        assert!(
            dangerous_command_scan(command, ws, ws).is_none(),
            "{command}"
        );
    }
    for (command, rule) in [
        ("cd sub && cmd 2>&1 >out", "cd_then_mutation"),
        ("cd sub && cmd 2>1", "cd_then_mutation"),
        ("cd sub && cmd 2>1>/dev/null", "cd_then_mutation"),
        ("cd sub && cmd >1>/dev/null", "cd_then_mutation"),
        ("cd sub && cmd &>out", "cd_then_mutation"),
        ("cd sub && cmd &>>out", "cd_then_mutation"),
        ("cd sub && cmd >&out", "cd_then_mutation"),
        ("cd sub && 2>/dev/null rm -rf x", "cd_then_mutation"),
        ("2>/dev/null cd sub && cmd >out", "cd_then_mutation"),
        ("cd .. && cat x > /tmp/y", "cd_then_mutation"),
        ("cmd &>/etc/passwd", "outside_workspace"),
        ("cmd >&/etc/passwd", "outside_workspace"),
        ("cmd 2>>../outside", "outside_workspace"),
        ("cmd 2>/dev/null/../outside", "outside_workspace"),
        ("cmd 2>.git/config", "dangerous_config_write"),
        ("cmd 2>&$fd", "unresolvable_target"),
        ("cat /dev/null 2>/dev/null", "outside_workspace"),
        ("cat 2>/dev/null /dev/null", "outside_workspace"),
        ("rm /dev/null 2>/dev/null", "outside_workspace"),
        ("2>/dev/null cat /etc/passwd", "outside_workspace"),
        ("cat 2>&1 /etc/passwd", "outside_workspace"),
        ("cat < /etc/passwd", "outside_workspace"),
        ("cd sub && echo x >| out.txt", "cd_then_mutation"),
    ] {
        let reason = dangerous_command_scan(command, ws, ws)
            .unwrap_or_else(|| panic!("dangerous command allowed: {command}"));
        assert_eq!(reason.rule, rule, "{command}");
    }
}

#[test]
fn lexical_resolve_joins_relative_and_collapses_dotdot() {
    let cwd = Path::new("/ws/proj");
    assert_eq!(
        lexical_resolve("src/x.rs", cwd),
        Path::new("/ws/proj/src/x.rs")
    );
    assert_eq!(
        lexical_resolve("../../etc/passwd", cwd),
        Path::new("/etc/passwd")
    );
    assert_eq!(lexical_resolve("/etc/hosts", cwd), Path::new("/etc/hosts"));
    assert_eq!(lexical_resolve("./a/./b", cwd), Path::new("/ws/proj/a/b"));
}

#[test]
fn outside_workspace_detects_escape() {
    let ws = Path::new("/ws/proj");
    assert!(!is_outside_workspace(Path::new("/ws/proj/src/x.rs"), ws));
    assert!(is_outside_workspace(Path::new("/etc/passwd"), ws));
    assert!(is_outside_workspace(Path::new("/ws/other"), ws));
}

#[test]
fn dangerous_removal_paths_match_cc_list() {
    assert!(is_dangerous_removal_path(Path::new("/")));
    assert!(is_dangerous_removal_path(Path::new("/etc")));
    assert!(is_dangerous_removal_path(Path::new("/usr")));
    assert!(is_dangerous_removal_path(Path::new("/tmp/")));
    assert!(is_dangerous_removal_path(Path::new("*")));
    assert!(is_dangerous_removal_path(Path::new("/var/*")));
    assert!(!is_dangerous_removal_path(Path::new("/usr/local/bin")));
    assert!(!is_dangerous_removal_path(Path::new("/ws/proj/src")));
}

#[test]
fn dangerous_config_hits_files_and_dirs_case_insensitive() {
    assert!(path_hits_dangerous_config(Path::new(
        "/ws/proj/.git/config"
    )));
    assert!(path_hits_dangerous_config(Path::new("/ws/proj/.bashrc")));
    assert!(path_hits_dangerous_config(Path::new(
        "/ws/proj/.CLAUDE.json"
    )));
    assert!(path_hits_dangerous_config(Path::new(
        "/ws/proj/.claude/settings.json"
    )));
    assert!(path_hits_dangerous_config(Path::new("/ws/proj/.mcp.json")));
    assert!(!path_hits_dangerous_config(Path::new(
        "/ws/proj/.gitignore"
    )));
    assert!(!path_hits_dangerous_config(Path::new(
        "/ws/proj/.vscode/settings.json"
    )));
    assert!(!path_hits_dangerous_config(Path::new(
        "/ws/proj/.idea/x.iml"
    )));
    assert!(!path_hits_dangerous_config(Path::new(
        "/ws/proj/src/main.rs"
    )));
}

#[test]
fn config_target_resolves_relative_to_cwd() {
    let cwd = Path::new("/ws/proj/sub");
    assert!(is_dangerous_config_target("../.git/config", cwd));
    assert!(is_dangerous_config_target(".bashrc", cwd));
    assert!(!is_dangerous_config_target("notes.md", cwd));
}

use std::path::PathBuf;

fn ws() -> (PathBuf, PathBuf) {
    (PathBuf::from("/ws/proj"), PathBuf::from("/ws/proj"))
}

#[test]
fn scan_blocks_write_outside_workspace() {
    let (cwd, w) = ws();
    assert!(dangerous_command_scan("echo x > ../out.txt", &cwd, &w).is_some());
    assert!(dangerous_command_scan("rm /etc/hosts", &cwd, &w).is_some());
    assert!(dangerous_command_scan("mv src/a.rs /tmp/a.rs", &cwd, &w).is_some());
}

#[test]
fn scan_blocks_dangerous_removal_and_config() {
    let (cwd, w) = ws();
    assert_eq!(
        dangerous_command_scan("rm -rf /etc", &cwd, &w)
            .unwrap()
            .rule,
        "rm_system_path"
    );
    assert_eq!(
        dangerous_command_scan("rm -rf /", &cwd, &w).unwrap().rule,
        "rm_system_path"
    );
    assert_eq!(
        dangerous_command_scan("rm .git/config", &cwd, &w)
            .unwrap()
            .rule,
        "dangerous_config_write"
    );
    assert_eq!(
        dangerous_command_scan("echo x > .bashrc", &cwd, &w)
            .unwrap()
            .rule,
        "dangerous_config_write"
    );
}

#[test]
fn scan_blocks_proc_sub_cd_mutation_and_expansion() {
    let (cwd, w) = ws();
    assert_eq!(
        dangerous_command_scan("echo x > >(tee .git/config)", &cwd, &w)
            .unwrap()
            .rule,
        "process_substitution"
    );
    assert_eq!(
        dangerous_command_scan("cd .git && echo x > config", &cwd, &w)
            .unwrap()
            .rule,
        "cd_then_mutation"
    );
    assert_eq!(
        dangerous_command_scan("rm $HOME/x", &cwd, &w).unwrap().rule,
        "unresolvable_target"
    );
}

#[test]
fn scan_blocks_read_secret_outside_workspace() {
    let (cwd, w) = ws();
    assert!(dangerous_command_scan("cat /etc/passwd", &cwd, &w).is_some());
}

#[test]
fn scan_applies_scope_only_to_reads_and_never_to_writes() {
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().join("workspace");
    let venv = root.path().join("venv");
    let python = venv.join("bin/python3");
    let dependency = venv.join("lib/site-packages/foo.py");
    std::fs::create_dir(&workspace).unwrap();
    std::fs::create_dir_all(python.parent().unwrap()).unwrap();
    std::fs::create_dir_all(dependency.parent().unwrap()).unwrap();
    std::fs::write(venv.join("pyvenv.cfg"), "home = /usr\n").unwrap();
    std::fs::write(&python, "").unwrap();
    std::fs::write(&dependency, "x = 1\n").unwrap();
    let workspace = workspace.canonicalize().unwrap();

    let test_path = std::env::join_paths([python.parent().unwrap()]).unwrap();
    let roots =
        crate::fs_scope::discover_project_dependency_roots(Some(&test_path), None, None, &[]);
    let mk_ctx = |scope| super::ScanCtx {
        cwd: workspace.as_path(),
        workspace: workspace.as_path(),
        fs_read_scope: scope,
        dependency_roots: &roots,
        extra_read_roots: &[],
    };
    assert!(super::dangerous_command_scan_with_roots(
        &format!("cat {}", dependency.display()),
        &mk_ctx(crate::fs_scope::FsReadScope::Workspace),
    )
    .is_some());
    assert!(super::dangerous_command_scan_with_roots(
        &format!("cat {}", dependency.display()),
        &mk_ctx(crate::fs_scope::FsReadScope::ProjectDeps),
    )
    .is_none());
    for command in [
        format!("tee {}", dependency.display()),
        format!("rm {}", dependency.display()),
    ] {
        for scope in [
            crate::fs_scope::FsReadScope::Workspace,
            crate::fs_scope::FsReadScope::ProjectDeps,
            crate::fs_scope::FsReadScope::Wide,
        ] {
            assert!(super::dangerous_command_scan_with_roots(&command, &mk_ctx(scope)).is_some());
        }
    }
}

#[test]
#[serial_test::serial]
fn expanded_scope_still_blocks_tilde_credentials() {
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().join("workspace");
    let home = root.path().join("home");
    std::fs::create_dir(&workspace).unwrap();
    std::fs::create_dir_all(home.join(".ssh")).unwrap();
    std::fs::write(home.join(".ssh/id_rsa"), "secret").unwrap();
    let workspace = workspace.canonicalize().unwrap();
    let old_home = std::env::var_os("HOME");
    std::env::set_var("HOME", &home);

    assert!(super::dangerous_command_scan(
        "cat ~/.ssh/id_rsa",
        &workspace,
        &workspace,
        crate::fs_scope::FsReadScope::Wide,
        &[],
    )
    .is_some());
    // Workspace keeps the historical dynamic/tilde-read scan behavior.
    assert!(super::dangerous_command_scan(
        "cat ~/.ssh/id_rsa",
        &workspace,
        &workspace,
        crate::fs_scope::FsReadScope::Workspace,
        &[],
    )
    .is_none());

    match old_home {
        Some(value) => std::env::set_var("HOME", value),
        None => std::env::remove_var("HOME"),
    }
}

#[test]
fn scan_allows_normal_in_workspace_commands() {
    let (cwd, w) = ws();
    assert!(dangerous_command_scan("rm src/x.rs", &cwd, &w).is_none());
    assert!(dangerous_command_scan("grep -r foo .", &cwd, &w).is_none());
    assert!(dangerous_command_scan("echo x > out.txt", &cwd, &w).is_none());
    assert!(dangerous_command_scan("cargo build", &cwd, &w).is_none());
    assert!(dangerous_command_scan("cat src/main.rs", &cwd, &w).is_none());
}

#[test]
fn scan_is_honest_does_not_block_interpreters() {
    // 诚实 gap（设计 §二·用户拍）：解释器/eval/xargs 不在防护内·不拦。别删这条。
    let (cwd, w) = ws();
    assert!(
        dangerous_command_scan("python -c \"import os; os.remove('/etc/x')\"", &cwd, &w).is_none()
    );
    assert!(dangerous_command_scan("xargs rm < list.txt", &cwd, &w).is_none());
    assert!(dangerous_command_scan("eval \"$DANGER\"", &cwd, &w).is_none());
}

#[test]
fn scan_fail_closed_on_unparseable_write() {
    let (cwd, w) = ws();
    assert!(dangerous_command_scan("rm 'unterminated", &cwd, &w).is_some());
}

#[test]
fn scan_recurses_into_sh_c_payload() {
    let (cwd, w) = ws();
    assert!(dangerous_command_scan("sh -c 'rm /etc/hosts'", &cwd, &w).is_some());
    assert!(dangerous_command_scan("bash -c \"rm .git/config\"", &cwd, &w).is_some());
}

#[test]
fn scan_fixups_tilde_dd_and_redirect_branches() {
    let (cwd, w) = ws();
    // Bug 1：裸 ~ / ~/ 写删必须挡（HOME·致命 footgun）
    assert!(dangerous_command_scan("rm -rf ~", &cwd, &w).is_some());
    assert!(dangerous_command_scan("echo x > ~/out", &cwd, &w).is_some());
    assert!(dangerous_command_scan("rm -rf ~/Documents", &cwd, &w).is_some());
    // Bug 2：dd of=PATH 写工作区外必须挡
    assert!(dangerous_command_scan("dd of=/etc/passwd", &cwd, &w).is_some());
    assert!(dangerous_command_scan("dd if=/dev/zero of=../escape", &cwd, &w).is_some());
    // dd 的非文件操作数（bs=/count=）不该误拦
    assert!(dangerous_command_scan("dd if=in.bin of=out.bin bs=4096 count=10", &cwd, &w).is_none());
    // 补的分支覆盖：>> append 出界 / cp 目标出界 / /dev/null 放行
    assert!(dangerous_command_scan("echo x >> ../out.txt", &cwd, &w).is_some());
    assert!(dangerous_command_scan("cp src/a.rs /tmp/b.rs", &cwd, &w).is_some());
    assert!(dangerous_command_scan("echo x > /dev/null", &cwd, &w).is_none());
}

#[test]
fn scan_fixups_shell_cluster_and_touch_mkdir() {
    let (cwd, w) = ws();
    // P1：组合 flag 簇 -lc / -ec 也要穿透 payload
    assert!(dangerous_command_scan("bash -lc 'rm /etc/hosts'", &cwd, &w).is_some());
    assert!(dangerous_command_scan("sh -ec 'rm .git/config'", &cwd, &w).is_some());
    // 单独 -c 仍穿透（不回归）
    assert!(dangerous_command_scan("bash -c 'rm /etc/hosts'", &cwd, &w).is_some());
    // P2：touch/mkdir 出界写要挡
    assert!(dangerous_command_scan("touch /etc/x", &cwd, &w).is_some());
    assert!(dangerous_command_scan("mkdir /etc/foo", &cwd, &w).is_some());
    assert!(dangerous_command_scan("touch ~/x", &cwd, &w).is_some());
    // 工作区内 touch/mkdir 仍放行
    assert!(dangerous_command_scan("touch out.txt", &cwd, &w).is_none());
    assert!(dangerous_command_scan("mkdir -p src/new", &cwd, &w).is_none());
}

// --- --read-root / extra_read_roots wiring through the shell footgun scanner ---

fn setup_extra_root() -> (std::path::PathBuf, std::path::PathBuf, tempfile::TempDir) {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join("workspace")).unwrap();
    std::fs::create_dir(root.path().join("pasted")).unwrap();
    std::fs::write(root.path().join("pasted/a.png"), b"x").unwrap();
    let workspace = root.path().join("workspace").canonicalize().unwrap();
    let extra = root.path().join("pasted").canonicalize().unwrap();
    (workspace, extra, root)
}

#[test]
fn extra_read_root_allows_shell_cat_of_extra_dir() {
    let (workspace, extra, _root) = setup_extra_root();
    let extra_roots = vec![extra.clone()];
    let command = format!("cat {}", extra.join("a.png").to_string_lossy());

    assert!(super::dangerous_command_scan(
        &command,
        &workspace,
        &workspace,
        crate::fs_scope::FsReadScope::Workspace,
        &extra_roots,
    )
    .is_none());
}

#[test]
fn extra_read_root_allows_shell_ls_of_extra_dir() {
    let (workspace, extra, _root) = setup_extra_root();
    let extra_roots = vec![extra.clone()];
    let command = format!("ls {}", extra.to_string_lossy());

    assert!(super::dangerous_command_scan(
        &command,
        &workspace,
        &workspace,
        crate::fs_scope::FsReadScope::Workspace,
        &extra_roots,
    )
    .is_none());
}

#[test]
fn extra_read_root_still_denies_write_into_extra_dir() {
    let (workspace, extra, _root) = setup_extra_root();
    let extra_roots = vec![extra.clone()];
    let command = format!("echo x > {}", extra.join("out.txt").to_string_lossy());

    let reason = super::dangerous_command_scan(
        &command,
        &workspace,
        &workspace,
        crate::fs_scope::FsReadScope::Workspace,
        &extra_roots,
    );
    assert!(
        reason.is_some(),
        "write into extra read root must stay denied"
    );
}

#[test]
fn extra_read_root_still_denies_rm_into_extra_dir() {
    let (workspace, extra, _root) = setup_extra_root();
    let extra_roots = vec![extra.clone()];
    let command = format!("rm {}", extra.join("a.png").to_string_lossy());

    let reason = super::dangerous_command_scan(
        &command,
        &workspace,
        &workspace,
        crate::fs_scope::FsReadScope::Workspace,
        &extra_roots,
    );
    assert!(reason.is_some(), "rm into extra read root must stay denied");
}

#[test]
fn extra_read_root_without_it_denies_same_read() {
    let (workspace, extra, _root) = setup_extra_root();
    let command = format!("cat {}", extra.join("a.png").to_string_lossy());

    let reason = super::dangerous_command_scan(
        &command,
        &workspace,
        &workspace,
        crate::fs_scope::FsReadScope::Workspace,
        &[],
    );
    assert!(
        reason.is_some(),
        "without --read-root the same read must remain blocked (behavior unchanged)"
    );
}

#[test]
fn extra_read_root_two_roots_both_readable_via_shell() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join("workspace")).unwrap();
    std::fs::create_dir(root.path().join("a")).unwrap();
    std::fs::create_dir(root.path().join("b")).unwrap();
    std::fs::write(root.path().join("a/x.txt"), b"x").unwrap();
    std::fs::write(root.path().join("b/y.txt"), b"y").unwrap();
    let workspace = root.path().join("workspace").canonicalize().unwrap();
    let extra_roots =
        crate::fs_scope::resolve_read_roots(&[root.path().join("a"), root.path().join("b")])
            .unwrap();

    for file in ["a/x.txt", "b/y.txt"] {
        let command = format!("cat {}", root.path().join(file).to_string_lossy());
        assert!(
            super::dangerous_command_scan(
                &command,
                &workspace,
                &workspace,
                crate::fs_scope::FsReadScope::Workspace,
                &extra_roots,
            )
            .is_none(),
            "{file}"
        );
    }
}

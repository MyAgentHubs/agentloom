#![cfg(test)]

use super::*;
use serial_test::serial;

fn make_candidate(id: &str) -> Lesson {
    Lesson {
        id: id.to_string(),
        status: LessonStatus::Candidate,
        source: LessonSource::AutoError,
        created: "t".to_string(),
        last_confirmed: "t".to_string(),
        last_used: None,
        evidence_runs: vec!["run-1".to_string()],
        tags: vec!["build".to_string()],
        observed_commands: vec!["cmd-1".to_string()],
        episode_ref: Some("win-0123456789abcdef".to_string()),
        body: "## 问题特征\ncargo build fails with E0463\n## 修复·做法\nRun `rustup update` before retrying.\n## 适用条件·边界\nRust toolchain drift in local workspace.\n".to_string(),
    }
}

#[test]
fn run_args_parse_network_off() {
    use clap::Parser;
    let cli = Cli::try_parse_from(["myagent", "run", "hi", "--network", "off"]).unwrap();
    match cli.command {
        Some(Command::Run(args)) => assert_eq!(args.network, crate::goal::NetworkPolicy::Off),
        _ => panic!("expected run"),
    }
}

#[test]
fn run_args_network_defaults_on() {
    use clap::Parser;
    let cli = Cli::try_parse_from(["myagent", "run", "hi"]).unwrap();
    match cli.command {
        Some(Command::Run(args)) => assert_eq!(args.network, crate::goal::NetworkPolicy::On),
        _ => panic!("expected run"),
    }
}

#[test]
fn fs_read_scope_defaults_to_workspace_for_all_cli_entrypoints() {
    use clap::Parser;

    let cli = Cli::try_parse_from(["myagent"]).unwrap();
    assert_eq!(
        cli.interactive.fs_read_scope,
        crate::fs_scope::FsReadScope::Workspace
    );

    for argv in [
        vec!["myagent", "run", "hi"],
        vec!["myagent", "plan", "build"],
        vec!["myagent", "resume", "run-1"],
    ] {
        let cli = Cli::try_parse_from(argv).unwrap();
        let scope = match cli.command.unwrap() {
            Command::Run(args) => args.fs_read_scope,
            Command::Plan(args) => args.fs_read_scope,
            Command::Resume(args) => args.fs_read_scope,
            other => panic!("unexpected command: {other:?}"),
        };
        assert_eq!(scope, crate::fs_scope::FsReadScope::Workspace);
    }
}

#[test]
fn run_args_parse_explicit_fs_read_scope() {
    use clap::Parser;

    let cli =
        Cli::try_parse_from(["myagent", "run", "hi", "--fs-read-scope", "project-deps"]).unwrap();
    match cli.command {
        Some(Command::Run(args)) => {
            assert_eq!(
                args.fs_read_scope,
                crate::fs_scope::FsReadScope::ProjectDeps
            );
        }
        other => panic!("expected run, got {other:?}"),
    }
}

#[test]
fn fs_write_fence_defaults_off_for_all_cli_entrypoints() {
    use clap::Parser;

    let cli = Cli::try_parse_from(["myagent"]).unwrap();
    assert_eq!(
        cli.interactive.fs_write_fence,
        crate::exec::sandbox::FsWriteFence::Off
    );

    for argv in [
        vec!["myagent", "run", "hi"],
        vec!["myagent", "plan", "build"],
        vec!["myagent", "resume", "run-1"],
    ] {
        let cli = Cli::try_parse_from(argv).unwrap();
        let fence = match cli.command.unwrap() {
            Command::Run(args) => args.fs_write_fence,
            Command::Plan(args) => args.fs_write_fence,
            Command::Resume(args) => args.fs_write_fence,
            other => panic!("unexpected command: {other:?}"),
        };
        assert_eq!(fence, crate::exec::sandbox::FsWriteFence::Off);
    }
}

#[test]
fn run_args_parse_explicit_fs_write_fence() {
    use clap::Parser;

    let cli = Cli::try_parse_from(["myagent", "run", "hi", "--fs-write-fence", "on"]).unwrap();
    match cli.command {
        Some(Command::Run(args)) => {
            assert_eq!(args.fs_write_fence, crate::exec::sandbox::FsWriteFence::On);
        }
        other => panic!("expected run, got {other:?}"),
    }
}

#[test]
fn max_turn_defaults_apply_to_all_cli_entrypoints() {
    use clap::Parser;

    let cli = Cli::try_parse_from(["myagent"]).unwrap();
    assert_eq!(
        cli.interactive.max_turns,
        crate::orchestrator::MIN_TASK_TURN_BUDGET
    );

    let cli = Cli::try_parse_from(["myagent", "run", "hi"]).unwrap();
    match cli.command {
        Some(Command::Run(args)) => {
            assert_eq!(args.max_turns, crate::orchestrator::MIN_TASK_TURN_BUDGET);
        }
        other => panic!("expected run, got {other:?}"),
    }

    let cli = Cli::try_parse_from(["myagent", "plan", "build"]).unwrap();
    match cli.command {
        Some(Command::Plan(args)) => {
            assert_eq!(args.max_turns, crate::orchestrator::MIN_TASK_TURN_BUDGET);
        }
        other => panic!("expected plan, got {other:?}"),
    }

    let cli = Cli::try_parse_from(["myagent", "resume", "run-1"]).unwrap();
    match cli.command {
        Some(Command::Resume(args)) => {
            assert_eq!(args.max_turns, crate::orchestrator::MIN_TASK_TURN_BUDGET);
        }
        other => panic!("expected resume, got {other:?}"),
    }
}

#[test]
fn plan_subcommand_parses_objective_and_knobs() {
    use clap::Parser;
    let cli = Cli::try_parse_from([
        "myagent",
        "plan",
        "build the thing",
        "--provider",
        "mock",
        "--max-review-attempts",
        "4",
        "--max-plan-steps",
        "30",
        "--max-replan-rounds",
        "7",
        "--max-turns",
        "6",
        "--criteria",
        "cmd: cargo test",
        "--resume",
    ])
    .unwrap();
    match cli.command {
        Some(Command::Plan(args)) => {
            assert_eq!(args.input, "build the thing");
            assert_eq!(args.provider, "mock");
            assert_eq!(args.max_review_attempts, 4);
            assert_eq!(args.max_plan_steps, 30);
            assert_eq!(args.max_replan_rounds, 7);
            assert_eq!(args.max_turns, 6);
            assert_eq!(args.criteria, vec!["cmd: cargo test".to_string()]);
            assert!(args.resume);
        }
        _ => panic!("expected plan"),
    }
}

#[test]
fn plan_subcommand_defaults_max_replan_rounds_to_three() {
    use clap::Parser;
    let cli = Cli::try_parse_from(["myagent", "plan", "build"]).unwrap();
    match cli.command {
        Some(Command::Plan(args)) => assert_eq!(args.max_replan_rounds, 3),
        _ => panic!("expected plan"),
    }
}

#[test]
fn plan_subcommand_defaults_preflight_gate_on() {
    use clap::Parser;
    let cli = Cli::try_parse_from(["myagent", "plan", "do the thing"]).unwrap();
    match cli.command {
        Some(Command::Plan(args)) => {
            assert!(matches!(args.preflight_gate, PreflightGate::On));
        }
        other => panic!("expected plan, got {other:?}"),
    }
}

#[test]
fn plan_subcommand_parses_preflight_gate_off() {
    use clap::Parser;
    let cli = Cli::try_parse_from(["myagent", "plan", "do the thing", "--preflight-gate", "off"])
        .unwrap();
    match cli.command {
        Some(Command::Plan(args)) => {
            assert!(matches!(args.preflight_gate, PreflightGate::Off));
        }
        other => panic!("expected plan, got {other:?}"),
    }
}

#[test]
fn verify_and_watchdog_defaults_apply_to_all_cli_entrypoints() {
    use clap::Parser;

    let cli = Cli::try_parse_from(["myagent"]).unwrap();
    assert_eq!(
        cli.interactive.verify_every,
        crate::orchestrator::DEFAULT_VERIFY_EVERY
    );
    assert_eq!(
        cli.interactive.watchdog_repeat,
        crate::orchestrator::DEFAULT_WATCHDOG_REPEAT
    );

    let cli = Cli::try_parse_from(["myagent", "run", "hi"]).unwrap();
    match cli.command {
        Some(Command::Run(args)) => {
            assert_eq!(args.verify_every, crate::orchestrator::DEFAULT_VERIFY_EVERY);
            assert_eq!(
                args.watchdog_repeat,
                crate::orchestrator::DEFAULT_WATCHDOG_REPEAT
            );
        }
        _ => panic!("expected run"),
    }

    let cli = Cli::try_parse_from(["myagent", "resume", "run-1"]).unwrap();
    match cli.command {
        Some(Command::Resume(args)) => {
            assert_eq!(args.verify_every, crate::orchestrator::DEFAULT_VERIFY_EVERY);
            assert_eq!(
                args.watchdog_repeat,
                crate::orchestrator::DEFAULT_WATCHDOG_REPEAT
            );
        }
        _ => panic!("expected resume"),
    }
}

#[test]
fn resume_args_parse_realign_flags() {
    use clap::Parser;

    let cli = Cli::try_parse_from([
        "myagent",
        "resume",
        "run-1",
        "--realign-objective",
        " ship smaller slice ",
        "--realign-criteria",
        "cmd: cargo test",
        "--realign-scope",
        " harness-agent ",
        "--realign-constraint",
        " no UI work ",
        "--realign-reason",
        "stuck repeating",
    ])
    .unwrap();

    let Some(Command::Resume(args)) = cli.command else {
        panic!("expected resume");
    };
    let input = resume_realign_input(&args).unwrap().expect("realign input");
    assert_eq!(input.objective.as_deref(), Some(" ship smaller slice "));
    assert_eq!(input.add_criteria.len(), 1);
    assert_eq!(input.add_criteria[0].id, "c1");
    assert_eq!(input.scope.as_deref(), Some(" harness-agent "));
    assert_eq!(input.add_constraints, vec![" no UI work "]);
    assert_eq!(input.reason, "stuck repeating");
}

#[test]
fn resume_realign_reason_alone_is_noop() {
    use clap::Parser;

    let cli = Cli::try_parse_from([
        "myagent",
        "resume",
        "run-1",
        "--realign-reason",
        "only a reason",
    ])
    .unwrap();

    let Some(Command::Resume(args)) = cli.command else {
        panic!("expected resume");
    };
    assert!(resume_realign_input(&args).unwrap().is_none());
}

#[test]
fn run_args_parse_learn_flags() {
    use clap::Parser;
    let cli = Cli::try_parse_from(["myagent", "run", "do x", "--learn"]).unwrap();
    match cli.command {
        Some(Command::Run(args)) => assert!(args.learn && !args.auto_learn),
        _ => panic!("expected run"),
    }

    let cli = Cli::try_parse_from(["myagent", "run", "do x", "--auto-learn"]).unwrap();
    match cli.command {
        Some(Command::Run(args)) => assert!(args.auto_learn),
        _ => panic!("expected run"),
    }
}

#[test]
fn parses_config_search_subcommand() {
    use clap::Parser;
    let cli = Cli::try_parse_from([
        "myagent",
        "config",
        "search",
        "--backend",
        "brave",
        "--api-key",
        "k",
    ])
    .unwrap();
    match cli.command {
        Some(Command::Config {
            command: ConfigCommand::Search(ConfigSearchArgs { backend, api_key }),
        }) => {
            assert_eq!(backend, "brave");
            assert_eq!(api_key, "k");
        }
        _ => panic!("expected config search"),
    }
}

#[test]
#[serial]
fn config_command_search_exa_writes_exa_config() {
    let tmp = tempfile::tempdir().unwrap();
    std::env::set_var("MYAGENT_HOME", tmp.path());
    config_command(ConfigCommand::Search(ConfigSearchArgs {
        backend: "exa".into(),
        api_key: "k".into(),
    }))
    .unwrap();
    let loaded = crate::config::load_config().unwrap().search;
    assert!(matches!(loaded, Some(crate::config::SearchConfig::Exa { api_key }) if api_key == "k"));
    std::env::remove_var("MYAGENT_HOME");
}

#[test]
#[serial]
fn config_command_search_brave_unchanged_and_unknown_not_written() {
    let tmp = tempfile::tempdir().unwrap();
    std::env::set_var("MYAGENT_HOME", tmp.path());
    config_command(ConfigCommand::Search(ConfigSearchArgs {
        backend: "brave".into(),
        api_key: "bk".into(),
    }))
    .unwrap();
    assert!(matches!(
        crate::config::load_config().unwrap().search,
        Some(crate::config::SearchConfig::Brave { api_key }) if api_key == "bk"
    ));
    config_command(ConfigCommand::Search(ConfigSearchArgs {
        backend: "bogus".into(),
        api_key: "x".into(),
    }))
    .unwrap();
    assert!(matches!(
        crate::config::load_config().unwrap().search,
        Some(crate::config::SearchConfig::Brave { .. })
    ));
    std::env::remove_var("MYAGENT_HOME");
}

#[test]
#[serial]
fn memory_remember_writes_active() {
    let tmp = tempfile::tempdir().unwrap();
    let ws = tempfile::tempdir().unwrap();
    std::env::set_var("MYAGENT_HOME", tmp.path());

    run_memory_remember(ws.path(), "cargo E0463 用 rustup update", &["build".into()]).unwrap();

    let store = MemoryStore::for_workspace(ws.path()).unwrap();
    let active = store.list_active().unwrap();
    assert_eq!(active.len(), 1);
    assert_eq!(active[0].source, LessonSource::UserTaught);
    assert_eq!(active[0].status, LessonStatus::Active);

    let log = std::fs::read_to_string(store.root().join("log.md")).unwrap();
    assert!(log.contains("user_taught"));

    std::env::remove_var("MYAGENT_HOME");
}

#[test]
#[serial]
fn accept_promotes_and_respects_cap() {
    let home = tempfile::tempdir().unwrap();
    std::env::set_var("MYAGENT_HOME", home.path());
    let ws = tempfile::tempdir().unwrap();
    let store = MemoryStore::for_workspace(ws.path()).unwrap();
    store.init().unwrap();

    store.write_lesson(&make_candidate("lesson-c1")).unwrap();
    accept_candidate(ws.path(), "lesson-c1").unwrap();
    assert_eq!(
        store.read_lesson("lesson-c1").unwrap().status,
        LessonStatus::Active
    );
    assert!(store.read_index().unwrap().contains("lesson-c1"));

    for i in 0..49 {
        let mut active = make_candidate(&format!("lesson-a{i}"));
        active.status = LessonStatus::Active;
        store.write_lesson(&active).unwrap();
    }
    assert_eq!(store.list_active().unwrap().len(), 50);
    store.write_lesson(&make_candidate("lesson-over")).unwrap();
    assert!(accept_candidate(ws.path(), "lesson-over").is_err());

    std::env::remove_var("MYAGENT_HOME");
}

#[test]
#[serial]
fn reject_archives_not_in_index() {
    let home = tempfile::tempdir().unwrap();
    std::env::set_var("MYAGENT_HOME", home.path());
    let ws = tempfile::tempdir().unwrap();
    let store = MemoryStore::for_workspace(ws.path()).unwrap();
    store.init().unwrap();

    store.write_lesson(&make_candidate("lesson-r1")).unwrap();
    reject_candidate(ws.path(), "lesson-r1").unwrap();
    assert_eq!(
        store.read_lesson("lesson-r1").unwrap().status,
        LessonStatus::Archived
    );
    assert!(!store.read_index().unwrap().contains("lesson-r1"));

    std::env::remove_var("MYAGENT_HOME");
}

#[test]
fn elevate_permission_raises_to_allow_only_when_always_used() {
    use crate::shell::PermissionPolicy::*;
    assert_eq!(elevate_permission(Ask, true), Allow);
    assert_eq!(elevate_permission(Ask, false), Ask);
    assert_eq!(elevate_permission(Deny, true), Allow);
    assert_eq!(elevate_permission(Allow, false), Allow);
}

mod image_tests;
mod mcp_args_tests;

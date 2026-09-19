//! T1 图片附件：CLI `--image` 参数解析回归测试。从 `cli.rs` 的 `mod tests` 拆出
//! （避免该文件继续超出文件大小门禁的基线历史额度）。经 `mod` 挂在其 `mod tests`
//! 下，`use super::*` 沿用父模块（`tests`）已导入的名字。
use super::*;

#[test]
fn run_args_repeatable_image_flag_collects_all_paths() {
    use clap::Parser;
    let cli = Cli::try_parse_from([
        "myagent", "run", "hi", "--image", "a.png", "--image", "b.jpg",
    ])
    .unwrap();
    match cli.command {
        Some(Command::Run(args)) => {
            assert_eq!(
                args.image_args.image,
                vec![PathBuf::from("a.png"), PathBuf::from("b.jpg")]
            );
        }
        _ => panic!("expected run"),
    }
}

#[test]
fn run_args_image_flag_defaults_to_empty() {
    use clap::Parser;
    let cli = Cli::try_parse_from(["myagent", "run", "hi"]).unwrap();
    match cli.command {
        Some(Command::Run(args)) => assert!(args.image_args.image.is_empty()),
        _ => panic!("expected run"),
    }
}

#[test]
fn plan_and_resume_args_accept_repeatable_image_flag() {
    use clap::Parser;
    let cli = Cli::try_parse_from([
        "myagent", "plan", "goal", "--image", "a.png", "--image", "b.jpg",
    ])
    .unwrap();
    match cli.command {
        Some(Command::Plan(args)) => {
            assert_eq!(
                args.image_args.image,
                vec![PathBuf::from("a.png"), PathBuf::from("b.jpg")]
            );
        }
        _ => panic!("expected plan"),
    }

    let cli = Cli::try_parse_from([
        "myagent", "resume", "run-1", "--image", "a.png", "--image", "b.jpg",
    ])
    .unwrap();
    match cli.command {
        Some(Command::Resume(args)) => {
            assert_eq!(
                args.image_args.image,
                vec![PathBuf::from("a.png"), PathBuf::from("b.jpg")]
            );
        }
        _ => panic!("expected resume"),
    }
}

#[test]
fn provider_help_text_documents_supports_images_env_override() {
    // item 6：`{PREFIX}_SUPPORTS_IMAGES` 这个兜底开关（`config_images::resolve`）此前
    // 存在但没写进任何 --help / 文档——用户只能翻源码才知道有这个开关。
    use clap::CommandFactory;
    let help = Cli::command()
        .find_subcommand("run")
        .unwrap()
        .clone()
        .render_long_help()
        .to_string();
    assert!(
        help.contains("SUPPORTS_IMAGES"),
        "run 的 --provider help 必须提到 {{PREFIX}}_SUPPORTS_IMAGES 覆盖开关：{help}"
    );
}

#[test]
fn image_help_text_does_not_claim_interactive_only_sends_first_fresh_run() {
    // P3-8：旧文案说「interactive 只随首次 fresh run 带出去」，但 cli.rs 的 resume
    // 分支同样 `mem::take(&mut pending_images)`——interactive 里第一条就 `/resume`
    // 的话，图片照样会挂到 resume 的追加消息上。文案必须说清「本次追加的消息」
    // 这个真实语义，不能暗示"只有 fresh run 才行"。
    use clap::CommandFactory;
    let help = Cli::command()
        .find_subcommand("run")
        .unwrap()
        .clone()
        .render_long_help()
        .to_string();
    assert!(
        !help.contains("interactive 只随首次 fresh run 带出去"),
        "help 文案仍在说过时的「只首次 fresh run」承诺：{help}"
    );
    assert!(
        help.contains("resume") && help.contains("interactive"),
        "help 文案必须提到 resume/interactive 都会挂图：{help}"
    );
}

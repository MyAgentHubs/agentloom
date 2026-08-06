//! file-size 棘轮：挡住未来新增/长大的胖 src 文件（GUIDELINES §5 模块小而专）。
//! 规矩：非白名单 src 文件 ≤ 800 行；白名单文件 ≤ 记录上限（只许降不许升）。
//! 行数 = 总物理行数（数 `\n` 字节，模拟 wc -l，与现有白名单值口径一致）。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// 白名单：(相对 src 的路径, 当前行数上限)。只许降不许升。
/// `orchestrator/mod.rs` 是拆分期临时项，最后一刀（Task 8）移除，
/// 只留 `orchestrator/run_loop.rs` + `orchestrator/tests.rs`。
const WHITELIST: &[(&str, usize)] = &[
    // 棘轮重基线：evidence gate 与自适应安全网已合入；后续独立拆 run-loop 阶段处理器。
    ("orchestrator/run_loop.rs", 2983),
    // 棘轮重基线：随 run-loop 行为补齐的大量回归测试；后续按行为域拆测试模块。
    ("orchestrator/tests.rs", 8521),
    ("orchestrator/probe_runner.rs", 1488),
    ("plan/run_plan.rs", 4955),
    // 棘轮收口：2155 之后叠加 MCP 管理/注入、fs read/write fence 等已合入 CLI 能力；
    // 本次先同步实际值，后续独立拆分 CLI 参数解析、命令执行与内联测试后再下拉。
    ("cli.rs", 2542),
    ("evaluator.rs", 1905),
    ("plan/contract.rs", 1138),
    ("guardrails.rs", 1055),
    ("plan/replan.rs", 1023),
    ("plan/write_audit.rs", 1031),
    ("provider/openai_compatible.rs", 1068),
    // 棘轮重基线：跨平台 shell、专属进程组收割和 checkpoint 竞态修复；后续按职责拆分。
    ("exec/controlled/mod.rs", 960),
    // 棘轮重基线：从 controlled.rs 原样搬出的回归测试；后续按行为域拆测试模块。
    ("exec/controlled/tests.rs", 934),
    ("tools/fs_edit.rs", 915),
    ("tools/fs_write.rs", 916),
    ("tools/mod.rs", 845),
    ("mcp/tool.rs", 831),
    // 棘轮收口：provider 协议自动判定配置入口(+126)
    ("config.rs", 1074),
];

const HARD_LIMIT: usize = 800;

fn count_lines(bytes: &[u8]) -> usize {
    bytes.iter().filter(|&&b| b == b'\n').count()
}

fn src_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src")
}

fn collect_rs(dir: &Path, root: &Path, out: &mut BTreeMap<String, usize>) {
    for entry in std::fs::read_dir(dir).expect("read_dir") {
        let path = entry.expect("dir entry").path();
        if path.is_dir() {
            collect_rs(&path, root, out);
        } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
            let rel = path
                .strip_prefix(root)
                .expect("strip_prefix")
                .to_string_lossy()
                .replace('\\', "/");
            let bytes = std::fs::read(&path).expect("read file");
            out.insert(rel, count_lines(&bytes));
        }
    }
}

#[test]
fn no_unwhitelisted_src_file_exceeds_hard_limit() {
    let root = src_root();
    let mut files = BTreeMap::new();
    collect_rs(&root, &root, &mut files);

    let whitelist: BTreeMap<&str, usize> = WHITELIST.iter().copied().collect();
    let mut violations = Vec::new();

    for (rel, &lines) in &files {
        match whitelist.get(rel.as_str()) {
            Some(&cap) => {
                if lines > cap {
                    violations.push(format!(
                        "{rel}: {lines} 行 > 白名单上限 {cap}——棘轮只许降不许升；\
                         确需放大则改白名单并在 commit 写明理由"
                    ));
                }
            }
            None => {
                if lines > HARD_LIMIT {
                    violations.push(format!(
                        "{rel}: {lines} 行 > {HARD_LIMIT} 且不在白名单——按 GUIDELINES §5 拆分，\
                         或加入白名单并在 commit 写明为何这一坨确是一件事、不该拆"
                    ));
                }
            }
        }
    }

    for (rel, _) in WHITELIST {
        if !files.contains_key(*rel) {
            violations.push(format!(
                "{rel}: 在白名单但文件不存在——拆分/改名后请同步更新白名单"
            ));
        }
    }

    assert!(
        violations.is_empty(),
        "file-size 棘轮失败（GUIDELINES §5 模块小而专）:\n{}",
        violations.join("\n")
    );
}

//! file-size 棘轮：挡住未来新增/长大的胖 src 文件（GUIDELINES §5 模块小而专）。
//! 规矩：非白名单 src 文件 ≤ 800 行；白名单文件 ≤ 记录上限（只许降不许升）。
//! 行数 = 总物理行数（数 `\n` 字节，模拟 wc -l，与现有白名单值口径一致）。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// 白名单：(相对 src 的路径, 当前行数上限)。只许降不许升。
/// `orchestrator/mod.rs` 是拆分期临时项，最后一刀（Task 8）移除，
/// 只留 `orchestrator/run_loop.rs` + `orchestrator/tests.rs`。
const WHITELIST: &[(&str, usize)] = &[
    // 棘轮收口：ProviderResponse 加 interruption 字段·16 处测试构造点机械补 None(+16)
    // PlanRunOptions gained extra_read_roots with forwarding through child_run_options (+4).
    // The tail of mod tests moved to the external file
    // plan/run_plan/tests/tail.rs to pass the check_file_size.py gate; the smaller
    // main file now has a tighter limit with no slack.
    ("plan/run_plan.rs", 4377),
    // 棘轮收口：2155 之后叠加 MCP 管理/注入、fs read/write fence 等已合入 CLI 能力；
    // 本次先同步实际值，后续独立拆分 CLI 参数解析、命令执行与内联测试后再下拉。
    // 棘轮收口：config mcp add 补 --header KEY=VALUE（可重复）+ 抽出 parse_kv_pairs
    // 复用于 env/header 解析 + list 打印 headers 名（不打印值）(+28)
    // 棘轮收口：resume 路径接线 mcp_servers——resume_with_provider/interactive
    // resume 分支补 config::load_config 加载 + 转发(+9)
    // Added an environment-variable shadowing comment to config mcp list (+1).
    // Added a repeatable --read-root <DIR> CLI flag to four subcommands,
    // resolve_read_roots validation, and extra_read_roots forwarding through
    // run/resume/plan/interactive (+36).
    // All of mod tests moved to cli/tests.rs to pass the check_file_size.py gate;
    // the smaller main file now has a tighter limit with no slack.
    // 棘轮收口：t12-img --image CLI 接线（mod 声明 + 4 处 flatten + load_images
    // 调用 + images 透传 + config info 覆盖显示）(+29)·权威门禁 check_file_size.py
    // 额度 2580 内。
    ("cli.rs", 1984),
    ("evaluator.rs", 1905),
    // 棘轮收口：超时语义改空闲(read_timeout)+连接超时,附约束注释(+4)
    // 棘轮收口：ProviderResponse 加 interruption 字段——collect() 中断分支把断流事实
    // 上车 + 本文件内 5 处构造点机械补 None(+8)
    // 棘轮收口：T2c Minor-1 断流文案区分空闲超时——is_timeout() 分流 + 实勘注释(+13)
    // 棘轮重基线：跨平台 shell、专属进程组收割和 checkpoint 竞态修复；后续按职责拆分。
    ("exec/controlled/mod.rs", 960),
    // 棘轮重基线：从 controlled.rs 原样搬出的回归测试；后续按行为域拆测试模块。
    ("exec/controlled/tests.rs", 934),
    // 棘轮收紧：tools/fs_edit.rs / tools/fs_write.rs / tools/mod.rs / mcp/tool.rs
    // Adding extra_read_roots to ToolContext for --read-root briefly pushed tools/fs_edit.rs,
    // tools/fs_write.rs, tools/mod.rs, and mcp/tool.rs above the 800-line hard cap. Their mod tests
    // moved to tools/fs_edit/tests.rs, tools/fs_write/tests.rs, tools/tests.rs, and mcp/tool/tests.rs,
    // bringing each main file below 800 lines and removing all four whitelist entries (former limits:
    // fs_edit.rs 919 / fs_write.rs 920 / mod.rs 849 / tool.rs 831).
    // 棘轮收口：provider 协议自动判定配置入口(+126)
    // 棘轮收口：timeout_secs 加 {env_prefix}_TIMEOUT_SECS→MYAGENT_TIMEOUT_SECS 覆盖链
    // + 对应 5 条 env 覆盖/回退测试(+71)
    // 棘轮收口：timeout_secs 非法值(parse 失败/等于 0)改硬报错——对齐邻居字段
    // (temperature/top_p/output_tokens)语义，拒绝静默 fail-open(+15)
    // 棘轮收口：模型登记表按官方 API 文档校准——default_context_tokens/
    // default_output_tokens 补 zai 分支来源注释 + 新增 zai_has_output_default_not_none
    // 回归测试(+26)
    // 棘轮收口：McpServerConfig 加 headers 字段·4 处既有测试字面量机械补 headers: None(+4)
    // 棘轮收紧：config.rs 把 mod tests 整段搬到外部文件 config/tests.rs（过
    // check_file_size.py 硬门禁），主文件降回 800 行以下，不再需要白名单条目——
    // 原条目（1190）整条移除。
    //
    // 棘轮收紧：mcp/client.rs 把 mod tests 整段搬到外部文件 mcp/client/tests.rs
    // （过 check_file_size.py 硬门禁），主文件降回 800 行以下，不再需要白名单
    // 条目——原条目（909）整条移除。
    //
    // safety/dangerous_paths.rs briefly exceeded the 800-line hard cap when --read-root threaded
    // extra_read_roots through ScanCtx and dangerous_command_scan with matching positive/negative
    // test cases. All of mod tests moved to the external file
    // safety/dangerous_paths/tests.rs to pass the check_file_size.py gate, bringing the main file
    // below 800 lines and removing its whitelist entry (former limit: 919).
    //
    // plan/contract.rs, plan/replan.rs, and plan/write_audit.rs each had their inline mod tests
    // moved to external files (plan/contract/tests.rs, plan/replan/tests.rs, and
    // plan/write_audit/tests.rs respectively) to pass the check_file_size.py gate, bringing each
    // main file below 800 lines and removing all three whitelist entries (former limits:
    // contract.rs 1138 / replan.rs 1023 / write_audit.rs 1031).
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

/// Decides whether a whitelisted file is stale, i.e. its real line count has
/// dropped back to (or below) `HARD_LIMIT` and no longer needs a whitelist
/// entry at all. Unlike the cap itself (which only ever ratchets down and can
/// legitimately sit above the real count as slack), staleness is a hard
/// equality-adjacent check: once a file is back under the universal limit,
/// keeping its entry around just hides future regressions.
fn is_stale_entry(lines: usize) -> bool {
    lines <= HARD_LIMIT
}

fn size_violations(files: &BTreeMap<String, usize>, whitelist: &[(&str, usize)]) -> Vec<String> {
    let whitelist_map: BTreeMap<&str, usize> = whitelist.iter().copied().collect();
    let mut violations = Vec::new();

    for (rel, &lines) in files {
        match whitelist_map.get(rel.as_str()) {
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

    for (rel, &cap) in &whitelist_map {
        match files.get(*rel) {
            None => violations.push(format!(
                "{rel}: 在白名单但文件不存在——拆分/改名后请同步更新白名单"
            )),
            Some(&lines) => {
                if is_stale_entry(lines) {
                    violations.push(format!(
                        "{rel}: {lines} 行已降到硬上限 {HARD_LIMIT} 以内（白名单上限 {cap}）——\
                         已降到上限内，请删掉白名单条目"
                    ));
                }
            }
        }
    }

    violations
}

#[test]
fn no_unwhitelisted_src_file_exceeds_hard_limit() {
    let root = src_root();
    let mut files = BTreeMap::new();
    collect_rs(&root, &root, &mut files);

    let violations = size_violations(&files, WHITELIST);

    assert!(
        violations.is_empty(),
        "file-size 棘轮失败（GUIDELINES §5 模块小而专）:\n{}",
        violations.join("\n")
    );
}

#[cfg(test)]
mod size_violations_samples {
    use super::size_violations;
    use std::collections::BTreeMap;

    #[test]
    fn whitelisted_file_within_cap_and_above_hard_limit_passes() {
        let mut files = BTreeMap::new();
        files.insert("big.rs".to_string(), 900);
        let whitelist = [("big.rs", 1000)];
        assert!(size_violations(&files, &whitelist).is_empty());
    }

    #[test]
    fn whitelisted_file_over_its_cap_is_blocked() {
        let mut files = BTreeMap::new();
        files.insert("big.rs".to_string(), 1200);
        let whitelist = [("big.rs", 1000)];
        let violations = size_violations(&files, &whitelist);
        assert_eq!(violations.len(), 1);
        assert!(violations[0].contains("big.rs"));
    }

    #[test]
    fn whitelisted_file_shrunk_back_under_hard_limit_is_stale() {
        let mut files = BTreeMap::new();
        files.insert("shrunk.rs".to_string(), 500);
        let whitelist = [("shrunk.rs", 1000)];
        let violations = size_violations(&files, &whitelist);
        assert_eq!(violations.len(), 1);
        assert!(violations[0].contains("请删掉白名单条目"));
    }
}

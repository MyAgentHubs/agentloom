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
    // 棘轮收口：ProviderResponse 加 interruption 字段·本文件内测试用构造点机械补 None(+1)
    // 棘轮收口：T2b 主循环消费 interruption——新增拦截分支+计数器(+24)
    // 棘轮收口：T2c M-1 断流分支收窄到 finish_reason 也缺失——嵌套 if + 加长注释(+8)
    // 棘轮收口：截断分支补占位工具结果（append_unpaired_tool_results）修 conversation
    // pairing invalid 崩溃——分支入口一次调用 + 注释(+10)
    // 棘轮收紧：T13 --read-root 特性把 `immediate_diagnostic_tests` 测试模块搬到
    // 外部文件 orchestrator/run_loop/immediate_diagnostic_tests.rs（过 check_file_size.py
    // 800/quota 硬门禁），主文件实际行数下降，同步收紧上限，不留余量。
    ("orchestrator/run_loop.rs", 2857),
    // 棘轮重基线：随 run-loop 行为补齐的大量回归测试；后续按行为域拆测试模块。
    // 棘轮收口：ProviderResponse 加 interruption 字段·88 处测试构造点机械补 None(+88)
    // 棘轮收口：T2b 断流轮行为回归测试——5 条新用例 + mock provider/helper(+270)
    // 棘轮收口：T2c M-1 新增回归测试——finish_reason 已收到的完整轮不误判断流(+64)
    // 棘轮收口：截断分支补占位工具结果——复现钉 + 撞连续上限路径回归测试 2 条(+132)
    // 棘轮收口：McpServerConfig 加 headers 字段·1 处既有测试字面量机械补 headers: None(+1)
    // 棘轮收口：resume_solo_with_judge 新增 mcp_servers 形参·4 处既有调用点机械补
    // 尾参 Vec::new()(+4)
    // 棘轮收口：T13 --read-root 特性·多处既有 RunOptions 构造点机械补
    // extra_read_roots: Vec::new()(+2)
    // 棘轮收紧：T13 把文件尾部一批测试搬到外部文件 orchestrator/tests/tail.rs
    // （过 check_file_size.py 硬门禁），主文件实际行数下降，同步收紧上限，不留余量。
    ("orchestrator/tests.rs", 8943),
    ("orchestrator/probe_runner.rs", 1488),
    // 棘轮收口：ProviderResponse 加 interruption 字段·16 处测试构造点机械补 None(+16)
    // 棘轮收口：T13 PlanRunOptions 加 extra_read_roots 字段 + child_run_options 透传(+4)
    // 棘轮收紧：T13 把 mod tests 尾部一段测试搬到外部文件
    // plan/run_plan/tests/tail.rs（过 check_file_size.py 硬门禁），主文件实际行数
    // 下降，同步收紧上限，不留余量。
    ("plan/run_plan.rs", 4377),
    // 棘轮收口：2155 之后叠加 MCP 管理/注入、fs read/write fence 等已合入 CLI 能力；
    // 本次先同步实际值，后续独立拆分 CLI 参数解析、命令执行与内联测试后再下拉。
    // 棘轮收口：config mcp add 补 --header KEY=VALUE（可重复）+ 抽出 parse_kv_pairs
    // 复用于 env/header 解析 + list 打印 headers 名（不打印值）(+28)
    // 棘轮收口：resume 路径接线 mcp_servers——resume_with_provider/interactive
    // resume 分支补 config::load_config 加载 + 转发(+9)
    // 2026-09-03 +1：config mcp list env 遮蔽注释
    // 棘轮收口：T13 加 --read-root <DIR>（可重复）·4 个子命令各加一个 CLI 参数 +
    // resolve_read_roots 校验 + 贯穿 run/resume/plan/interactive 的 extra_read_roots
    // 透传(+36)
    // 棘轮收紧：T13 把 mod tests 整段搬到外部文件 cli/tests.rs（过
    // check_file_size.py 硬门禁），主文件实际行数下降，同步收紧上限，不留余量。
    // 棘轮收口：t12-img --image CLI 接线（mod 声明 + 4 处 flatten + load_images
    // 调用 + images 透传 + config info 覆盖显示）(+29)·权威门禁 check_file_size.py
    // 额度 2580 内。
    ("cli.rs", 1984),
    ("evaluator.rs", 1905),
    ("plan/contract.rs", 1138),
    ("guardrails.rs", 1055),
    ("plan/replan.rs", 1023),
    ("plan/write_audit.rs", 1031),
    // 棘轮收口：超时语义改空闲(read_timeout)+连接超时,附约束注释(+4)
    // 棘轮收口：ProviderResponse 加 interruption 字段——collect() 中断分支把断流事实
    // 上车 + 本文件内 5 处构造点机械补 None(+8)
    // 棘轮收口：T2c Minor-1 断流文案区分空闲超时——is_timeout() 分流 + 实勘注释(+13)
    ("provider/openai_compatible.rs", 1093),
    // 棘轮重基线：跨平台 shell、专属进程组收割和 checkpoint 竞态修复；后续按职责拆分。
    ("exec/controlled/mod.rs", 960),
    // 棘轮重基线：从 controlled.rs 原样搬出的回归测试；后续按行为域拆测试模块。
    ("exec/controlled/tests.rs", 934),
    // 棘轮收紧：tools/fs_edit.rs / tools/fs_write.rs / tools/mod.rs / mcp/tool.rs
    // 曾因 T13 ToolContext 加 extra_read_roots 字段短暂越过 800（各 mod tests 整段
    // 搬到外部文件：tools/fs_edit/tests.rs、tools/fs_write/tests.rs、
    // tools/tests.rs、mcp/tool/tests.rs），搬完主文件均已降回 800 行以下，
    // 不再需要白名单条目——原 4 条（fs_edit.rs 919 / fs_write.rs 920 /
    // mod.rs 849 / tool.rs 831）整条移除。
    // 棘轮收口：provider 协议自动判定配置入口(+126)
    // 棘轮收口：timeout_secs 加 {env_prefix}_TIMEOUT_SECS→MYAGENT_TIMEOUT_SECS 覆盖链
    // + 对应 5 条 env 覆盖/回退测试(+71)
    // 棘轮收口：timeout_secs 非法值(parse 失败/等于 0)改硬报错——对齐邻居字段
    // (temperature/top_p/output_tokens)语义，拒绝静默 fail-open(+15)
    // 棘轮收口：模型登记表按官方 API 文档校准——default_context_tokens/
    // default_output_tokens 补 zai 分支来源注释 + 新增 zai_has_output_default_not_none
    // 回归测试(+26)
    // 棘轮收口：McpServerConfig 加 headers 字段·4 处既有测试字面量机械补 headers: None(+4)
    ("config.rs", 1190),
    // 棘轮收口：MCP Streamable HTTP 自定义请求头——build header map（HeaderName/
    // HeaderValue + ${ENV_NAME} 展开）+ connect_with_timeouts 分支 + 7 条单测，
    // 首次越过 800 硬上限，本文件仍是「一件事」（一个 mcp client 连接实现），暂不拆分。
    ("mcp/client.rs", 909),
    // 棘轮收紧：safety/dangerous_paths.rs 曾因 T13 --read-root 贯穿 shell 危险命令
    // 扫描（ScanCtx 加 extra_read_roots 字段 + dangerous_command_scan 新参数 + 对应
    // 正反测试语料）首次越过 800 硬上限；已把 mod tests 整段搬到外部文件
    // safety/dangerous_paths/tests.rs（过 check_file_size.py 硬门禁），主文件降回
    // 800 行以下，不再需要白名单条目——原条目（919）整条移除。
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

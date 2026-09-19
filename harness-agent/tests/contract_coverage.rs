fn contract() -> String {
    std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/CONTRACT.md")).unwrap()
}

#[test]
fn documents_every_vocabulary_type() {
    let c = contract();
    for ty in myagent::vocabulary::VOCABULARY {
        assert!(c.contains(ty), "CONTRACT.md missing type `{ty}`");
    }
}

/// P2-2 反向检查：`documents_every_vocabulary_type` 只单向核「VOCABULARY 里的都在
/// CONTRACT.md 里」——抓不到「代码里真发了、但 VOCABULARY 压根没登记」这种契约漏记。
/// 这里逐个源文件扫 `.emit("literal")` 字面量事件名，要求全部落在 VOCABULARY 里。
///
/// 唯一的显式豁免：`provider.mock.images_received`——纯 mock provider 内部用事件流
/// 充当"测试断言通道"（mock 没有别的可变状态可查），从未上过真机 wire，不属对外
/// `harness.runtime.v1` 契约，见 `src/provider/mock.rs` 里的注释。
const CONTRACT_EXEMPT_EVENT_TYPES: &[&str] = &["provider.mock.images_received"];

#[test]
fn every_emitted_literal_event_type_is_registered_in_vocabulary() {
    let src_root = std::path::PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/src"));
    let mut missing: Vec<String> = Vec::new();
    for entry in walk_rs_files(&src_root) {
        let text = std::fs::read_to_string(&entry).unwrap();
        for name in extract_emit_literals(&text) {
            if CONTRACT_EXEMPT_EVENT_TYPES.contains(&name.as_str()) {
                continue;
            }
            if !myagent::vocabulary::VOCABULARY.contains(&name.as_str()) {
                missing.push(format!("{name} ({})", entry.display()));
            }
        }
    }
    assert!(
        missing.is_empty(),
        "以下 .emit(\"...\") 字面量事件名没有登记进 vocabulary::VOCABULARY：\n{}",
        missing.join("\n")
    );
}

fn walk_rs_files(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return out,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            out.extend(walk_rs_files(&path));
        } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
            out.push(path);
        }
    }
    out
}

/// 没有引入 regex 依赖——用最简单的字符串扫描找 `.emit("literal"` / `::emit("literal"`
/// （后者是 UFCS 形态，如 `EventRecorder::emit(r, "x", ...)`——t12-img 第四轮返工
/// P3-D：此前只认 `.emit(`，UFCS 调用点会被两个扫描器一起漏过）的第一个字符串参数。
/// **已知盲区**：变量/`format!` 拼出来的事件名这个函数天然认不出——那些走
/// `find_dynamic_emit_lines` + `DYNAMIC_EMIT_WHITELIST` 单独人工核对（P3-1）；再往
/// 深一层，把 `emit` 存成函数指针/闭包再间接调用、或用宏生成调用点，两个扫描器
/// （这个 + `find_dynamic_emit_lines`）都识别不出——目前仓内没有这类写法（已 grep
/// 核过 `::emit(`/`.emit(` 全部调用点），但这不是结构性保证，只是现状。
fn extract_emit_literals(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for site in find_emit_call_sites(text) {
        let after = &text[site.args_start..];
        let trimmed = after.trim_start();
        if let Some(stripped) = trimmed.strip_prefix('"') {
            if let Some(end) = stripped.find('"') {
                out.push(stripped[..end].to_string());
            }
        }
    }
    out
}

/// 一个 `.emit(`/`::emit(` 调用点。
struct EmitCallSite {
    /// needle（`.` 或 `::` 的起始字符）在文本里的字节偏移——用来定位行号/判断整行
    /// 是不是注释。
    needle_start: usize,
    /// 参数列表起始位置（`emit(` 后一个字符）。
    args_start: usize,
}

/// 扫出全文所有 `.emit(`/`::emit(` 调用点。两种 needle 互不重叠（`emit(` 前一个字符
/// 要么是 `.` 要么是第二个 `:`），天然也不会命中 `pub fn emit(...)` 这个定义本身
/// （定义前面既没有 `.` 也没有 `::`）。
fn find_emit_call_sites(text: &str) -> Vec<EmitCallSite> {
    let mut out = Vec::new();
    for needle in [".emit(", "::emit("] {
        let mut rest_from = 0usize;
        while let Some(rel) = text[rest_from..].find(needle) {
            let idx = rest_from + rel;
            out.push(EmitCallSite {
                needle_start: idx,
                args_start: idx + needle.len(),
            });
            rest_from = idx + needle.len();
        }
    }
    out.sort_by_key(|s| s.needle_start);
    out
}

#[test]
fn documents_exit_codes_and_invariants() {
    let c = contract();
    for n in ["`0`", "`1`", "`2`", "`3`", "`4`", "`130`"] {
        assert!(c.contains(n), "missing exit code {n}");
    }
    for s in [
        "schema_version",
        "harness.runtime.v1",
        "seq",
        "Tier 0",
        "Tier 1",
        "null",
        "completed",
        "blocked",
        "needs_decision",
        "interrupted",
    ] {
        assert!(c.contains(s), "CONTRACT.md missing `{s}`");
    }
}

#[test]
fn documents_control_commands_and_criteria_and_status_set() {
    let c = contract();
    for cmd in [
        "stop",
        "approve",
        "reject",
        "pause",
        "resume",
        "revise",
        "inspect_runtime",
    ] {
        assert!(c.contains(cmd), "missing control command {cmd}");
    }
    for syn in ["cmd:", "contains:", "judge:"] {
        assert!(c.contains(syn), "missing criteria syntax {syn}");
    }
    for st in ["pending", "passed", "failed", "waived", "uncertain"] {
        assert!(c.contains(st), "missing status {st}");
    }
    assert!(c.contains(".myagenthubs/runs"), "missing journal layout");
}

/// P3-1 反向契约检查（t12-img 第三轮 opus 审）：`extract_emit_literals` 只认
/// `.emit(` 后紧跟字符串字面量的形态——对 `.emit(event_type, ...)` 这类事件类型是
/// 变量的调用点完全是盲的（opus 变异实验坐实：加一个
/// `let t = "zz.unregistered.dynamic.a"; r.emit(t, ...)` 不会被 `every_emitted_
/// literal_event_type_is_registered_in_vocabulary` 抓到）。这里逐个 `.emit(` 调用
/// 点判断第一个实参是不是字符串字面量；不是的话必须在下面的显式白名单里登记
/// （file、line、该处变量实际可能取哪些值——人工核对这些值都已登记进
/// `vocabulary::VOCABULARY`）。白名单外的动态 `.emit(` 调用点一律判红。
struct DynamicEmitSite {
    file: &'static str,
    line: usize,
    /// 该调用点事件类型变量实际可能取的值（已逐个核对进 `vocabulary::VOCABULARY`）。
    possible_values: &'static [&'static str],
}

const DYNAMIC_EMIT_WHITELIST: &[DynamicEmitSite] = &[
    DynamicEmitSite {
        file: "src/orchestrator/run_loop.rs",
        line: 236,
        possible_values: &[
            "evidence.probe.green",
            "evidence.probe.still_red",
            "evidence.probe.workspace_mutated",
            "evidence.probe.infra",
        ],
    },
    DynamicEmitSite {
        file: "src/orchestrator/run_loop.rs",
        line: 487,
        possible_values: &["evidence.probe.registered", "evidence.probe.rejected"],
    },
    DynamicEmitSite {
        file: "src/plan/run_plan.rs",
        // 行号会随合并漂移，后续应改为按锚点注释匹配。
        line: 980,
        possible_values: &[
            "plan.preflight.pre_green",
            "plan.preflight.refine_requested",
        ],
    },
];

#[test]
fn every_dynamic_emit_call_site_is_explicitly_whitelisted() {
    let src_root = std::path::PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/src"));
    let manifest_root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut unlisted: Vec<String> = Vec::new();
    for entry in walk_rs_files(&src_root) {
        let text = std::fs::read_to_string(&entry).unwrap();
        for line in find_dynamic_emit_lines(&text) {
            let rel = entry
                .strip_prefix(&manifest_root)
                .unwrap_or(entry.as_path());
            let rel_str = rel.to_string_lossy().replace('\\', "/");
            let listed = DYNAMIC_EMIT_WHITELIST
                .iter()
                .any(|w| w.file == rel_str && w.line == line);
            if !listed {
                unlisted.push(format!("{rel_str}:{line}"));
            }
        }
    }
    assert!(
        unlisted.is_empty(),
        "以下 .emit(<非字符串字面量>) 调用点不在 DYNAMIC_EMIT_WHITELIST 里，必须登记 \
         （file/line/可取值）并核实这些值都已登记进 vocabulary::VOCABULARY：\n{}",
        unlisted.join("\n")
    );

    // 反向兜底：白名单登记的行如果代码搬走了（重构改了行号/删掉了变量），也要及时
    // 更新——否则白名单会悄悄"过期放行"真正的新漏洞而不自知。
    for w in DYNAMIC_EMIT_WHITELIST {
        assert!(
            !w.possible_values.is_empty(),
            "{}:{} 的 possible_values 不能是空——白名单存在的意义就是让人核对可取值",
            w.file,
            w.line
        );
        // t12-img 第四轮返工（P3-D）：此前这里只断言 possible_values 非空，从不校验
        // 里面的值真的登记进了 vocabulary::VOCABULARY——白名单条目以后多出一个未登记
        // 的取值，这个守卫照样绿。
        for value in w.possible_values {
            assert!(
                myagent::vocabulary::VOCABULARY.contains(value),
                "{}:{} 的 possible_values 里 `{value}` 没有登记进 vocabulary::VOCABULARY",
                w.file,
                w.line
            );
        }
        let path = manifest_root.join(w.file);
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("白名单登记的文件读不到：{} ({e})", w.file));
        let lines = find_dynamic_emit_lines(&text);
        assert!(
            lines.contains(&w.line),
            "白名单登记的 {}:{} 已经不是一个动态 .emit( 调用点了（代码搬走了？更新白名单登记的行号）",
            w.file,
            w.line
        );
    }
}

/// 找出文本里所有「`.emit(`/`::emit(` 后第一个非空白字符不是 `"`」的调用点，返回其
/// 1-based 行号（t12-img 第四轮返工 P3-D：与 `extract_emit_literals` 共用
/// `find_emit_call_sites`，同样认 UFCS 形态）。
fn find_dynamic_emit_lines(text: &str) -> Vec<usize> {
    let mut out = Vec::new();
    for site in find_emit_call_sites(text) {
        // 跳过注释里提到 `.emit(...)` 的行（例如 vocabulary.rs 里解释某事件类型来源
        // 的说明性注释）——不是真调用点。判定：该行去掉前导空白后以 `//` 开头。
        let line_start = text[..site.needle_start]
            .rfind('\n')
            .map(|p| p + 1)
            .unwrap_or(0);
        let line_prefix = text[line_start..site.needle_start].trim_start();
        if line_prefix.starts_with("//") {
            continue;
        }
        let after = &text[site.args_start..];
        let trimmed = after.trim_start();
        if !trimmed.starts_with('"') {
            let line = text[..site.needle_start].matches('\n').count() + 1;
            out.push(line);
        }
    }
    out
}

// t12-img 第四轮返工（P3-D）：直接对扫描器的纯字符串函数做单元测试（不落物理文件到
// src/），钉住 UFCS 形态不再被两个扫描器一起漏过——复现 opus 复审第 5 节的三个探针
// 调用点。

#[test]
fn find_emit_call_sites_recognizes_dot_and_ufcs_forms() {
    let src = "recorder.emit(\"dot.form\", payload);\n\
               crate::events::EventRecorder::emit(r, \"ufcs.form\", payload);\n";
    assert_eq!(
        find_emit_call_sites(src).len(),
        2,
        "`.emit(` 和 `::emit(` 两种形态都必须被扫描器认出"
    );
}

#[test]
fn extract_emit_literals_unaffected_by_ufcs_receiver_argument() {
    // `.emit(` 形态：第一个实参就是事件类型字面量，照常提取。
    // `::emit(` 形态：UFCS 调用第一个实参是 receiver（不是事件类型），提取不到字面量
    // 是预期行为——这条调用点会经 `find_dynamic_emit_lines` 落到「动态调用点」那条
    // 检查路径，强制人工登记进 `DYNAMIC_EMIT_WHITELIST`（不会被静默放过）。
    let src = "recorder.emit(\"dot.form\", payload);\n\
               crate::events::EventRecorder::emit(r, \"ufcs.form\", payload);\n";
    assert_eq!(extract_emit_literals(src), vec!["dot.form".to_string()]);
}

#[test]
fn find_dynamic_emit_lines_catches_ufcs_dynamic_and_ufcs_literal_call_sites() {
    // 复现 opus 第三轮返工复审第 5 节的探针：三个调用点里，只有 `.emit(t, …)`
    // （方法调用·动态变量）此前会被抓到；两个 `::emit(` UFCS 调用点（一个变量、一个
    // 字面量都在第二个实参位置）此前会被两个扫描器一起漏过。三个现在都必须落进
    // `find_dynamic_emit_lines`（UFCS 那两个的第一实参是 receiver、不是字符串字面量，
    // 天然会被判「动态」——即使第二个实参其实是字面量，也强制走白名单人工核对，不会
    // 静默放过）。
    let src = "\
pub fn probe(r: &mut crate::events::EventRecorder) {
    let t = \"zz.unregistered.dynamic.a\";
    let _ = r.emit(t, serde_json::json!({}));
    let _ = crate::events::EventRecorder::emit(r, t, serde_json::json!({}));
    let _ = crate::events::EventRecorder::emit(r, \"zz.unregistered.literal\", serde_json::json!({}));
}
";
    let lines = find_dynamic_emit_lines(src);
    assert_eq!(
        lines,
        vec![3, 4, 5],
        "三个探针调用点必须全部落进「动态调用点」清单（UFCS 两种形态都不能再被漏过）：{lines:?}"
    );
}

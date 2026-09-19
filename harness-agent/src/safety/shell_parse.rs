//! shell 命令保守解析：只为危险扫描服务·吃不准就让上层对写类 fail-closed。
//! 不是通用 shell 解析器（解释器/对抗混淆不在防护内·设计 §二）。

/// 写/删类命令（路径越界 + 危险目标即拒·T3 用）。
pub const WRITE_COMMANDS: &[&str] = &[
    "rm", "rmdir", "mv", "cp", "dd", "truncate", "tee", "install", "ln", "touch", "mkdir",
];
/// 读类命令（路径越界即拒·best-effort·T3 用）。
pub const READ_COMMANDS: &[&str] = &[
    "cat", "head", "tail", "less", "more", "grep", "awk", "sort", "nl", "od", "hexdump", "strings",
    "wc", "cut",
];
/// 剥前缀的 wrapper。
pub const WRAPPERS: &[&str] = &["timeout", "nohup", "nice", "env", "stdbuf", "time"];

fn is_grouping_token(token: &str) -> bool {
    matches!(token, "(" | ")" | "{" | "}")
}

/// 一个 shell token：text=去引号字面值；is_operator=分隔/重定向操作符；dynamic=含未展开 expansion。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token {
    pub text: String,
    pub is_operator: bool,
    pub dynamic: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RedirOp {
    Out,
    Append,
    In,
}

/// quote-aware 分词。None = 引号不平衡 / 尾随裸 `\`（写类 fail-closed 信号）。
pub fn tokenize(cmd: &str) -> Option<Vec<Token>> {
    let mut tokens: Vec<Token> = Vec::new();
    let mut cur = String::new();
    let mut has = false;
    let mut quoted = false;
    let mut dynamic = false;
    let mut leading_tilde_name = false;
    let mut chars = cmd.chars().peekable();

    macro_rules! flush {
        () => {
            if has {
                tokens.push(Token {
                    text: std::mem::take(&mut cur),
                    is_operator: false,
                    dynamic: dynamic || leading_tilde_name,
                });
                has = false;
                quoted = false;
                dynamic = false;
                leading_tilde_name = false;
            }
        };
    }

    while let Some(c) = chars.next() {
        match c {
            '\'' => {
                has = true;
                quoted = true;
                loop {
                    match chars.next() {
                        Some('\'') => break,
                        Some(ch) => cur.push(ch),
                        None => return None,
                    }
                }
            }
            '"' => {
                has = true;
                quoted = true;
                loop {
                    match chars.next() {
                        Some('"') => break,
                        Some('\\') => match chars.next() {
                            Some(n) => cur.push(n),
                            None => return None,
                        },
                        Some(ch @ '$') | Some(ch @ '`') => {
                            dynamic = true;
                            cur.push(ch);
                        }
                        Some(ch) => cur.push(ch),
                        None => return None,
                    }
                }
            }
            '\\' => match chars.next() {
                Some(ch) => {
                    has = true;
                    quoted = true;
                    cur.push(ch);
                }
                None => return None,
            },
            '$' | '`' => {
                has = true;
                dynamic = true;
                cur.push(c);
            }
            '~' if !has => {
                has = true;
                cur.push('~');
                if let Some(&n) = chars.peek() {
                    if n != '/' && !n.is_whitespace() {
                        leading_tilde_name = true;
                    }
                }
            }
            c if c.is_whitespace() => flush!(),
            ';' | '&' | '|' | '<' | '>' | '(' | ')' => {
                // 只有紧邻且未引用的数字才是 fd 前缀；`2 >x` / `'2'>x` 仍有参数 2。
                // 前一个重定向尚缺目标时，数字先充当它的操作数：`>1>/dev/null`。
                let fd = if matches!(c, '<' | '>')
                    && has
                    && !quoted
                    && cur.chars().all(|ch| ch.is_ascii_digit())
                    && tokens.last().is_none_or(|t| redirect_operator(t).is_none())
                {
                    has = false;
                    std::mem::take(&mut cur)
                } else {
                    String::new()
                };
                flush!();
                let mut op = fd;
                op.push(c);
                if let Some(&n) = chars.peek() {
                    if (c == '&' && n == '&')
                        || (c == '|' && n == '|')
                        || (c == '>' && n == '>')
                        || (c == '<' && n == '<')
                        || (c == '&' && n == '>')
                        || (matches!(c, '<' | '>') && n == '&')
                        || (c == '>' && n == '|')
                    {
                        op.push(n);
                        chars.next();
                    }
                }
                if op == "&>" && chars.peek() == Some(&'>') {
                    op.push(chars.next().unwrap());
                }
                tokens.push(Token {
                    text: op,
                    is_operator: true,
                    dynamic: false,
                });
            }
            c => {
                has = true;
                cur.push(c);
            }
        }
    }
    if has {
        tokens.push(Token {
            text: cur,
            is_operator: false,
            dynamic: dynamic || leading_tilde_name,
        });
    }
    Some(tokens)
}

/// 按 `;` `&&` `||` `|` `&` 切 segment（操作符本身不入段；重定向 `<>` 留段内）。
pub fn split_segments(tokens: &[Token]) -> Vec<Vec<Token>> {
    let mut segs: Vec<Vec<Token>> = vec![Vec::new()];
    for t in tokens {
        if t.is_operator && matches!(t.text.as_str(), ";" | "&&" | "||" | "|" | "&") {
            segs.push(Vec::new());
        } else {
            segs.last_mut().unwrap().push(t.clone());
        }
    }
    segs.into_iter().filter(|s| !s.is_empty()).collect()
}

/// 剥 wrapper 前缀（timeout/nohup/nice/env/stdbuf/time + 它们的 flag/数值/`A=B`）。
pub fn strip_wrappers(seg: &[Token]) -> &[Token] {
    let mut i = 0;
    while i < seg.len() {
        if is_grouping_token(&seg[i].text) {
            i += 1;
            continue;
        }
        let name = seg[i].text.as_str();
        if WRAPPERS.contains(&name) {
            i += 1;
            while i < seg.len() {
                let a = &seg[i].text;
                if is_grouping_token(a)
                    || a.starts_with('-')
                    || a.chars().all(|c| c.is_ascii_digit() || c == '.')
                    || a.contains('=')
                {
                    i += 1;
                } else {
                    break;
                }
            }
            continue;
        } else {
            break;
        }
    }
    &seg[i..]
}

/// 去掉可选 fd 前缀后，识别文件重定向或 fd 复制操作符。
fn redirect_operator(tok: &Token) -> Option<&str> {
    if !tok.is_operator {
        return None;
    }
    let op = tok.text.trim_start_matches(|c: char| c.is_ascii_digit());
    matches!(op, ">" | ">>" | ">|" | "<" | ">&" | "<&" | "&>" | "&>>").then_some(op)
}

/// fd 复制/关闭/移动不打开文件；动态目标仍交给路径检查保守拒绝。
fn is_fd_target(target: &Token) -> bool {
    let fd = target.text.strip_suffix('-').unwrap_or(&target.text);
    !target.dynamic
        && (target.text == "-" || (!fd.is_empty() && fd.chars().all(|c| c.is_ascii_digit())))
}

/// 精确匹配空设备；不豁免相似路径、变量展开或普通位置参数。
pub fn is_null_redirect_target(target: &Token) -> bool {
    !target.dynamic && target.text == "/dev/null"
}

/// 去掉重定向及其操作数，保留真正的命令参数（按位置，不按目标文本去重）。
pub fn without_redirects(seg: &[Token]) -> Vec<Token> {
    let mut words = Vec::new();
    let mut i = 0;
    while i < seg.len() {
        if redirect_operator(&seg[i]).is_some() {
            i += 1;
            if seg.get(i).is_some_and(|target| !target.is_operator) {
                i += 1;
            }
        } else {
            words.push(seg[i].clone());
            i += 1;
        }
    }
    words
}

/// 抽段内文件重定向目标；`2>&1` / `>&2` 等 fd 操作不产生文件目标。
pub fn extract_redirects(seg: &[Token]) -> Vec<(RedirOp, Token)> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < seg.len() {
        if let Some(operator) = redirect_operator(&seg[i]) {
            let op = match operator {
                ">>" | "&>>" => RedirOp::Append,
                "<" | "<&" => RedirOp::In,
                _ => RedirOp::Out,
            };
            if let Some(target) = seg.get(i + 1) {
                if !target.is_operator {
                    if !matches!(operator, ">&" | "<&") || !is_fd_target(target) {
                        out.push((op, target.clone()));
                    }
                    i += 2;
                    continue;
                }
            }
        }
        i += 1;
    }
    out
}

/// `&>`/`&>>` 是双方言分歧点：bash 认成合并重定向操作符（本模块 tokenize 的默认读法），
/// dash/posix sh 认成 `&`（后台分隔符）+ `>`/`>>`（重定向）。两种真实 shell 都在线，
/// 危险扫描必须两种读法都过、任一危险即拒（fail-closed，见 dangerous_paths::dangerous_command_scan）。
pub fn has_ampersand_redirect(tokens: &[Token]) -> bool {
    tokens
        .iter()
        .any(|t| t.is_operator && matches!(t.text.as_str(), "&>" | "&>>"))
}

/// 由 bash 读法派生 posix 读法：把 `&>`/`&>>` 操作符 token 拆成 `&` 分隔符 + `>`/`>>` 重定向 token。
pub fn derive_posix_ampersand(tokens: &[Token]) -> Vec<Token> {
    let mut out = Vec::with_capacity(tokens.len() + 1);
    for t in tokens {
        if t.is_operator && matches!(t.text.as_str(), "&>" | "&>>") {
            out.push(Token {
                text: "&".to_string(),
                is_operator: true,
                dynamic: false,
            });
            let redir = if t.text == "&>>" { ">>" } else { ">" };
            out.push(Token {
                text: redir.to_string(),
                is_operator: true,
                dynamic: false,
            });
        } else {
            out.push(t.clone());
        }
    }
    out
}

/// 含 process substitution `<(` / `>(` / `=(`（quote-aware·单引号内不算）。
pub fn has_process_substitution(cmd: &str) -> bool {
    let mut in_single = false;
    let mut in_double = false;
    let mut prev: Option<char> = None;
    for c in cmd.chars() {
        match c {
            '\'' if !in_double => in_single = !in_single,
            '"' if !in_single => in_double = !in_double,
            '(' if !in_single && !in_double => {
                if matches!(prev, Some('<') | Some('>') | Some('=')) {
                    return true;
                }
            }
            _ => {}
        }
        prev = Some(c);
    }
    false
}

/// 含 `cd` 段 + 之后有写命令/输出重定向段（路径按旧 cwd 校验、按新 cwd 执行的绕过）。
pub fn has_cd_then_mutation(segments: &[Vec<Token>]) -> bool {
    let mut saw_cd = false;
    for seg in segments {
        let words = without_redirects(seg);
        let real = strip_wrappers(&words);
        let base = real.first().map(|t| t.text.as_str()).unwrap_or("");
        if saw_cd {
            let has_out_redir = extract_redirects(seg).iter().any(|(op, target)| {
                matches!(op, RedirOp::Out | RedirOp::Append) && !is_null_redirect_target(target)
            });
            if WRITE_COMMANDS.contains(&base) || has_out_redir {
                return true;
            }
        }
        if base == "cd" {
            saw_cd = true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(seg: &[Token]) -> Vec<String> {
        seg.iter().map(|t| t.text.clone()).collect()
    }

    #[test]
    fn tokenize_handles_quotes_and_returns_none_on_unbalanced() {
        let t = tokenize("echo 'a b' \"c d\"").unwrap();
        let words: Vec<_> = t
            .iter()
            .filter(|x| !x.is_operator)
            .map(|x| x.text.clone())
            .collect();
        assert_eq!(words, vec!["echo", "a b", "c d"]);
        assert!(tokenize("echo 'unterminated").is_none());
        assert!(tokenize("echo \\").is_none());
    }

    #[test]
    fn tokenize_marks_dynamic_expansion_outside_single_quotes() {
        let t = tokenize("rm $HOME/x").unwrap();
        assert!(t.iter().any(|x| x.text.contains("HOME") && x.dynamic));
        let t2 = tokenize("rm '$HOME'").unwrap();
        assert!(t2.iter().filter(|x| !x.is_operator).all(|x| !x.dynamic));
        assert!(tokenize("cat ~root/.ssh/id_rsa")
            .unwrap()
            .iter()
            .any(|x| x.dynamic));
    }

    #[test]
    fn split_segments_breaks_on_operators() {
        let t = tokenize("cd .git && rm config | tee x").unwrap();
        let segs = split_segments(&t);
        assert_eq!(segs.len(), 3);
        assert_eq!(texts(&segs[0]), vec!["cd", ".git"]);
        assert_eq!(texts(&segs[1]), vec!["rm", "config"]);
    }

    #[test]
    fn strip_wrappers_peels_timeout_nohup_nice() {
        let t = tokenize("timeout 5 rm x").unwrap();
        let segs = split_segments(&t);
        assert_eq!(texts(strip_wrappers(&segs[0])), vec!["rm", "x"]);
        let t2 = tokenize("nohup nice -n 5 rm x").unwrap();
        let segs2 = split_segments(&t2);
        assert_eq!(strip_wrappers(&segs2[0])[0].text, "rm");
    }

    #[test]
    fn extract_redirects_finds_targets() {
        let t = tokenize("echo x > out.txt 2>> err.log < in.txt").unwrap();
        let segs = split_segments(&t);
        let r = extract_redirects(&segs[0]);
        let targets: Vec<_> = r.iter().map(|(_, tk)| tk.text.clone()).collect();
        assert!(targets.contains(&"out.txt".to_string()));
        assert!(targets.contains(&"err.log".to_string()));
        assert!(targets.contains(&"in.txt".to_string()));
    }

    #[test]
    fn fd_redirects_keep_files_and_skip_descriptor_operations() {
        for (syntax, expected) in [
            ("2>err", Some((RedirOp::Out, "err"))),
            ("2>>err", Some((RedirOp::Append, "err"))),
            ("1>out", Some((RedirOp::Out, "out"))),
            ("12>out", Some((RedirOp::Out, "out"))),
            ("&>out", Some((RedirOp::Out, "out"))),
            ("&>>out", Some((RedirOp::Append, "out"))),
            (">&out", Some((RedirOp::Out, "out"))),
            ("3<input", Some((RedirOp::In, "input"))),
            ("2>1", Some((RedirOp::Out, "1"))),
            ("2>&1", None),
            (">&2", None),
            ("2>& 1", None),
            ("2>&'1'", None),
            ("2>&-", None),
            ("2>&1-", None),
            ("0<&3", None),
        ] {
            let tokens = tokenize(&format!("cmd {syntax} | tail")).unwrap();
            let segments = split_segments(&tokens);
            assert_eq!(segments.len(), 2, "{syntax}");
            let redirects = extract_redirects(&segments[0]);
            let actual: Vec<_> = redirects
                .iter()
                .map(|(op, target)| (*op, target.text.as_str()))
                .collect();
            assert_eq!(actual, expected.into_iter().collect::<Vec<_>>(), "{syntax}");
            assert_eq!(
                texts(&without_redirects(&segments[0])),
                vec!["cmd"],
                "{syntax}"
            );
        }
    }

    #[test]
    fn ampersand_redirect_bash_and_posix_dual_reading() {
        // bash 读法：`&>` 合并成重定向操作符、不切段——2 段（cmd &>out / tail）。
        let tokens = tokenize("cmd &>out | tail").unwrap();
        assert!(has_ampersand_redirect(&tokens));
        let segments = split_segments(&tokens);
        assert_eq!(segments.len(), 2, "bash reading: cmd &>out / tail");
        assert_eq!(texts(&segments[0]), vec!["cmd", "&>", "out"]);
        assert_eq!(texts(&segments[1]), vec!["tail"]);
        let redirects = extract_redirects(&segments[0]);
        assert_eq!(
            redirects,
            vec![(
                RedirOp::Out,
                Token {
                    text: "out".to_string(),
                    is_operator: false,
                    dynamic: false,
                }
            )]
        );

        // posix 派生读法：`&` 分隔 + `>` 重定向——3 段（cmd / >out / tail）。
        let posix_tokens = derive_posix_ampersand(&tokens);
        let posix_segments = split_segments(&posix_tokens);
        assert_eq!(posix_segments.len(), 3, "posix reading: cmd / >out / tail");
        assert_eq!(texts(&posix_segments[0]), vec!["cmd"]);
        assert_eq!(texts(&posix_segments[1]), vec![">", "out"]);
        assert_eq!(texts(&posix_segments[2]), vec!["tail"]);
    }

    #[test]
    fn noclobber_operator_is_parsed_as_output_redirect() {
        let tokens = tokenize("cmd >| out.txt | tail").unwrap();
        let segments = split_segments(&tokens);
        assert_eq!(segments.len(), 2);
        let redirects = extract_redirects(&segments[0]);
        assert_eq!(redirects.len(), 1);
        assert_eq!(redirects[0].0, RedirOp::Out);
        assert_eq!(redirects[0].1.text, "out.txt");
    }

    #[test]
    fn fd_prefix_requires_unquoted_adjacent_digits() {
        for command in ["cmd 2 >out", "cmd '2'>out", "cmd \"2\">out", r"cmd \2>out"] {
            let tokens = tokenize(command).unwrap();
            assert_eq!(
                texts(&without_redirects(&tokens)),
                vec!["cmd", "2"],
                "{command}"
            );
        }
        let tokens = tokenize("cmd 2>out").unwrap();
        assert_eq!(texts(&tokens), vec!["cmd", "2>", "out"]);
        assert!(tokens[1].is_operator);
        let tokens = tokenize("cmd '2>&1' /dev/null 2>/dev/null").unwrap();
        assert_eq!(
            texts(&without_redirects(&tokens)),
            vec!["cmd", "2>&1", "/dev/null"]
        );
    }

    #[test]
    fn numeric_redirect_operand_is_not_the_next_fd_prefix() {
        for command in ["cd sub && cmd >1>/dev/null", "cd sub && cmd 2>1>/dev/null"] {
            let segments = split_segments(&tokenize(command).unwrap());
            assert!(has_cd_then_mutation(&segments), "{command}");
            let targets: Vec<_> = extract_redirects(&segments[1])
                .into_iter()
                .map(|(_, token)| token.text)
                .collect();
            assert_eq!(targets, vec!["1", "/dev/null"], "{command}");
        }
        let tokens = tokenize("cmd 2>&1>/dev/null").unwrap();
        let redirects = extract_redirects(&tokens);
        assert_eq!(redirects.len(), 1);
        assert_eq!(redirects[0].1.text, "/dev/null");
    }

    #[test]
    fn process_substitution_detected() {
        assert!(has_process_substitution("echo x > >(tee .git/config)"));
        assert!(has_process_substitution("diff <(a) <(b)"));
        assert!(!has_process_substitution("echo (literal) text"));
        assert!(!has_process_substitution("echo '>(x)'"));
    }

    #[test]
    fn cd_then_mutation_detected() {
        let t = tokenize("cd .git && echo x > config").unwrap();
        assert!(has_cd_then_mutation(&split_segments(&t)));
        let t2 = tokenize("cd sub && rm y").unwrap();
        assert!(has_cd_then_mutation(&split_segments(&t2)));
        let t3 = tokenize("cd sub && cat y").unwrap();
        assert!(!has_cd_then_mutation(&split_segments(&t3)));
        let t4 = tokenize("( cd .. ; rm -rf x )").unwrap();
        assert!(has_cd_then_mutation(&split_segments(&t4)));
        let t5 = tokenize("{ cd .. ; rm -rf x ; }").unwrap();
        assert!(has_cd_then_mutation(&split_segments(&t5)));
        let t6 = tokenize("rm y").unwrap();
        assert!(!has_cd_then_mutation(&split_segments(&t6)));
    }
}

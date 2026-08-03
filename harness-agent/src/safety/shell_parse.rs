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

/// 一个 shell token：text=去引号字面值；is_operator=分隔/重定向操作符；dynamic=含未展开 expansion。
#[derive(Debug, Clone)]
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
                dynamic = false;
                leading_tilde_name = false;
            }
        };
    }

    while let Some(c) = chars.next() {
        match c {
            '\'' => {
                has = true;
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
                flush!();
                let mut op = String::new();
                op.push(c);
                if let Some(&n) = chars.peek() {
                    if (c == '&' && n == '&')
                        || (c == '|' && n == '|')
                        || (c == '>' && n == '>')
                        || (c == '<' && n == '<')
                    {
                        op.push(n);
                        chars.next();
                    }
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
        let name = seg[i].text.as_str();
        if WRAPPERS.contains(&name) {
            i += 1;
            while i < seg.len() {
                let a = &seg[i].text;
                if a.starts_with('-')
                    || a.chars().all(|c| c.is_ascii_digit() || c == '.')
                    || a.contains('=')
                {
                    i += 1;
                } else {
                    break;
                }
            }
        } else {
            break;
        }
    }
    &seg[i..]
}

/// 抽段内重定向目标（`>`/`>>`/`<` 后紧跟的非操作符 token）。
pub fn extract_redirects(seg: &[Token]) -> Vec<(RedirOp, Token)> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < seg.len() {
        if seg[i].is_operator {
            let op = match seg[i].text.as_str() {
                ">" => Some(RedirOp::Out),
                ">>" => Some(RedirOp::Append),
                "<" => Some(RedirOp::In),
                _ => None,
            };
            if let Some(op) = op {
                if let Some(target) = seg.get(i + 1) {
                    if !target.is_operator {
                        out.push((op, target.clone()));
                        i += 2;
                        continue;
                    }
                }
            }
        }
        i += 1;
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
        let real = strip_wrappers(seg);
        let base = real.first().map(|t| t.text.as_str()).unwrap_or("");
        if saw_cd {
            let has_out_redir = extract_redirects(seg)
                .iter()
                .any(|(op, _)| matches!(op, RedirOp::Out | RedirOp::Append));
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
        let t4 = tokenize("rm y").unwrap();
        assert!(!has_cd_then_mutation(&split_segments(&t4)));
    }
}

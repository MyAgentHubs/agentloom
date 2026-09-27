#!/usr/bin/env python3
"""Report frequently changed production source files without enforcing a gate."""

import argparse
from datetime import datetime, timezone
import json
from pathlib import Path, PurePosixPath
import os
import re
import subprocess
import sys


SCRIPT_DIR = Path(__file__).resolve().parent
sys.path.insert(0, str(SCRIPT_DIR))

from check_file_size import GateError, category, count_lines, in_scope  # noqa: E402
import check_file_size  # noqa: E402


EXCLUDED_RULES = [
    "合并提交：通过 git log --no-merges 排除。",
    "标题匹配 ^(docs|style|refactor|chore)(\\(|:)（大小写不敏感）的提交整条排除。",
    "单个提交触及的文件数 > 40 时，作为批量提交整条排除。",
    "某文件是相似度 >= 90%（内容变动 < 10%）的重命名目标时，该提交不计入该文件的 churn 或 cochange。",
]
NOTE = "本榜只用于提名，拆不拆由职责体检决定（例如 i18n 文案表改得勤不等于职责混杂）"
SPLIT_NOTE = (
    "路径历史包含拆分前的内容，刚被拆过的外壳文件（如 lib.rs、i18nMessages.ts）的改动次数反映的是旧内容，"
    "评审时要结合当前行数与职责判断。"
)
SCORE_NOTE = "score 为经验排序，非结论。"
FIX_NOTE = "修 bug 提交按标题任意位置包含 fix、hotfix、修复、bug、回归或 regression 判定。"
SUBJECT_EXCLUDE_RE = re.compile(r"^(docs|style|refactor|chore)(\(|:)", re.IGNORECASE)
FIX_RE = re.compile(r"(fix|hotfix)|修复|bug|回归|regression", re.IGNORECASE)
LOG_MARKER = "@@HOTSPOT@@"


class ReportError(Exception):
    pass


def cap_note(has_block_cap):
    if has_block_cap:
        return (
            "本仓 check_file_size.py 已提供 block_cap() 拦截线函数，“提醒·拦截”列区分“超提醒”"
            "（超过提醒线但未超拦截线）与“超拦截”（超过拦截线）两种状态。"
        )
    return (
        "本仓 check_file_size.py 当前只有单一行数阈值常量，暂无独立硬拦截线，"
        "“提醒·拦截”列以该阈值判定。"
    )


def git(root, *args, check=True):
    env = {key: value for key, value in os.environ.items() if not key.startswith("GIT_")}
    result = subprocess.run(
        ["git", "--no-replace-objects", "--no-pager", "-C", str(root), *args],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        env=env,
        check=False,
    )
    if check and result.returncode:
        detail = result.stderr.decode("utf-8", errors="replace").strip()
        raise ReportError(f"git {args[0]} failed (exit {result.returncode}): {detail}")
    return result


def repository_root(start):
    result = git(start, "rev-parse", "--show-toplevel")
    return Path(result.stdout.decode("utf-8").strip()).resolve()


def resolve_until(root, value):
    verified = git(root, "rev-parse", "--verify", "--quiet", f"{value}^{{commit}}", check=False)
    if verified.returncode == 0:
        commit = git(root, "rev-list", "-1", value).stdout.decode("ascii").strip()
        epoch_text = git(root, "show", "-s", "--format=%ct", commit).stdout.decode("ascii").strip()
    else:
        parsed = git(root, "rev-parse", f"--since={value}").stdout.decode("ascii").strip()
        match = re.fullmatch(r"--max-age=(\d+)", parsed)
        if not match:
            raise ReportError(f"cannot parse --until value: {value}")
        epoch_text = match.group(1)
    epoch = int(epoch_text)
    display = datetime.fromtimestamp(epoch, timezone.utc).isoformat().replace("+00:00", "Z")
    return epoch, display


def current_files(root):
    output = git(root, "ls-tree", "-r", "--name-only", "HEAD").stdout
    files = []
    for raw_path in output.splitlines():
        try:
            path_text = raw_path.decode("utf-8")
        except UnicodeDecodeError:
            continue
        path = PurePosixPath(path_text)
        if not in_scope(path):
            continue
        entry = git(root, "ls-tree", "HEAD", "--", path_text, check=False)
        if entry.returncode or not entry.stdout.startswith((b"100644 ", b"100755 ")):
            continue
        blob = git(root, "show", f"HEAD:{path_text}", check=False)
        if blob.returncode:
            continue
        data = blob.stdout
        try:
            lines = count_lines(data, path_text)
            label, cap = category(path, data)
        except GateError:
            continue
        if "测试" not in label:
            files.append((path_text, data, lines, cap, label))
    return files


def parse_file_history(root, path, since_epoch, until_epoch):
    result = git(
        root,
        "log",
        "--no-merges",
        "-M90%",
        "--follow",
        f"--since=@{since_epoch}",
        f"--until=@{until_epoch}",
        f"--format={LOG_MARKER}%H%x09%s",
        "--name-status",
        "--",
        path,
    )
    commits = []
    current = None
    for line in result.stdout.decode("utf-8", errors="replace").splitlines():
        if line.startswith(LOG_MARKER):
            if current is not None:
                commits.append(current)
            header = line[len(LOG_MARKER):]
            sha, _, subject = header.partition("\t")
            current = {"sha": sha, "subject": subject, "statuses": []}
        elif current is not None and line.strip():
            current["statuses"].append(line.split("\t"))
    if current is not None:
        commits.append(current)
    return commits


def is_high_similarity_rename(statuses):
    for fields in statuses:
        status = fields[0]
        if status.startswith("R") and status[1:].isdigit() and int(status[1:]) >= 90:
            return True
    return False


def commit_paths(root, sha, cache):
    if sha not in cache:
        output = git(root, "show", "--no-color", "--name-only", "--pretty=format:", sha).stdout
        cache[sha] = [
            line.decode("utf-8", errors="replace")
            for line in output.splitlines()
            if line.strip()
        ]
    return cache[sha]


def directory_key(path):
    parts = path.split("/")
    return "/".join(parts[:2])


def metrics_for_file(root, path, lines, cap, label, since_epoch, until_epoch, path_cache):
    accepted = []
    for commit in parse_file_history(root, path, since_epoch, until_epoch):
        if SUBJECT_EXCLUDE_RE.search(commit["subject"]):
            continue
        paths = commit_paths(root, commit["sha"], path_cache)
        if len(paths) > 40:
            continue
        if is_high_similarity_rename(commit["statuses"]):
            continue
        accepted.append((commit, paths))

    churn = len(accepted)
    directory_counts = [len({directory_key(item) for item in paths}) for _, paths in accepted]
    cochange = sum(directory_counts) / churn if churn else 0.0
    fixes = sum(bool(FIX_RE.search(commit["subject"])) for commit, _ in accepted)
    block_cap_fn = getattr(check_file_size, "block_cap", None)
    block_cap_value = block_cap_fn(label, cap) if block_cap_fn is not None else None
    if block_cap_value is None:
        status = "超" if lines > cap else "未超"
    elif lines > block_cap_value:
        status = "超拦截"
    elif lines > cap:
        status = "超提醒"
    else:
        status = "未超"
    return {
        "path": path,
        "lines": lines,
        "cap": cap,
        "over_cap": lines > cap,
        "block_cap": block_cap_value,
        "status": status,
        "churn": churn,
        "cochange": cochange,
        "fixes": fixes,
        "score": churn * (1 + cochange),
    }


def build_report(root, days, top_k, until_value):
    until_epoch, until_display = resolve_until(root, until_value)
    since_epoch = until_epoch - days * 24 * 60 * 60
    since_display = datetime.fromtimestamp(since_epoch, timezone.utc).isoformat().replace("+00:00", "Z")
    path_cache = {}
    rows = [
        metrics_for_file(root, path, lines, cap, label, since_epoch, until_epoch, path_cache)
        for path, _data, lines, cap, label in current_files(root)
    ]
    rows.sort(key=lambda row: (-row["score"], row["path"]))
    has_block_cap = getattr(check_file_size, "block_cap", None) is not None
    return {
        "window": {"since": since_display, "until": until_display},
        "top_k": top_k,
        "excluded_rules": EXCLUDED_RULES,
        "files": rows[:top_k],
        "note": NOTE,
        "split_note": SPLIT_NOTE,
        "cap_note": cap_note(has_block_cap),
    }


def markdown(report):
    lines = [
        "| 文件 | 行数 | 提醒·拦截 | 改动次数 | 同改面 | 修 bug 提交 | 分数 |",
        "| --- | ---: | :---: | ---: | ---: | ---: | ---: |",
    ]
    for row in report["files"]:
        path = row["path"].replace("|", "\\|")
        lines.append(
            f"| {path} | {row['lines']} | {row['status']} | "
            f"{row['churn']} | {row['cochange']:.2f} | {row['fixes']} | {row['score']:.2f} |"
        )
    window = report["window"]
    lines.extend(["", f"- 窗口：{window['since']} 至 {window['until']}。"])
    lines.extend(f"- 排除规则 {index}：{rule}" for index, rule in enumerate(EXCLUDED_RULES, 1))
    lines.extend([
        f"- {report['note']}",
        f"- {report['split_note']}",
        f"- {report['cap_note']}",
        f"- {FIX_NOTE}",
        f"- {SCORE_NOTE}",
    ])
    return "\n".join(lines)


def parse_args(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--days", type=int, default=90)
    parser.add_argument("--top", type=int, default=20)
    parser.add_argument("--until", default="HEAD")
    parser.add_argument("--format", choices=("md", "json"), default="md")
    parser.add_argument("--repo", type=Path, default=Path.cwd())
    args = parser.parse_args(argv)
    if args.days < 0:
        parser.error("--days must be non-negative")
    if args.top < 0:
        parser.error("--top must be non-negative")
    return args


def main(argv=None):
    args = parse_args(argv)
    root = repository_root(args.repo)
    report = build_report(root, args.days, args.top, args.until)
    if args.format == "json":
        print(json.dumps(report, ensure_ascii=False, indent=2))
    else:
        print(markdown(report))
    return 0


if __name__ == "__main__":
    sys.exit(main())

#!/usr/bin/env python3
"""Read-only local feedback. CI with a freshly fetched baseline is authoritative.

No arguments, environment overrides, baseline files, writes, or rename detection.
The checked worktree is anchored to this script, not the caller's cwd.
"""

import io
import json
import os
from pathlib import Path, PurePosixPath
import re
import stat
import subprocess
import sys


BASELINE_REFS = ("refs/remotes/origin/master", "refs/remotes/origin/main")
SOURCE_EXTENSIONS = {".rs", ".ts", ".tsx", ".js", ".mts", ".css"}
SCAN_ROOTS = {
    "app/src": {".ts", ".tsx", ".js", ".mts", ".css"},
    "app/src-tauri/src": {".rs"},
    "app/src-tauri/tests": {".rs"},
    "harness-agent/src": {".rs"},
    "harness-agent/tests": {".rs"},
    "remote-web/src": {".ts", ".tsx", ".js", ".mts", ".css"},
    "remote-relay/src": {".ts", ".tsx", ".js", ".mts", ".css"},
    "remote-relay/test": {".ts", ".tsx", ".js", ".mts", ".css"},
}
SCAN_FILES = {
    "app/vite.config.ts", "app/src-tauri/build.rs",
    "remote-web/vite.config.ts", "remote-web/vitest.config.ts",
}
# Only the public snapshot may omit these roots. A tracked or historical root
# still must exist; deleting a private worktree directory must fail closed.
SNAPSHOT_OPTIONAL_ROOTS = {"remote-web/src", "remote-relay/src", "remote-relay/test"}
EXCLUDED_DIRS = {"node_modules", "target", ".git"}
# Directory-level policy decisions, never per-file numeric allowances.
COVERAGE_EXEMPT_DIRS = {
    "harness-agent/evals": "Benchmark fixtures must remain byte-for-byte stable.",
}
LINE_BREAK = re.compile(r"\r\n|[\n\r\u2028\u2029]")


class GateError(Exception):
    pass


def git(root, *args, input_bytes=None, missing_ok=False):
    # GIT_DIR / GIT_WORK_TREE / GIT_CONFIG_* must not redirect the baseline.
    env = {key: value for key, value in os.environ.items() if not key.startswith("GIT_")}
    result = subprocess.run(
        ["git", "--no-replace-objects", "--no-pager", "-C", str(root), *args],
        input=input_bytes,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        env=env,
        check=False,
    )
    # Quiet rev-parse reports absence as 1 with no diagnostic. A broken ref can
    # also return 1, but with a warning: that must not permit a main fallback.
    if missing_ok and result.returncode == 1 and not result.stderr:
        return None
    if result.returncode:
        detail = result.stderr.decode("utf-8", errors="replace").strip()
        raise GateError(f"git {args[0]} 失败（退出码 {result.returncode}）：{detail}")
    return result.stdout


def in_scope(path):
    if EXCLUDED_DIRS.intersection(path.parts[:-1]):
        return False
    return path.as_posix() in SCAN_FILES or any(
        path.as_posix().startswith(prefix + "/") and path.suffix in extensions
        for prefix, extensions in SCAN_ROOTS.items()
    )


def coverage_exempt(path):
    return any(path.as_posix().startswith(prefix + "/") for prefix in COVERAGE_EXEMPT_DIRS)


def tracked_sources(root):
    paths = git(root, "ls-files", "-z", "--", *["*" + ext for ext in sorted(SOURCE_EXTENSIONS)])
    return [PurePosixPath(os.fsdecode(path)) for path in paths.split(b"\0") if path]


def check_coverage(root):
    uncovered = sorted(str(path) for path in tracked_sources(root)
                       if not in_scope(path) and not coverage_exempt(path))
    if uncovered:
        raise GateError("已跟踪源码未纳入扫描或显式目录豁免：" + json.dumps(uncovered, ensure_ascii=False))


def count_lines(data, source):
    """Strict UTF-8; CRLF is one break, LF/CR/LS/PS and a final fragment count.

    The same conservative rule applies to every language and historical blob.
    Empty files have zero lines; a terminating separator adds no phantom line.
    """
    try:
        value = data.decode("utf-8")
    except UnicodeDecodeError as error:
        raise GateError(f"非法 UTF-8：{source}（字节偏移 {error.start}）") from error
    count, end = 0, 0
    for match in LINE_BREAK.finditer(value):
        count += 1
        end = match.end()
    return count + int(end < len(value))


def starts_with_test_cfg(data):
    """Find the first code line, skipping blank lines and nested Rust comments."""
    if data.startswith(b"\xef\xbb\xbf"):
        data = data[3:]
    index, depth = 0, 0
    line = bytearray()
    while index < len(data):
        pair = data[index:index + 2]
        if data[index:index + 1] == b"\n":
            if line.strip():
                return line.strip() == b"#![cfg(test)]"
            line.clear()
            index += 1
        elif depth:
            if pair == b"/*":
                depth += 1
                index += 2
            elif pair == b"*/":
                depth -= 1
                index += 2
            else:
                index += 1
        elif pair == b"/*":
            depth = 1
            line.extend(b" ")  # Comments separate tokens; never concatenate them.
            index += 2
        elif pair == b"//":
            end = data.find(b"\n", index)
            index = len(data) if end == -1 else end
        else:
            line.append(data[index])
            index += 1
    return line.strip() == b"#![cfg(test)]"


def category(path, data):
    if path.suffix == ".rs":
        if (
            any(path.as_posix().startswith(prefix + "/")
                for prefix in ("app/src-tauri/tests", "harness-agent/tests"))
            or path.name == "tests.rs"
            # This exemption depends on the compile gate: cfg(test) removes the
            # entire module from production, so production imports cannot build.
            or starts_with_test_cfg(data)
        ):
            return "Rust 测试文件", 1500
        # *_test.rs (especially conn_test.rs) is NOT a test-file convention.
        return "Rust 普通源文件", 800
    if path.suffix == ".css":
        return "CSS", 800
    if path.name.endswith(
        (".test.ts", ".test.tsx", ".spec.ts", ".spec.tsx")
    ):
        return "前端测试文件", 1500
    return "前端生产文件", 500


def collect_current(root, commit=None):
    files = {}

    def walk_error(error):
        raise error  # os.walk otherwise silently ignores unreadable directories.

    def collect(path):
        if not stat.S_ISREG(path.lstat().st_mode):
            raise GateError(f"扫描文件必须是普通文件（不能是符号链接）：{path}")
        data = path.read_bytes()
        relative = path.relative_to(root).as_posix()
        actual = count_lines(data, relative)
        label, cap = category(PurePosixPath(relative), data)
        files[relative] = (actual, label, cap)

    for prefix, extensions in SCAN_ROOTS.items():
        if prefix in SNAPSHOT_OPTIONAL_ROOTS and not (root / prefix).exists():
            # lexists catches dangling symlinks too. Check both trees, not just
            # ls-files: an unstaged/staged deletion is not a public snapshot.
            absent_from_trees = commit is not None and not any(
                git(root, "ls-tree", "-z", ref, "--", prefix) for ref in ("HEAD", commit)
            )
            if not os.path.lexists(root / prefix) and absent_from_trees:
                print(f"公开快照未包含扫描根：{prefix}（HEAD 与基线均无此路径）", flush=True)
                continue
        directory = root
        for part in PurePosixPath(prefix).parts:
            directory /= part
            if not stat.S_ISDIR(directory.lstat().st_mode):
                raise GateError(f"扫描根目录必须是实际目录（不能是符号链接）：{directory}")
        for current, directories, names in os.walk(directory, onerror=walk_error):
            directories[:] = sorted(set(directories) - EXCLUDED_DIRS)
            for name in directories:
                child = Path(current) / name
                if not stat.S_ISDIR(child.lstat().st_mode):
                    raise GateError(f"扫描目录不能是符号链接：{child}")
            for name in sorted(names):
                path = Path(current) / name
                if path.suffix not in extensions:
                    continue
                collect(path)
    for relative in sorted(SCAN_FILES):
        path = root / relative
        if os.path.lexists(path):
            # Refuse symlinked ancestors for the additional configuration files.
            for parent in path.parents:
                if parent == root:
                    break
                if not stat.S_ISDIR(parent.lstat().st_mode):
                    raise GateError(f"扫描目录不能是符号链接：{parent}")
            collect(path)
    return files


def baseline_blobs(root, commit):
    entries = git(root, "ls-tree", "-r", "-z", "--full-tree", commit, "--", *SCAN_ROOTS, *sorted(SCAN_FILES))
    blobs = {}
    for entry in entries.split(b"\0"):
        if not entry:
            continue
        metadata, raw_path = entry.split(b"\t", 1)
        mode, kind, oid = metadata.split()
        path = PurePosixPath(os.fsdecode(raw_path))
        if not in_scope(path):
            continue
        if mode == b"120000" and kind == b"blob":
            # A historical link is not source content and grants no allowance.
            # Replacing/removing it is allowed; current links still fail closed.
            continue
        if kind != b"blob" or mode not in (b"100644", b"100755"):
            raise GateError(f"基线路径不是普通文件：{path}")
        blobs[path.as_posix()] = oid
    return blobs


def historical_lines(root, oids):
    """Read raw blobs by pinned object IDs; do not apply filters or textconv."""
    unique = sorted(set(oids))
    if not unique:
        return {}
    response = io.BytesIO(git(root, "cat-file", "--batch", input_bytes=b"\n".join(unique) + b"\n"))
    counts = {}
    for oid in unique:
        header = response.readline().split()
        if len(header) != 3 or header[0] != oid or header[1] != b"blob":
            raise GateError(f"无法读取基线 blob：{oid.decode('ascii')}")
        size = int(header[2])
        if size < 0:
            raise GateError("基线 blob 长度非法")
        data = response.read(size)
        if len(data) != size or response.read(1) != b"\n":
            raise GateError("基线 blob 数据不完整")
        counts[oid] = count_lines(data, "基线 blob " + oid.decode("ascii"))
    if response.read(1):
        raise GateError("基线 blob 响应包含非预期数据")
    return counts


def check(root):
    toplevel = os.fsdecode(git(root, "rev-parse", "--show-toplevel")).rstrip("\n")
    if Path(toplevel).resolve() != root:
        raise GateError("检查器必须位于被检查仓库根目录的 scripts/ 下")
    # Fall back only when a ref is absent, never when an existing ref is broken.
    selected = next((ref for ref in BASELINE_REFS
                     if git(root, "rev-parse", "--verify", "--quiet", ref, missing_ok=True) is not None), None)
    if selected is None:
        raise GateError("缺少基线 origin/master 或 origin/main；先显式 fetch 远端历史。"
                        "首次导入须先建立经审查的基线；禁止退回 HEAD 或跳过。")
    commit = git(root, "rev-parse", "--verify", selected + "^{commit}").strip().decode("ascii")
    print(f"基线：{selected.removeprefix('refs/remotes/')} ({commit})", flush=True)
    check_coverage(root)
    baseline = baseline_blobs(root, commit)
    files = collect_current(root, commit)
    # Validate UTF-8 on both sides even if the current file is below its cap.
    previous = historical_lines(root, [
        baseline[path] for path in files if path in baseline
    ])
    excess, debt = 0, 0
    violations = []
    for path, (actual, label, cap) in sorted(files.items()):
        prev = previous.get(baseline.get(path), 0)
        allowed = max(cap, prev)
        debt += max(0, actual - cap)
        excess += max(0, actual - allowed)
        if actual > allowed:
            source = "基线历史额度" if prev > cap else "硬上限"
            if path not in baseline:
                source += "（新文件）"
            violations.append(
                f"{json.dumps(path, ensure_ascii=False)} / {label} / {actual} / {allowed} / {source}"
            )
    print(f"扫描文件数：{len(files)}；超标总量：{excess} 行；债务总量：{debt} 行")
    if violations:
        print("FAIL：文件 / 类别 / 实际 / allowed / 来源")
        print("\n".join(violations))
        print("请拆分文件。新文件不享有历史额度；改名后的新路径同样按硬上限检查。")
        return 1
    print("PASS：文件大小门禁通过。")
    return 0


def main():
    if len(sys.argv) != 1:
        print("ERROR：检查器不接受命令行参数；基线固定按 origin/master、origin/main 选择，"
              "不支持环境变量覆盖。", file=sys.stderr)
        return 2
    try:
        return check(Path(__file__).resolve().parent.parent)
    except (GateError, OSError, ValueError) as error:
        print(f"ERROR：文件大小门禁无法完成，拒绝放行：{error}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    sys.exit(main())

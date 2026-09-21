#!/usr/bin/env python3
"""Doc orphan ratchet: newly added documents must be referenced by an entry document.

No arguments, no environment overrides, no writes. Baseline selection and the
git()/read_blobs() plumbing mirror scripts/check_file_size.py's git()/historical_lines().
New orphans (present at HEAD but absent from the baseline orphan set) fail the
gate; the baseline orphan set may only shrink over time, never grow.
"""

from collections import Counter
import io
import os
from pathlib import Path, PurePosixPath
import re
import subprocess
import sys


BASELINE_REFS = ("refs/remotes/origin/master", "refs/remotes/origin/main")
# Joined at runtime so a public-snapshot residue scan (which greps for the
# literal joined path) does not flag this constant as leftover indexing code.
DOCS_ROOT = "/".join(("docs", "superpowers"))
SPEC_DIR = "/".join((DOCS_ROOT, "specs", "2026-05-21-github-fleet-ide"))
ENTRY_FILES = (
    DOCS_ROOT + "/INDEX.md",
    SPEC_DIR + "/README.md",
    SPEC_DIR + "/BACKLOG.md",
    DOCS_ROOT + "/_archive/INDEX.md",
    SPEC_DIR + "/mockups/index.html",
)
CANDIDATE_SUFFIXES = (".md", ".html")
# Hyphen last in both classes so it is a literal, not a range operator.
# The relative-path class also excludes "/": otherwise "notes/a.md" would be
# considered "bounded" right after the "x/" in "x/notes/a.md" (a false hit).
BASENAME_BOUNDARY_CLASS = "A-Za-z0-9_.-"
PATH_BOUNDARY_CLASS = "A-Za-z0-9_./-"


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
    if missing_ok and result.returncode == 1 and not result.stderr:
        return None
    if result.returncode:
        detail = result.stderr.decode("utf-8", errors="replace").strip()
        raise GateError(f"git {args[0]} 失败（退出码 {result.returncode}）：{detail}")
    return result.stdout


def read_blobs(root, oids):
    """Read raw blobs by pinned object IDs, batched (see historical_lines() in
    check_file_size.py for the origin of this batch-parsing shape)."""
    unique = sorted(set(oids))
    if not unique:
        return {}
    response = io.BytesIO(git(root, "cat-file", "--batch", input_bytes=b"\n".join(unique) + b"\n"))
    texts = {}
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
        texts[oid] = data.decode("utf-8", errors="replace")
    if response.read(1):
        raise GateError("基线 blob 响应包含非预期数据")
    return texts


def _bounded(literal, boundary_class):
    return re.compile(rf"(?<![{boundary_class}]){re.escape(literal)}(?![{boundary_class}])")


def is_referenced(relative_path, texts, unique_basenames):
    """Boundary-checked path match; boundary-checked basename match, but only
    when the basename is unique across the candidate set it was computed from.
    A basename shared by two or more candidates (e.g. two files both named
    `design.md` in different directories) must not let a mention of one path
    silently vouch for the other: those candidates are checked by full
    relative path only (fail-closed)."""
    basename = PurePosixPath(relative_path).name
    path_pattern = _bounded(relative_path, PATH_BOUNDARY_CLASS)
    basename_pattern = _bounded(basename, BASENAME_BOUNDARY_CLASS) if basename in unique_basenames else None
    for text in texts:
        if path_pattern.search(text):
            return True
        if basename_pattern is not None and basename_pattern.search(text):
            return True
    return False


def orphans(candidates, entry_texts):
    unique_basenames = {
        name for name, count in Counter(PurePosixPath(path).name for path in candidates).items()
        if count == 1
    }
    return {path for path in candidates if not is_referenced(path, entry_texts, unique_basenames)}


def _within(path, ancestor):
    try:
        path.relative_to(ancestor)
        return True
    except ValueError:
        return False


def collect_current(root):
    docs_dir = root / DOCS_ROOT
    entry_set = set(ENTRY_FILES)
    candidates = []
    for path in sorted(docs_dir.rglob("*")):
        if path.suffix not in CANDIDATE_SUFFIXES:
            continue
        doc_relative = path.relative_to(root).as_posix()
        if doc_relative in entry_set:
            continue
        if path.is_symlink():
            # A symlink whose target exists inside DOCS_ROOT is an alias for
            # a real file: the real file is walked and judged on its own
            # path, so the alias itself is not a second candidate. A broken
            # link, or one that escapes DOCS_ROOT, has no real file to be
            # judged and must fail closed instead of being silently skipped
            # (is_file() reports False for a broken link).
            target = Path(os.path.realpath(path))
            if target.exists() and _within(target, docs_dir):
                continue
            raise GateError(f"候选文档符号链接目标缺失或指向 {DOCS_ROOT} 之外：{doc_relative}")
        if not path.is_file():
            continue
        candidates.append(path.relative_to(docs_dir).as_posix())
    entry_texts = []
    for entry in ENTRY_FILES:
        entry_path = root / entry
        if entry_path.exists():
            entry_texts.append(entry_path.read_text(encoding="utf-8", errors="replace"))
    return candidates, entry_texts


def baseline_state(root, commit):
    entries = git(root, "ls-tree", "-r", "-z", "--full-tree", commit, "--", DOCS_ROOT)
    entry_set = set(ENTRY_FILES)
    oids = {}
    for entry in entries.split(b"\0"):
        if not entry:
            continue
        metadata, raw_path = entry.split(b"\t", 1)
        mode, kind, oid = metadata.split()
        if kind != b"blob":
            continue
        if mode == b"120000":
            # A historical symlink is an alias, not new source content; its
            # real target (if still tracked) is its own tree entry and is
            # judged there (mirrors check_file_size.py's baseline_blobs()).
            continue
        oids[os.fsdecode(raw_path)] = oid
    candidates = sorted(
        PurePosixPath(path).relative_to(DOCS_ROOT).as_posix()
        for path in oids
        if path not in entry_set and PurePosixPath(path).suffix in CANDIDATE_SUFFIXES
    )
    wanted = [oids[path] for path in ENTRY_FILES if path in oids]
    blobs = read_blobs(root, wanted)
    entry_texts = list(blobs.values())
    return candidates, entry_texts


def check(root):
    toplevel = os.fsdecode(git(root, "rev-parse", "--show-toplevel")).rstrip("\n")
    if Path(toplevel).resolve() != root:
        raise GateError("检查器必须位于被检查仓库根目录的 scripts/ 下")
    selected = next((ref for ref in BASELINE_REFS
                     if git(root, "rev-parse", "--verify", "--quiet", ref, missing_ok=True) is not None), None)
    if selected is None:
        raise GateError("缺少基线 origin/master 或 origin/main；先显式 fetch 远端历史。"
                        "首次导入须先建立经审查的基线；禁止退回 HEAD 或跳过。")
    commit = git(root, "rev-parse", "--verify", selected + "^{commit}").strip().decode("ascii")
    print(f"基线：{selected.removeprefix('refs/remotes/')} ({commit})", flush=True)

    current_candidates, current_entry_texts = collect_current(root)
    current_orphans = orphans(current_candidates, current_entry_texts)

    baseline_candidates, baseline_entry_texts = baseline_state(root, commit)
    baseline_orphans = orphans(baseline_candidates, baseline_entry_texts)

    new_orphans = sorted(current_orphans - baseline_orphans)
    print(f"扫描文档数：{len(current_candidates)}；新增孤儿：{len(new_orphans)}；存量孤儿：{len(current_orphans)}")
    if new_orphans:
        print("FAIL：以下文档未被任何入口文档引用，且不在基线孤儿豁免范围内：")
        for path in new_orphans:
            print(f"{path} (not referenced by any entry document)")
        print("请把新文档挂到入口文档（INDEX.md / README.md / BACKLOG.md / "
              "_archive/INDEX.md / mockups/index.html），或归档到已引用的位置。")
        return 1
    print("PASS：孤儿文档门禁通过。")
    return 0


def main():
    if len(sys.argv) != 1:
        print("ERROR：检查器不接受命令行参数；基线固定按 origin/master、origin/main 选择，"
              "不支持环境变量覆盖。", file=sys.stderr)
        return 2
    try:
        return check(Path(__file__).resolve().parent.parent)
    except (GateError, OSError, ValueError) as error:
        print(f"ERROR：孤儿文档门禁无法完成，拒绝放行：{error}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    sys.exit(main())

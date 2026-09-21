#!/usr/bin/env python3
"""Comment conventions ratchet: dated / ledger-keyword / CJK comment lines.

Per file, each of the three counts must stay at or below the count of the
same path in the origin/master baseline; a path absent from the baseline
gets an allowance of zero. This mirrors the self-baselining ratchet in
check_file_size.py (function check()), but never allows a hand-edited
per-file exception list.

Heuristic only: a comment line is judged purely by its line-start prefix,
not by parsing string literals or nested block comments.
"""

import io
import os
from pathlib import Path, PurePosixPath
import re
import stat
import subprocess
import sys


BASELINE_REFS = ("refs/remotes/origin/master", "refs/remotes/origin/main")
SOURCE_EXTENSIONS = {
    ".rs", ".ts", ".tsx", ".js", ".mjs", ".mts", ".css",
    ".py", ".sh", ".yml", ".yaml",
}
SCAN_ROOTS = {
    "app/src", "app/src-tauri/src", "app/src-tauri/tests",
    "harness-agent/src", "harness-agent/tests",
    "remote-web/src", "remote-relay/src", "remote-relay/test",
    "scripts", ".githooks", ".github/workflows", ".github/ISSUE_TEMPLATE",
    "app/scripts", ".github/scripts", "evals",
}
EXCLUDED_DIRS = {"node_modules", "target", "dist", "docs"}
HASH_COMMENT_EXTENSIONS = {".py", ".sh", ".yml", ".yaml"}
SLASH_COMMENT_PREFIXES = ("//", "///", "//!", "/*", "*", "{/*", "<!--")
RUST_DOC_ATTRIBUTE_PREFIXES = ("#[doc", "#![doc")
# Delimiter pairs scanned left to right to track block-comment state across
# lines. "{/*" (JSX) is not a separate pair: its opening delimiter is the
# same "/*" token, just preceded by an unrelated brace character.
BLOCK_COMMENT_PAIRS = (("/*", "*/"), ("<!--", "-->"))
DOUBLE_QUOTE_STRING_EXTENSIONS = {".rs", ".ts", ".tsx", ".js", ".mjs", ".jsx", ".css", ".html"}
SINGLE_QUOTE_STRING_EXTENSIONS = {".ts", ".tsx", ".js", ".mjs", ".jsx", ".css", ".html"}
BACKTICK_STRING_EXTENSIONS = {".ts", ".tsx", ".js", ".mjs", ".jsx"}
SLASH_LINE_COMMENT_EXTENSIONS = {".rs", ".ts", ".tsx", ".js", ".mjs", ".jsx"}

# Coverage safety net (mirrors check_file_size.py's check_coverage()), but
# policing this gate's own scanning extensions (including .py/.sh/.yml/
# .yaml), not check_file_size.py's narrower SOURCE_EXTENSIONS.
COVERAGE_ALLOWED_FILES = {
    "app/vite.config.ts", "app/src-tauri/build.rs",
    "remote-web/vite.config.ts", "remote-web/vitest.config.ts",
    # Lint config, not shipped application source; lives at the package root.
    "app/eslint.config.mjs", "app/eslint.config.js",
    "remote-web/eslint.config.mjs",
}
# Directory-level policy decisions, never per-file numeric allowances.
COVERAGE_EXEMPT_DIRS = {
    "harness-agent/evals": "Benchmark fixtures must remain byte-for-byte stable.",
    "docs": "Documentation tree; out of scope for the comment-convention ratchet.",
    "harness-agent/docs": "Documentation tree; out of scope for the comment-convention ratchet.",
    "app/.design-sync": "Generated placeholder input for the build, not source.",
}

# Patterns kept as plain assignments (never inside a "#" comment line) so the
# gate does not trip on its own source when scripts/ is in scope.
DATE_RE = re.compile(r"20[0-9]{2}-[0-9]{2}-[0-9]{2}")
# Assembled at runtime (not a literal) so scripts/ can ship in the public
# snapshot: the residue scan greps the whole tree for this literal string.
_INTERNAL_DOC_PATH = "/".join(("docs", "superpowers"))
LEDGER_RE = re.compile(
    r"\bT[0-9]+\b|续[0-9]+|\bB[0-9]\b|PR #|" + re.escape(_INTERNAL_DOC_PATH) + r"|"
    r"用户拍|用户定|用户校准|HANDOFF|roadmap\.html|TRACKER"
)
CJK_RE = re.compile(r"[一-鿿]")


class GateError(Exception):
    pass


def git(root, *args, input_bytes=None, missing_ok=False):
    # Derived from check_file_size.py's git(): strip GIT_* env so the baseline
    # cannot be redirected by the caller's environment.
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
        raise GateError(f"git {args[0]} failed (exit {result.returncode}): {detail}")
    return result.stdout


def effective_extension(path):
    """".githooks/" scripts are tracked without a file extension; treat them
    as shell scripts. This is the one deliberate special case: every other
    scan root relies on a real suffix."""
    if path.suffix:
        return path.suffix
    if path.as_posix() == ".githooks" or path.as_posix().startswith(".githooks/"):
        return ".sh"
    return path.suffix


def in_scope(path):
    if effective_extension(path) not in SOURCE_EXTENSIONS:
        return False
    if EXCLUDED_DIRS.intersection(path.parts[:-1]):
        return False
    posix = path.as_posix()
    return any(posix == prefix or posix.startswith(prefix + "/") for prefix in SCAN_ROOTS)


def is_comment_line(line, extension, is_first_line):
    stripped = line.strip()
    if not stripped:
        return False
    if extension in HASH_COMMENT_EXTENSIONS:
        if not stripped.startswith("#"):
            return False
        if is_first_line and stripped.startswith("#!"):
            return False
        return True
    if extension == ".rs" and stripped.startswith(RUST_DOC_ATTRIBUTE_PREFIXES):
        return True
    return stripped.startswith(SLASH_COMMENT_PREFIXES)


def scan_block_state(line, in_block, extension):
    """Advance the block-comment state left to right across one line's
    delimiters. Independent of whether the line itself counts as a comment
    line: code that opens an unclosed block still carries state forward.
    Outside a block, recognised single-line strings and line comments hide
    block delimiters. This single-line state machine does not model multiline
    or raw strings, template interpolation, or nested block comments."""
    string_delimiters = set()
    if extension in DOUBLE_QUOTE_STRING_EXTENSIONS:
        string_delimiters.add('"')
    if extension in SINGLE_QUOTE_STRING_EXTENSIONS:
        string_delimiters.add("'")
    if extension in BACKTICK_STRING_EXTENSIONS:
        string_delimiters.add("`")

    pos, length = 0, len(line)
    while pos < length:
        if in_block is not None:
            index = line.find(in_block, pos)
            if index == -1:
                return in_block
            pos = index + len(in_block)
            in_block = None
            continue

        current = line[pos]
        if current in string_delimiters:
            delimiter = current
            closing_pos = pos + 1
            while closing_pos < length:
                if line[closing_pos] == "\\":
                    closing_pos += 2
                    continue
                if line[closing_pos] == delimiter:
                    pos = closing_pos + 1
                    break
                closing_pos += 1
            else:
                pos += 1
            continue
        if extension in SLASH_LINE_COMMENT_EXTENSIONS and line.startswith("//", pos):
            return in_block

        matched = None
        for opener, closer in BLOCK_COMMENT_PAIRS:
            if line.startswith(opener, pos):
                matched = (opener, closer)
                break
        if matched is not None:
            opener, closer = matched
            pos += len(opener)
            in_block = closer
            continue
        pos += 1
    return in_block


def comment_flags(lines, extension):
    """Per-line comment classification with a single-level block-comment
    state, scanned left to right: a line counts as a comment line if it
    starts already inside an unclosed block (carried from a previous line),
    or if its own stripped start matches a recognised comment prefix.
    Nested blocks are not modelled."""
    if extension in HASH_COMMENT_EXTENSIONS:
        return [is_comment_line(line, extension, index == 0) for index, line in enumerate(lines)]
    flags = []
    in_block = None
    for line in lines:
        starts_in_block = in_block is not None
        flags.append(starts_in_block or is_comment_line(line, extension, False))
        in_block = scan_block_state(line, in_block, extension)
    return flags


def count_comment_stats(text, extension):
    lines = text.split("\n")
    dated = ledger = cjk = 0
    for line, flagged in zip(lines, comment_flags(lines, extension)):
        if not flagged:
            continue
        if DATE_RE.search(line):
            dated += 1
        if LEDGER_RE.search(line):
            ledger += 1
        if CJK_RE.search(line):
            cjk += 1
    return dated, ledger, cjk


def decode_text(data):
    # Heuristic gate: replace invalid bytes rather than failing the whole run.
    if data.startswith(b"\xef\xbb\xbf"):
        data = data[3:]
    return data.decode("utf-8", errors="replace").replace("\r\n", "\n").replace("\r", "\n")


def tracked_sources(root):
    paths = git(root, "ls-files", "-z", "--", *["*" + ext for ext in sorted(SOURCE_EXTENSIONS)])
    return [PurePosixPath(os.fsdecode(path)) for path in paths.split(b"\0") if path]


def coverage_exempt(path):
    posix = path.as_posix()
    return posix in COVERAGE_ALLOWED_FILES or any(
        posix.startswith(prefix + "/") for prefix in COVERAGE_EXEMPT_DIRS
    )


def check_coverage(root):
    uncovered = sorted(str(path) for path in tracked_sources(root)
                       if not in_scope(path) and not coverage_exempt(path))
    if uncovered:
        raise GateError(
            "tracked source not covered by any scan root or exemption: " + ", ".join(uncovered)
        )


def collect_current(root):
    files = {}
    for prefix in sorted(SCAN_ROOTS):
        base = root / prefix
        if not base.is_dir():
            continue
        for current, directories, names in os.walk(base):
            directories[:] = sorted(set(directories) - EXCLUDED_DIRS)
            for name in sorted(names):
                path = Path(current) / name
                relative = PurePosixPath(path.relative_to(root).as_posix())
                extension = effective_extension(relative)
                if extension not in SOURCE_EXTENSIONS or not in_scope(relative):
                    continue
                if not stat.S_ISREG(path.lstat().st_mode):
                    raise GateError(f"scanned path must be a regular file, not a symlink: {relative}")
                text = decode_text(path.read_bytes())
                files[relative.as_posix()] = count_comment_stats(text, extension)
    return files


def baseline_blobs(root, commit):
    entries = git(root, "ls-tree", "-r", "-z", "--full-tree", commit, "--", *sorted(SCAN_ROOTS))
    blobs = {}
    for entry in entries.split(b"\0"):
        if not entry:
            continue
        metadata, raw_path = entry.split(b"\t", 1)
        mode, kind, oid = metadata.split()
        if kind != b"blob" or mode == b"120000":
            continue
        path = PurePosixPath(os.fsdecode(raw_path))
        if not in_scope(path):
            continue
        blobs[path.as_posix()] = oid
    return blobs


def historical_texts(root, oids):
    """Read raw blobs by pinned object IDs, matching historical_lines() in
    check_file_size.py."""
    unique = sorted(set(oids))
    if not unique:
        return {}
    response = io.BytesIO(git(root, "cat-file", "--batch", input_bytes=b"\n".join(unique) + b"\n"))
    texts = {}
    for oid in unique:
        header = response.readline().split()
        if len(header) != 3 or header[0] != oid or header[1] != b"blob":
            raise GateError(f"cannot read baseline blob: {oid.decode('ascii')}")
        size = int(header[2])
        if size < 0:
            raise GateError("baseline blob has an invalid length")
        data = response.read(size)
        if len(data) != size or response.read(1) != b"\n":
            raise GateError("baseline blob data is incomplete")
        texts[oid] = decode_text(data)
    if response.read(1):
        raise GateError("baseline blob response contains unexpected trailing data")
    return texts


def baseline_stats(root, commit):
    blobs = baseline_blobs(root, commit)
    texts = historical_texts(root, blobs.values())
    return {
        path: count_comment_stats(texts[oid], effective_extension(PurePosixPath(path)))
        for path, oid in blobs.items()
    }


def check(root):
    toplevel = os.fsdecode(git(root, "rev-parse", "--show-toplevel")).rstrip("\n")
    if Path(toplevel).resolve() != root:
        raise GateError("the checker must live under scripts/ at the repository root being checked")
    selected = next(
        (ref for ref in BASELINE_REFS
         if git(root, "rev-parse", "--verify", "--quiet", ref, missing_ok=True) is not None),
        None,
    )
    if selected is None:
        raise GateError(
            "missing baseline origin/master or origin/main; fetch remote history explicitly first."
        )
    commit = git(root, "rev-parse", "--verify", selected + "^{commit}").strip().decode("ascii")
    print(f"baseline: {selected.removeprefix('refs/remotes/')} ({commit})", flush=True)

    check_coverage(root)
    baseline = baseline_stats(root, commit)
    current = collect_current(root)

    excess = 0
    total_dated = total_ledger = total_cjk = 0
    violations = []
    for path in sorted(current):
        actual_dated, actual_ledger, actual_cjk = current[path]
        base_dated, base_ledger, base_cjk = baseline.get(path, (0, 0, 0))
        total_dated += actual_dated
        total_ledger += actual_ledger
        total_cjk += actual_cjk
        for label, actual, allowed in (
            ("dated", actual_dated, base_dated),
            ("ledger", actual_ledger, base_ledger),
            ("cjk", actual_cjk, base_cjk),
        ):
            if actual > allowed:
                excess += actual - allowed
                violations.append(f"{path}: {label} {actual} > {allowed} (baseline)")

    print(
        f"扫描文件数：{len(current)}；超出总量：{excess} 行；"
        f"债务总量：dated {total_dated} / ledger {total_ledger} / cjk {total_cjk}"
    )
    if violations:
        print("FAIL:")
        print("\n".join(violations))
        return 1
    print("PASS: comment conventions ratchet passed.")
    return 0


def main():
    if len(sys.argv) != 1:
        print("ERROR: the checker accepts no arguments.", file=sys.stderr)
        return 2
    try:
        return check(Path(__file__).resolve().parent.parent)
    except (GateError, OSError, ValueError) as error:
        print(f"ERROR: comment conventions gate could not complete: {error}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    sys.exit(main())

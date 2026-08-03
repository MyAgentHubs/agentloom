export type DiffLineKind = "hunk" | "add" | "del" | "ctx";

export type DiffLine = {
  kind: DiffLineKind;
  text: string;
};

export type FileDiffStatus = "added" | "modified" | "deleted";

export type FileDiff = {
  path: string;
  status: FileDiffStatus;
  insertions: number;
  deletions: number;
  lines: DiffLine[];
  binary?: boolean;
};

const MAINSTREAM_DIFF_EXTENSIONS = new Set([
  "c",
  "bash",
  "cc",
  "cjs",
  "conf",
  "cpp",
  "cs",
  "cts",
  "css",
  "fish",
  "go",
  "h",
  "hh",
  "hpp",
  "hxx",
  "html",
  "htm",
  "ini",
  "java",
  "js",
  "jsx",
  "kt",
  "kts",
  "less",
  "md",
  "mjs",
  "mts",
  "php",
  "py",
  "rb",
  "rs",
  "sass",
  "scala",
  "scss",
  "sh",
  "sql",
  "svg",
  "svelte",
  "swift",
  "toml",
  "ts",
  "tsx",
  "txt",
  "vue",
  "xml",
  "yaml",
  "yml",
  "zsh",
  "json",
]);

const NON_MAINSTREAM_BASENAMES = new Set([
  "package-lock.json",
  "pnpm-lock.yaml",
  "pnpm-lock.yml",
  "yarn.lock",
  "cargo.lock",
  "composer.lock",
  "gemfile.lock",
  "poetry.lock",
  "bun.lock",
  "bun.lockb",
]);

const MAINSTREAM_DIFF_BASENAMES = new Set([
  ".gitignore",
  ".gitattributes",
  ".editorconfig",
  "makefile",
  "gnumakefile",
]);

export function isMainstreamDiffFile(path: string): boolean {
  const basename = path.split(/[\\/]/).pop()?.toLowerCase() ?? "";
  if (NON_MAINSTREAM_BASENAMES.has(basename) || basename.endsWith(".lock")) {
    return false;
  }
  if (
    MAINSTREAM_DIFF_BASENAMES.has(basename) ||
    basename === "dockerfile" ||
    basename.startsWith("dockerfile.")
  ) {
    return true;
  }
  const dot = basename.lastIndexOf(".");
  return dot >= 0 && MAINSTREAM_DIFF_EXTENSIONS.has(basename.slice(dot + 1));
}

/**
 * 同一路径跨段的 status 序列（按出现顺序，即时间顺序）合成最终 status。
 * 规则（F5 修正版——之前「deleted 粘住」的版本方向不对称：只处理了「后面出现 deleted」，
 * 没处理反过来「先 deleted 后 added」，会把『已提交段删除 + 未提交段重建』的文件误判成
 * 已删除，而文件其实还在）：
 * - 最终段（最后出现的那段）是 deleted → 文件最终态就是被删了，deleted 赢，不用管中间。
 * - 起点是 added 且没有任何一段是 deleted → 相对会话起点仍是新文件，哪怕后面几段是
 *   modified，整体仍算 added。
 * - 起点是 deleted、后面又出现过 added（先删后建）→ 不是单纯的「新增」，也不是单纯的
 *   「删除」，算 modified（内容变了，但文件本来就存在，只是中途被删过又建回来）。
 * - 其余情况：按最后一段的 status。
 */
function mergeFileStatus(statuses: FileDiffStatus[]): FileDiffStatus {
  const first = statuses[0];
  const last = statuses[statuses.length - 1];
  if (last === "deleted") return "deleted";
  if (first === "added") return "added";
  if (first === "deleted" && statuses.includes("added")) return "modified";
  return last;
}

/**
 * 同一路径跨多段 diff --git 块（后端归因求和：本会话各 run 分段 diff + 当前未提交 diff
 * 拼接而成的 patch，同一个文件被多段各自改过时会出现多次）合并成一张卡片：insertions/
 * deletions 累加、行按段出现顺序拼接（各自保留自己的 `@@` hunk 头，等价于同一文件里的
 * 多个不相连 hunk）、status 见 `mergeFileStatus`。按首次出现的路径顺序输出，保证跟后端
 * `files`（同样按首次出现去重）逐项对齐。
 *
 * 行拼接用 for 循环逐条 push，不用 `existing.lines.push(...file.lines)` 展开传参
 * （F7：单段行数极大时展开传参会撞 V8 的函数参数上限，抛 `Maximum call stack size
 * exceeded`；逐条 push 零这个风险，成本一样是 O(n)）。
 */
function mergeDuplicatePaths(files: FileDiff[]): FileDiff[] {
  const order: string[] = [];
  const groups = new Map<string, FileDiff[]>();
  for (const file of files) {
    const existing = groups.get(file.path);
    if (existing) {
      existing.push(file);
    } else {
      order.push(file.path);
      groups.set(file.path, [file]);
    }
  }
  return order.map((path) => {
    const occurrences = groups.get(path)!;
    if (occurrences.length === 1) return occurrences[0];
    let insertions = 0;
    let deletions = 0;
    let binary = false;
    const lines: DiffLine[] = [];
    const statuses: FileDiffStatus[] = [];
    for (const occurrence of occurrences) {
      insertions += occurrence.insertions;
      deletions += occurrence.deletions;
      if (occurrence.binary) binary = true;
      statuses.push(occurrence.status);
      for (const line of occurrence.lines) {
        lines.push(line);
      }
    }
    return {
      path,
      status: mergeFileStatus(statuses),
      insertions,
      deletions,
      lines,
      binary,
    };
  });
}

/**
 * 把 git unified diff 字符串拆成按文件分组的结构（纯函数·前端用）。
 * 复用现有 session_review 返回的单 patch（= `git diff base_ref` + untracked 的
 * `git diff --no-index /dev/null f` 串联·后端 worktree.rs·patch 不含 --stat 摘要）。
 * 关键：用 inHunk 状态机——`@@` 之前是文件头区（跳 ---/+++/index/mode 等元数据）；
 * `@@` 之后是 hunk 内容区，**严格按首字符 +/-/空格 分类**，绝不再把 `--- foo`/`+++ foo`
 * 这种「内容行恰好以 --- /+++ 开头」误当元数据丢掉（codex P1）。
 */
export function parseUnifiedDiff(patch: string): FileDiff[] {
  if (!patch || !patch.trim()) return [];
  const lines = patch.split("\n");
  const files: FileDiff[] = [];
  let cur: FileDiff | null = null;
  let inHunk = false;

  const pushCur = () => {
    if (cur) files.push(cur);
  };

  for (const line of lines) {
    if (line.startsWith("diff --git ")) {
      pushCur();
      // diff --git a/<old> b/<new> —— 取 b/ 侧路径作 path
      const m = line.match(/^diff --git a\/(.+?) b\/(.+)$/);
      const path = m ? m[2] : line.slice("diff --git ".length);
      cur = {
        path,
        status: "modified",
        insertions: 0,
        deletions: 0,
        lines: [],
      };
      inHunk = false;
      continue;
    }
    if (!cur) continue; // diff --git 之前的内容（防御性·真实后端无）：丢弃

    if (line.startsWith("@@")) {
      inHunk = true;
      cur.lines.push({ kind: "hunk", text: line });
      continue;
    }

    if (inHunk) {
      // hunk 内容区：首字符决定 kind（内容行可能恰好以 ---/+++ 开头·不可误判元数据）
      const c = line[0];
      if (line.startsWith("\\ No newline")) {
        // git 的「\ No newline at end of file」标记·不计增删·按 ctx 留痕
        cur.lines.push({ kind: "ctx", text: line });
      } else if (c === "+") {
        cur.insertions += 1;
        cur.lines.push({ kind: "add", text: line });
      } else if (c === "-") {
        cur.deletions += 1;
        cur.lines.push({ kind: "del", text: line });
      } else {
        // 上下文行（开头空格）或空字符串
        cur.lines.push({ kind: "ctx", text: line });
      }
      continue;
    }

    // 文件头区（@@ 之前）：识别状态 + 跳过元数据
    if (line.startsWith("new file mode")) {
      cur.status = "added";
      continue;
    }
    if (line.startsWith("deleted file mode")) {
      cur.status = "deleted";
      continue;
    }
    if (line.startsWith("Binary files ") || line === "GIT binary patch") {
      cur.binary = true;
      continue;
    }
    // 其余文件头元数据（index/---/+++/mode/rename/similarity/binary 提示）：不渲染、不计数
    // （binary 文件 git 会输出「Binary files ... differ」·此处 inHunk=false 故丢弃·不崩）
  }
  pushCur();
  return mergeDuplicatePaths(files);
}

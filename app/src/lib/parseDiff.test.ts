import { describe, it, expect } from "vitest";
import { isMainstreamDiffFile, parseUnifiedDiff } from "./parseDiff";

describe("isMainstreamDiffFile", () => {
  it("只允许主流代码、文档与配置文本扩展名（大小写不敏感）", () => {
    expect(isMainstreamDiffFile("src/App.TSX")).toBe(true);
    expect(isMainstreamDiffFile("README.md")).toBe(true);
    expect(isMainstreamDiffFile("config/settings.yaml")).toBe(true);
    expect(isMainstreamDiffFile("data/events.jsonl")).toBe(false);
    expect(isMainstreamDiffFile("pnpm-lock.yaml")).toBe(false);
    expect(isMainstreamDiffFile("assets/logo.png")).toBe(false);
  });

  it("无扩展名的常见文本配置与 htm 别名正常预览", () => {
    expect(isMainstreamDiffFile("Dockerfile")).toBe(true);
    expect(isMainstreamDiffFile("deploy/Dockerfile.prod")).toBe(true);
    expect(isMainstreamDiffFile("Makefile")).toBe(true);
    expect(isMainstreamDiffFile(".gitignore")).toBe(true);
    expect(isMainstreamDiffFile("public/index.htm")).toBe(true);
  });

  it("只排除精确 lockfile，不误杀文件名含 -lock. 的源码", () => {
    expect(isMainstreamDiffFile("src/distributed-lock.go")).toBe(true);
    expect(isMainstreamDiffFile("src/use-lock.tsx")).toBe(true);
    expect(isMainstreamDiffFile("src/dead-lock.ts")).toBe(true);
    expect(isMainstreamDiffFile("package-lock.json")).toBe(false);
    expect(isMainstreamDiffFile("pnpm-lock.yaml")).toBe(false);
  });
});

describe("parseUnifiedDiff", () => {
  it("空 patch → 空数组", () => {
    expect(parseUnifiedDiff("")).toEqual([]);
    expect(parseUnifiedDiff("   \n")).toEqual([]);
  });

  it("单文件修改：抽 path / 统计 +N−N / 保留 hunk 与增删行", () => {
    const patch = [
      "diff --git a/src/GoalBar.tsx b/src/GoalBar.tsx",
      "index 1111111..2222222 100644",
      "--- a/src/GoalBar.tsx",
      "+++ b/src/GoalBar.tsx",
      "@@ -42,3 +42,4 @@ function GoalBar() {",
      " const total = goals.length;",
      "-  return old;",
      "+  const allPass = resolved === total;",
      "+  return next;",
    ].join("\n");
    const files = parseUnifiedDiff(patch);
    expect(files).toHaveLength(1);
    expect(files[0].path).toBe("src/GoalBar.tsx");
    expect(files[0].status).toBe("modified");
    expect(files[0].insertions).toBe(2);
    expect(files[0].deletions).toBe(1);
    // 行：1 个 hunk 头 + 1 ctx + 1 del + 2 add（meta 行 index/---/+++ 不进 lines）
    expect(files[0].lines.map((l) => l.kind)).toEqual([
      "hunk",
      "ctx",
      "del",
      "add",
      "add",
    ]);
    expect(files[0].lines[2].text).toBe("-  return old;");
  });

  it("新增文件 → status added（new file mode）", () => {
    const patch = [
      "diff --git a/tmp.txt b/tmp.txt",
      "new file mode 100644",
      "index 0000000..3333333",
      "--- /dev/null",
      "+++ b/tmp.txt",
      "@@ -0,0 +1,2 @@",
      "+hello",
      "+world",
    ].join("\n");
    const files = parseUnifiedDiff(patch);
    expect(files).toHaveLength(1);
    expect(files[0].path).toBe("tmp.txt");
    expect(files[0].status).toBe("added");
    expect(files[0].insertions).toBe(2);
    expect(files[0].deletions).toBe(0);
  });

  it("删除文件 → status deleted（deleted file mode）", () => {
    const patch = [
      "diff --git a/old.txt b/old.txt",
      "deleted file mode 100644",
      "index 4444444..0000000",
      "--- a/old.txt",
      "+++ /dev/null",
      "@@ -1,1 +0,0 @@",
      "-bye",
    ].join("\n");
    const files = parseUnifiedDiff(patch);
    expect(files[0].path).toBe("old.txt");
    expect(files[0].status).toBe("deleted");
    expect(files[0].deletions).toBe(1);
  });

  it("多文件：按 diff --git 边界切分", () => {
    const patch = [
      "diff --git a/a.ts b/a.ts",
      "--- a/a.ts",
      "+++ b/a.ts",
      "@@ -1 +1 @@",
      "-a",
      "+A",
      "diff --git a/b.ts b/b.ts",
      "--- a/b.ts",
      "+++ b/b.ts",
      "@@ -0,0 +1 @@",
      "+B",
    ].join("\n");
    const files = parseUnifiedDiff(patch);
    expect(files.map((f) => f.path)).toEqual(["a.ts", "b.ts"]);
  });

  it("防御：diff --git 之前的杂行不误判为文件块（真实后端 patch 不含 --stat·此为防御兼容）", () => {
    const patch = [
      " src/x.ts | 2 +-",
      " 1 file changed, 1 insertion(+), 1 deletion(-)",
      "diff --git a/src/x.ts b/src/x.ts",
      "--- a/src/x.ts",
      "+++ b/src/x.ts",
      "@@ -1 +1 @@",
      "-x",
      "+X",
    ].join("\n");
    const files = parseUnifiedDiff(patch);
    expect(files).toHaveLength(1);
    expect(files[0].path).toBe("src/x.ts");
  });

  it("hunk 内『内容行恰好以 -/+ 多字符开头』按增删计、不误当 ---/+++ 元数据丢（codex P1）", () => {
    const patch = [
      "diff --git a/doc.md b/doc.md",
      "--- a/doc.md",
      "+++ b/doc.md",
      "@@ -1,2 +1,2 @@",
      "--- 旧的分隔线标题",
      "+++ 新的分隔线标题",
    ].join("\n");
    const files = parseUnifiedDiff(patch);
    expect(files).toHaveLength(1);
    // hunk 内的 `--- xxx` = 删除行（首字符 -）·`+++ xxx` = 新增行（首字符 +）·不被当元数据
    expect(files[0].deletions).toBe(1);
    expect(files[0].insertions).toBe(1);
    expect(files[0].lines.map((l) => l.kind)).toEqual(["hunk", "del", "add"]);
  });

  it("『\\ No newline at end of file』标记按 ctx 留痕·不计增删", () => {
    const patch = [
      "diff --git a/x b/x",
      "--- a/x",
      "+++ b/x",
      "@@ -1 +1 @@",
      "-a",
      "\\ No newline at end of file",
      "+b",
      "\\ No newline at end of file",
    ].join("\n");
    const files = parseUnifiedDiff(patch);
    expect(files[0].insertions).toBe(1);
    expect(files[0].deletions).toBe(1);
    expect(files[0].lines.filter((l) => l.kind === "ctx")).toHaveLength(2);
  });

  it("同一路径跨多段 diff --git 块合并成一张卡片：insertions/deletions 累加、行拼接、按首次出现去重（commit 1 归因求和的已知代价）", () => {
    const patch = [
      "diff --git a/tracked.md b/tracked.md",
      "--- a/tracked.md",
      "+++ b/tracked.md",
      "@@ -1 +1 @@",
      "-before",
      "+run one edit",
      "diff --git a/other.ts b/other.ts",
      "--- a/other.ts",
      "+++ b/other.ts",
      "@@ -1 +1 @@",
      "-old",
      "+new",
      "diff --git a/tracked.md b/tracked.md",
      "--- a/tracked.md",
      "+++ b/tracked.md",
      "@@ -5 +5,2 @@",
      " context",
      "+run two edit",
    ].join("\n");
    const files = parseUnifiedDiff(patch);
    // 按首次出现的路径顺序去重：tracked.md 第一次出现在最前，合并后仍排第一。
    expect(files.map((f) => f.path)).toEqual(["tracked.md", "other.ts"]);
    const tracked = files[0];
    expect(tracked.insertions).toBe(2); // 两段各 +1
    expect(tracked.deletions).toBe(1); // 只有第一段有一行删除
    // 两个 hunk 头都保留（各自的 @@ 标记 + 内容行按段先后拼接）。
    expect(tracked.lines.map((l) => l.kind)).toEqual([
      "hunk",
      "del",
      "add",
      "hunk",
      "ctx",
      "add",
    ]);
  });

  it("跨段合并：最终段是 deleted 时整卡的 status 以最终态为准，即便先出现的段是 modified", () => {
    const patch = [
      "diff --git a/gone.ts b/gone.ts",
      "--- a/gone.ts",
      "+++ b/gone.ts",
      "@@ -1 +1 @@",
      "-a",
      "+b",
      "diff --git a/gone.ts b/gone.ts",
      "deleted file mode 100644",
      "--- a/gone.ts",
      "+++ /dev/null",
      "@@ -1 +0,0 @@",
      "-b",
    ].join("\n");
    const files = parseUnifiedDiff(patch);
    expect(files).toHaveLength(1);
    expect(files[0].status).toBe("deleted");
  });

  it("跨段合并：先 added 后 modified，仍算 added（相对会话起点仍是新文件）", () => {
    const patch = [
      "diff --git a/new.ts b/new.ts",
      "new file mode 100644",
      "--- /dev/null",
      "+++ b/new.ts",
      "@@ -0,0 +1 @@",
      "+hello",
      "diff --git a/new.ts b/new.ts",
      "--- a/new.ts",
      "+++ b/new.ts",
      "@@ -1 +1 @@",
      "-hello",
      "+hello world",
    ].join("\n");
    const files = parseUnifiedDiff(patch);
    expect(files).toHaveLength(1);
    expect(files[0].status).toBe("added");
    expect(files[0].insertions).toBe(2);
    expect(files[0].deletions).toBe(1);
  });

  it("F5 定罪回归：跨段合并——先 deleted 后 added（已提交段删除 + 未提交段重建），整卡应算 modified 而非粘住 deleted（文件其实还在）", () => {
    const patch = [
      "diff --git a/revived.ts b/revived.ts",
      "deleted file mode 100644",
      "--- a/revived.ts",
      "+++ /dev/null",
      "@@ -1 +0,0 @@",
      "-old content",
      "diff --git a/revived.ts b/revived.ts",
      "new file mode 100644",
      "--- /dev/null",
      "+++ b/revived.ts",
      "@@ -0,0 +1 @@",
      "+new content",
    ].join("\n");
    const files = parseUnifiedDiff(patch);
    expect(files).toHaveLength(1);
    // 先删后建：文件本来就存在（不是纯新增），最终也没被删（不是纯删除）——
    // 两个方向都不对，"modified" 是唯一诚实的描述。
    expect(files[0].status).toBe("modified");
    expect(files[0].insertions).toBe(1);
    expect(files[0].deletions).toBe(1);
  });

  it("binary 文件（Binary files ... differ·无 hunk）不崩·产出一个 0/0 的文件项", () => {
    const patch = [
      "diff --git a/logo.png b/logo.png",
      "new file mode 100644",
      "index 0000000..abc1234",
      "Binary files /dev/null and b/logo.png differ",
    ].join("\n");
    const files = parseUnifiedDiff(patch);
    expect(files).toHaveLength(1);
    expect(files[0].path).toBe("logo.png");
    expect(files[0].status).toBe("added");
    expect(files[0].insertions).toBe(0);
    expect(files[0].deletions).toBe(0);
    expect(files[0].lines).toEqual([]); // 无 hunk·body 空·渲染时显「二进制·无文本 diff」由组件兜底
    expect(files[0].binary).toBe(true);
  });
});

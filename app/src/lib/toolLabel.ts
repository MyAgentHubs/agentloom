import type { I18nKey } from "../i18n";

type Translate = (
  key: I18nKey,
  values?: Record<string, string | number>,
) => string;

/**
 * chat 消息流工具卡展示层：内部工具名（claude 原名 / myagent 名 / codex 伪名 /
 * 交付四件套）→ 人话 i18n key。纯展示层映射，后端事件契约（block.tool 原值）不变。
 *
 * 覆盖范围（F1 实勘定罪·2026-07-25）：
 * - claude 原名：Bash / Read / Write / Edit / Glob / Grep / Task / TodoWrite /
 *   WebFetch / WebSearch / NotebookEdit / BashOutput / KillShell。
 * - myagent 名：fs_read / fs_edit / fs_write / shell_exec / grep / glob / ls /
 *   web_search / memory_*（前缀）。
 * - codex 伪名：command / file / image_gen。
 * - 交付四件套：mcp__agentloom__commit / push / create_pr / publish（这四个从
 *   streamItems.ts 的隐藏名单里拎出来显示，映射层同样要给人话）。
 * - verifier：propose_verifier Auto 直跑后落库的结果信息卡（决策打扰收敛刀 T2 改款·
 *   fold-default），工具名固定 "verifier"（跨刀协调已定，别改名）。
 *
 * 未收录的工具名原样透传——前向兼容的硬要求：新工具在这张表补上之前必须保持可见，
 * 绝不能被静默吞掉或译错（同 stopReason.ts 的透传纪律）。
 */
const TOOL_NAME_KEYS: Record<string, I18nKey> = {
  // claude 原名
  Bash: "toolCard.name.bash",
  Read: "toolCard.name.read",
  Write: "toolCard.name.write",
  Edit: "toolCard.name.edit",
  Glob: "toolCard.name.glob",
  Grep: "toolCard.name.grep",
  Task: "toolCard.name.task",
  TodoWrite: "toolCard.name.todoWrite",
  WebFetch: "toolCard.name.webFetch",
  WebSearch: "toolCard.name.webSearch",
  NotebookEdit: "toolCard.name.notebookEdit",
  BashOutput: "toolCard.name.bashOutput",
  KillShell: "toolCard.name.killShell",
  // myagent 名
  fs_read: "toolCard.name.read",
  fs_edit: "toolCard.name.edit",
  fs_write: "toolCard.name.write",
  shell_exec: "toolCard.name.bash",
  grep: "toolCard.name.grep",
  glob: "toolCard.name.glob",
  ls: "toolCard.name.ls",
  web_search: "toolCard.name.webSearch",
  // codex 伪名
  command: "toolCard.name.bash",
  file: "toolCard.name.edit",
  image_gen: "toolCard.name.imageGen",
  // 交付四件套
  mcp__agentloom__commit: "toolCard.name.commit",
  mcp__agentloom__push: "toolCard.name.push",
  mcp__agentloom__create_pr: "toolCard.name.createPr",
  mcp__agentloom__publish: "toolCard.name.publish",
  // 验证回执（决策打扰收敛刀 T2 改款·fold-default）
  verifier: "toolCard.name.verifier",
};

/** memory_* 前缀（myagent 内部记忆工具族）→ 记笔记。精确匹配优先于前缀匹配。 */
const MEMORY_PREFIX = "memory_";

/** 把展示层工具名过一遍人话映射；未收录的名字（含未来新增工具）原样透传，绝不吞。 */
export function humanizeToolName(raw: string, t: Translate): string {
  const key = TOOL_NAME_KEYS[raw];
  if (key) return t(key);
  if (raw.startsWith(MEMORY_PREFIX)) return t("toolCard.name.memory");
  return raw;
}

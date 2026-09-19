import type { Block } from "../types/agent";

// V1（2026-08-26·刀②「桌面 chat verbose 分级」）：从 MessageContent.tsx:120-150
// 原样迁出的路径级图片抽取 + 去重，供 MessageContent 与 foldByVerbosity 共用一份。
// 只承诺「路径字符串/后缀/首现」去重——按图片内容判重与加载后择优仍是
// ImageArtifactChips 组件本地 state 的事（不搬进这份纯函数，见设计稿 §2A）。

const IMAGE_PATH_TOKEN_BOUNDARY = /[\s"'`<>|]+/u;
const IMAGE_PATH_EXTENSION = /\.(?:png|jpe?g|gif|webp|bmp|svg)$/i;
const IMAGE_PATH_LEADING_PUNCTUATION = /^[([{<“”‘’「」『』]+/u;
const IMAGE_PATH_TRAILING_PUNCTUATION =
  /[.,;:!?)\]}>。，；：！？）】」』》〉…“”‘’]+$/u;
// 单个工具块最多触发 8 次附件读取，避免路径枚举输出造成缩略图洪泛。
const MAX_IMAGE_PATHS_PER_TOOL_BLOCK = 8;

// 搜索/列举类工具的输出是「路径列表」，不是「图片产物」——命中一堆 .png/.svg
// 路径不代表 agent 生成/保存了图片，别当图片附件渲染成缩略图卡。名单核对自
// lib/toolLabel.ts 的工具名映射表（claude 原名 / myagent 名，2026-07-27）。
const SEARCH_TOOLS: ReadonlySet<string> = new Set([
  "Grep",
  "Glob",
  "grep",
  "glob",
  "ls",
  "WebSearch",
  "web_search",
]);

// read/write/edit 类工具的输出是「文件内容/改动回执」，verifier 是「测试日志」；
// 里面出现的图片路径是被引用的字符串（如 import、补丁、日志里的截图路径），
// 不是这次工具调用产出的图片工件，同样不该渲染成图片附件卡。
// 注：不含 "file"（codex file_change）——见下方 PRODUCED_PATH_IN_SUMMARY_TOOLS 之外
// 那条注释：它的 summary 只有 basename，真实路径由后端塞进 output，走通用扫描捞取。
const CONTENT_TOOLS: ReadonlySet<string> = new Set([
  "Read",
  "fs_read",
  "verifier",
  "Write",
  "write",
  "Edit",
  "edit",
  "fs_write",
  "fs_edit",
  "apply_patch",
]);

// T24b 规则 A（工具产物即图）：这几个工具的 summary 恒等于本次操作的目标文件路径
// （后端 tool_summary()/harness s("path") 直接取 file_path/path，没有其他杂字），
// 是可信的「产物路径」信号——不同于 output（可能是确认文案/回执，会被 CONTENT_TOOLS
// 挡住不扫，理由同上）。只信 summary 本身，不解禁整个工具去扫 output，避免重新引入
// CONTENT_TOOLS 本要挡的「引用字符串误判成产物」。
const PRODUCED_PATH_IN_SUMMARY_TOOLS: ReadonlySet<string> = new Set([
  "Write",
  "Edit",
  "MultiEdit",
  "write",
  "edit",
  "fs_write",
  "fs_edit",
]);

function extractImagePathTokens(text: string): string[] {
  const tokens = text.split(IMAGE_PATH_TOKEN_BOUNDARY);
  const paths = tokens
    .map((token) =>
      token
        .replace(IMAGE_PATH_LEADING_PUNCTUATION, "")
        .replace(/^[^=]*=(?=\/)/u, "")
        .replace(IMAGE_PATH_TRAILING_PUNCTUATION, ""),
    )
    .filter((token) => {
      const isDrivePath = /^[A-Za-z]:[\\/]/.test(token);
      const hasUrlScheme = /^[A-Za-z][A-Za-z\d+.-]*:/.test(token);
      if (
        !IMAGE_PATH_EXTENSION.test(token) ||
        token.startsWith("//") ||
        (hasUrlScheme && !isDrivePath)
      ) {
        return false;
      }
      return (
        token.startsWith("/") ||
        token.startsWith("~/") ||
        isDrivePath ||
        token.includes("/")
      );
    });
  return [...new Set(paths)];
}

export function imagePathsFromTool(
  block: Extract<Block, { type: "tool" }>,
): string[] {
  if (SEARCH_TOOLS.has(block.tool)) return [];
  if (PRODUCED_PATH_IN_SUMMARY_TOOLS.has(block.tool)) {
    return extractImagePathTokens(block.summary).slice(
      0,
      MAX_IMAGE_PATHS_PER_TOOL_BLOCK,
    );
  }
  if (CONTENT_TOOLS.has(block.tool)) return [];
  return extractImagePathTokens(
    `${block.summary}\n${block.output ?? ""}`,
  ).slice(0, MAX_IMAGE_PATHS_PER_TOOL_BLOCK);
}

export type ImageArtifacts = {
  /** 整条消息去重后的图片路径（路径级：字符串/后缀/首现规则）。 */
  paths: string[];
  /** 每个去重后路径分配给其首次出现的 tool 块在输入 blocks 数组里的下标。 */
  byBlockIndex: Map<number, string[]>;
};

// 整条消息级的路径抽取 + 去重 + 首现分配——语义等同现状 MessageContent.tsx
// 里的 allPaths / preferredPaths / seen 三段式，只是把「按 StreamItem 分配」
// 改成「按原始 blocks 下标分配」（foldByVerbosity 按段切块需要下标锚点）。
export function collectImageArtifacts(blocks: Block[]): ImageArtifacts {
  const pathsByIndex = blocks.map((block) =>
    block.type === "tool" ? imagePathsFromTool(block) : [],
  );
  const allPaths = [...new Set(pathsByIndex.flat())];
  const preferredPaths = new Set(
    allPaths.filter(
      (path) => !allPaths.some((other) => other.endsWith(`/${path}`)),
    ),
  );
  const seen = new Set<string>();
  const byBlockIndex = new Map<number, string[]>();
  const paths: string[] = [];
  blocks.forEach((_block, index) => {
    const fresh: string[] = [];
    pathsByIndex[index].forEach((path) => {
      if (!preferredPaths.has(path) || seen.has(path)) return;
      seen.add(path);
      fresh.push(path);
      paths.push(path);
    });
    if (fresh.length > 0) byBlockIndex.set(index, fresh);
  });
  return { paths, byBlockIndex };
}

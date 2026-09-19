import { stripFileScheme } from "../components/localMarkdownImage";

/// 规则 B：从一段 assistant 文本（段落 / 列表项的原始 markdown 源片段）里
/// 抽取本地图片路径，供调用方在该段落下方追加图片块（不改原文）。
///
/// 识别形态：① 反引号内 `` `/a/b.png` ``；② 句中裸绝对路径（`/...`、`~/...`、
/// Windows 盘符）；③ `file://` scheme；④ `<...>` 包裹且内部含空格；
/// ⑤ 已是 `![]()` 语法的路径原样跳过（避免与既有 img 渲染重复）。
/// 返回值已规范化（剥 `file://`、去尾随标点）并按规范化路径去重——但只在
/// 「这一段文本」范围内去重；跨段/跨消息去重由调用方维护。

const IMG_EXT_RE = /\.(png|jpe?g|gif|webp|svg)$/i;

// 反引号 / <...> 捕获的内容可能带尾随标点（如中文句末标点紧跟在扫描到的
// 分隔符内侧），扫描到后统一裁掉；裸路径匹配本身已经靠字符集把这些标点
// 挡在匹配之外，这里再裁一次是幂等的兜底。
const TRAILING_PUNCT_RE = /[)\]}）】》,.;:!?，。；：！？、"'“”‘’]+$/u;

// 判定「像本地绝对路径」的前缀：`/`（且非协议相对 `//`）、`~/`、或 Windows 盘符。
const ABS_PREFIX_RE = /^(?:~\/|\/|[A-Za-z]:[\\/])/;

// 句中裸路径匹配：字符集刻意不含空白 / 中英文标点 / URL 特殊字符（`?`、`#`、
// `&`、`=`），命中即天然在这些字符前停住，不需要额外裁剪。前置负向前瞻
// 确保匹配起点不落在另一个路径 token 内部（如 `assets/x.png` 里 `x.png`
// 前的那个 `/` 不该被当成独立路径起点）。
const BARE_TOKEN_RE =
  /(?<![A-Za-z0-9_\-./%\\])(?:file:\/{1,3}[A-Za-z0-9_\-./%]+|~\/[A-Za-z0-9_\-./%]+|\/[A-Za-z0-9_\-./%]+|[A-Za-z]:[\\/][A-Za-z0-9_\-./\\%]+)/g;

// 裸路径匹配结束后，下一个字符必须是空白或这些收尾标点之一才接受——
// 用来把 `/a/b.png?x=1` 这类「看着像路径、其实后面还有东西」的伪匹配挡掉，
// 同时放行 `/a/b.png。`、`/a/b.png）` 这类真正的句末标点。ASCII `!`/`?`
// 刻意不放行——它们与 URL query 串 (`?x=1`) 的边界字符冲突，宁可漏判
// 英文感叹号/问号收尾的裸路径，也不能把 URL 误判成本地文件。
const ALLOWED_BOUNDARY_RE = /^[\s)\]}）】》,.;:，。；：！？、"'“”‘’]/u;

const IMAGE_MARKDOWN_RE = /!\[[^\]]*\]\([^)]*\)/g;
const BACKTICK_RE = /`([^`]+)`/g;
const ANGLE_WITH_SPACE_RE = /<([^<>]*\s[^<>]*)>/g;

function trimTrailingPunct(value: string): string {
  return value.replace(TRAILING_PUNCT_RE, "");
}

/// 校验并规范化一个候选字符串：非图片路径返回 null；是则返回剥掉
/// `file://`（并校验 host 为空）、裁掉尾随标点后的规范化路径。
function extractValidPath(rawCandidate: string): string | null {
  const trimmed = trimTrailingPunct(rawCandidate.trim());
  if (!trimmed || trimmed.startsWith("//")) return null;

  let candidate = trimmed;
  const fileStripped = stripFileScheme(trimmed);
  if (fileStripped !== null) {
    candidate = fileStripped;
  } else if (!ABS_PREFIX_RE.test(trimmed)) {
    return null;
  }
  if (candidate.startsWith("//")) return null;

  return IMG_EXT_RE.test(candidate) ? candidate : null;
}

export function scanImagePaths(text: string): string[] {
  if (!text) return [];

  // ⑤ 先把已有的 ![]() 语法整体抹白，避免其内部路径被下面的规则再当作
  // 裸路径抽出来重复渲染（该路径已经交给既有 img 渲染通道处理）。
  let working = text.replace(IMAGE_MARKDOWN_RE, (m) => " ".repeat(m.length));

  const found: string[] = [];
  const seen = new Set<string>();
  const addCandidate = (raw: string) => {
    const valid = extractValidPath(raw);
    if (!valid || seen.has(valid)) return;
    seen.add(valid);
    found.push(valid);
  };

  // ① 反引号内；抽完抹白，避免内容又被裸路径规则重复扫到。
  working = working.replace(BACKTICK_RE, (m, inner: string) => {
    addCandidate(inner);
    return " ".repeat(m.length);
  });

  // ④ <...> 包裹且内部含空格；同样抽完抹白。
  working = working.replace(ANGLE_WITH_SPACE_RE, (m, inner: string) => {
    addCandidate(inner);
    return " ".repeat(m.length);
  });

  // ②③ 剩余文本里的裸绝对路径 / file:// scheme。
  BARE_TOKEN_RE.lastIndex = 0;
  let match: RegExpExecArray | null;
  while ((match = BARE_TOKEN_RE.exec(working))) {
    const end = match.index + match[0].length;
    const next = working.slice(end, end + 1);
    if (next && !ALLOWED_BOUNDARY_RE.test(next)) continue;
    addCandidate(match[0]);
  }

  return found;
}

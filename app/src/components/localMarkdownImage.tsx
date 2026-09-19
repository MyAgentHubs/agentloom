import React, { useEffect, useState } from "react";
import type { ExtraProps, UrlTransform } from "react-markdown";
import {
  getAttachmentDataUri,
  setAttachmentDataUri,
} from "../lib/attachmentCache";
import { useAttachmentPort } from "../lib/attachmentPortContext";
import { scanImagePaths } from "../lib/imagePathScan";
import "../styles/chatImage.css";

export function isLocalImagePath(src: string): boolean {
  if (
    !src ||
    src.startsWith("//") ||
    src.startsWith("#") ||
    src.startsWith("?")
  ) {
    return false;
  }
  return /^[a-z]:[\\/]/i.test(src) || !/^[a-zA-Z][a-zA-Z0-9+.-]*:/.test(src);
}

// 支持 1~3 条斜杠的 file: 写法（`file:/a`、`file://host/a`、`file:///a`）。
// 具体哪几条斜杠算「URL authority 分隔符」需剥掉、哪一条其实是路径自身的开头
// `/` 要留下，在 stripFileScheme 里按斜杠数分支处理，见其注释。
const FILE_SCHEME_RE = /^file:\/{1,3}/i;

/// 剥掉 `file:` scheme、还原 percent-encoding，得到可交给
/// isLocalImagePath/read_attachment 的本地路径。不是 `file:` URL、或剥完不是
/// `/` 开头绝对路径（如 host 非空的 `file://host/path` 网络路径形态）时返回 null。
///
/// 斜杠数与剥法：
/// - 恰好 2 条（`file://xxx`）：这 2 条整体是 URL authority 分隔符，全部剥掉——
///   若后面紧跟的不是路径自身的 `/`（即 host 非空，如 `file://host/path`），
///   剥完 rest 不以 `/` 开头，下面的绝对路径校验会把它拒掉。
/// - 1 条或 3 条（`file:/path`、`file:///path`）：多剥的那条其实是路径自身的
///   开头 `/`，要留一条不剥（`match[0].slice(0, -1)`），剥完 rest 才继续以 `/`
///   开头。`file:///Users/a/b.png` → `/Users/a/b.png`；`file:/Users/a/b.png`
///   同样 → `/Users/a/b.png`。
///
/// 空格/中文等 percent-encoded 字符经 decodeURIComponent 还原，解码失败（畸形
/// 转义）时按原样剥壳串继续校验兜底。
export function stripFileScheme(url: string): string | null {
  const match = FILE_SCHEME_RE.exec(url);
  if (!match) return null;
  const slashCount = match[0].length - "file:".length;
  const consumed = slashCount === 2 ? match[0] : match[0].slice(0, -1);
  const rest = url.slice(consumed.length);
  let decoded = rest;
  try {
    decoded = decodeURIComponent(rest);
  } catch {
    // 畸形转义：原样剥壳串继续走下面的绝对路径校验兜底。
  }
  return decoded.startsWith("/") ? decoded : null;
}

/// react-markdown 的 urlTransform 工厂：本地路径豁免只作用于 <img src>（配合
/// localImageMarkdownComponent 使用），其余一切 URL（含 <a href>）一律交给调用方
/// 传入的 react-markdown 默认消毒器处理。用于避免「图片本地路径豁免」被误套用到
/// 链接 href 上、放行盘符形态（`C:\...`）或伪装盘符（`j:%5C...`）的路径。
///
/// `file://` scheme 单独判在最前：react-markdown 默认协议白名单不含 file:，
/// 若走下方 decodeURI + isLocalImagePath 判定，`file:...` 会被 isLocalImagePath
/// 当成「带 scheme 的外部 URL」拒绝、再被 defaultUrlTransform 清空 src——静默不
/// 渲染。剥掉 scheme 后按本地路径处理，行为与直接写绝对路径一致。
export function makeImgOnlyUrlTransform(
  defaultUrlTransform: (url: string) => string,
): UrlTransform {
  return (url, key, node) => {
    if (key !== "src" || node?.tagName !== "img") {
      return defaultUrlTransform(url);
    }

    const filePath = stripFileScheme(url);
    if (filePath !== null) {
      return isLocalImagePath(filePath) ? filePath : defaultUrlTransform(url);
    }

    let candidate = url;
    try {
      candidate = decodeURI(url);
    } catch {
      // Let react-markdown sanitize malformed URLs through its default transform.
    }
    return isLocalImagePath(candidate) ? candidate : defaultUrlTransform(url);
  };
}

function decodeLocalImagePath(path: string): string {
  try {
    return decodeURI(path);
  } catch {
    return path;
  }
}

export function PreviewablePath({
  path,
  onOpenPreview,
}: {
  path: string;
  onOpenPreview?: (path: string) => void;
}) {
  if (!onOpenPreview) return <code className="inline">{path}</code>;

  return (
    <code
      className="inline inline-path"
      role="button"
      tabIndex={0}
      title={path}
      onClick={() => onOpenPreview(path)}
      onKeyDown={(event) => {
        if (event.key === "Enter" || event.key === " ") {
          event.preventDefault();
          onOpenPreview(path);
        }
      }}
    >
      {path}
    </code>
  );
}

export function LocalMarkdownImage({
  path,
  alt,
  sessionId,
  onOpenPreview,
  onOpenLightbox,
}: {
  path: string;
  alt?: string;
  sessionId?: string | null;
  onOpenPreview?: (path: string) => void;
  onOpenLightbox?: (path: string) => void;
}) {
  const decodedPath = decodeLocalImagePath(path);
  const attachmentPort = useAttachmentPort();
  const [dataUri, setDataUri] = useState<string | null>(() =>
    getAttachmentDataUri(decodedPath, sessionId),
  );
  const [failed, setFailed] = useState(false);

  useEffect(() => {
    let cancelled = false;
    const cached = getAttachmentDataUri(decodedPath, sessionId);
    if (cached) {
      setDataUri(cached);
      setFailed(false);
      return;
    }
    setFailed(false);

    void attachmentPort
      .resolveImageSrc(decodedPath, sessionId)
      .then((src) => {
        if (cancelled) return;
        if (src) {
          setAttachmentDataUri(decodedPath, sessionId, src);
          setDataUri(src);
        } else {
          setFailed(true);
        }
      })
      .catch(() => {
        if (!cancelled) setFailed(true);
      });

    return () => {
      cancelled = true;
    };
  }, [attachmentPort, decodedPath, sessionId]);

  if (dataUri) {
    return (
      <img
        src={dataUri}
        alt={alt ?? ""}
        className="al-chat-image"
        onClick={onOpenLightbox ? () => onOpenLightbox(decodedPath) : undefined}
        style={{
          cursor: onOpenLightbox ? "zoom-in" : undefined,
        }}
      />
    );
  }
  if (failed) {
    return <PreviewablePath path={decodedPath} onOpenPreview={onOpenPreview} />;
  }
  return (
    <span
      role="status"
      aria-label={alt || decodedPath}
      className="al-chat-image-loading"
    >
      {decodedPath}
    </span>
  );
}

type BareParagraphOpts = {
  sessionId?: string | null;
  onOpenPreview?: (path: string) => void;
  onOpenLightbox?: (path: string) => void;
  /// 规则 B 总开关：默认关闭，调用方（MarkdownBody 的 autoInlineImagePaths /
  /// LeadSummaryBlock 固定传 true）显式打开才扫描裸路径。不是任何调用方都该
  /// 自动出图——非聊天场景（文档预览 / 更新说明 / worker 子任务标题等）不传
  /// 即保持关闭，避免文本里提到的任意绝对路径被无条件读进来。
  enabled?: boolean;
  /// 流式输出中途关闭规则 B 自动出图：半吐的路径行会闪图 + 空耗一次
  /// read_attachment invoke。`![]()` 语法渲染（localImageMarkdownComponent）
  /// 不受这个开关影响，只影响这条裸路径兜底通道。
  streaming?: boolean;
  /// 传给 <Markdown> 的完整原始 markdown 源文本——配合 mdast/hast 节点的
  /// position.offset 切出「这一段」的原始片段交给 scanImagePaths 扫描。
  /// 调用方需在每次渲染前把它同步进 optsRef（与 sessionId 等字段同款写法）。
  sourceText?: string;
};

/// markdown 段落渲染器（规则 B）：不改动段落原有渲染，按段落在原始 markdown
/// 源里的 position 偏移切出该段原文，交给 scanImagePaths 抽取反引号内 /
/// 句中裸绝对路径 / file:// / <...含空格> 几种形态的本地图片路径，命中的
/// 每条路径在段落下方追加一块 LocalMarkdownImage；`![]()` 语法已产生的路径
/// 由 scanImagePaths 自行跳过，不会重复渲染。renderedPathsRef 是调用方持有
/// 的「本条消息已出过图的路径集合」，用来做跨段落的消息级去重（同一路径在
/// 消息里出现几次只渲一次图），每次消息重新渲染前由调用方清空。
/// 用当前节点在原始 markdown 源里的 position 偏移切出原文，扫描出「本条消息
/// 内还没出过图」的新路径；命中的路径立刻登记进 renderedPathsRef，供后续
/// 段落 / 列表项级去重判断。node 没有 position（如某些插件合成节点）或调用方
/// 没提供 sourceText 时，视为无法安全切片，返回空数组（原样渲染，不出图）。
function scanNewBareImagePaths(
  node:
    | { position?: { start?: { offset?: number }; end?: { offset?: number } } }
    | undefined,
  opts: BareParagraphOpts,
  renderedPathsRef: React.MutableRefObject<Set<string>>,
): string[] {
  const start = node?.position?.start?.offset;
  const end = node?.position?.end?.offset;
  if (start == null || end == null || !opts.sourceText) return [];

  const raw = opts.sourceText.slice(start, end);
  const newPaths = scanImagePaths(raw).filter(
    (path) => !renderedPathsRef.current.has(path),
  );
  newPaths.forEach((path) => renderedPathsRef.current.add(path));
  return newPaths;
}

function BareImageAppend({
  paths,
  opts,
}: {
  paths: string[];
  opts: BareParagraphOpts;
}) {
  return (
    <>
      {paths.map((path) => (
        <LocalMarkdownImage
          key={path}
          path={path}
          sessionId={opts.sessionId}
          onOpenPreview={opts.onOpenPreview}
          onOpenLightbox={opts.onOpenLightbox}
        />
      ))}
    </>
  );
}

export function localImageBareParagraphComponent(
  optsRef: React.MutableRefObject<BareParagraphOpts>,
  renderedPathsRef: React.MutableRefObject<Set<string>>,
) {
  return function MarkdownParagraph({
    children,
    node,
    ...props
  }: React.ComponentProps<"p"> & ExtraProps) {
    const opts = optsRef.current;
    const original = <p {...props}>{children}</p>;
    if (!opts.enabled || opts.streaming) return original;

    const newPaths = scanNewBareImagePaths(node, opts, renderedPathsRef);
    if (newPaths.length === 0) return original;

    // <img> 是行内内容，作为 <p> 的紧邻兄弟元素追加，不破坏 HTML 结构。
    return (
      <>
        {original}
        <BareImageAppend paths={newPaths} opts={opts} />
      </>
    );
  };
}

/// markdown 列表项渲染器（规则 B，列表项版）：CommonMark 紧凑列表（相邻项间
/// 无空行）不会给列表项内容包一层 <p>（remark-rehype 直接把段落内容摊平进
/// <li>），localImageBareParagraphComponent 的 p 组件因此永远不会被调用到——
/// 这里单独接管 li 节点本身的 position 做同一套扫描。<ul>/<ol> 只允许 <li>
/// 做直接子元素，图片块必须嵌在 <li> 内部（而不是像段落那样接成兄弟节点）。
export function localImageBareListItemComponent(
  optsRef: React.MutableRefObject<BareParagraphOpts>,
  renderedPathsRef: React.MutableRefObject<Set<string>>,
) {
  return function MarkdownListItem({
    children,
    node,
    ...props
  }: React.ComponentProps<"li"> & ExtraProps) {
    const opts = optsRef.current;
    if (!opts.enabled || opts.streaming) {
      return <li {...props}>{children}</li>;
    }

    const newPaths = scanNewBareImagePaths(node, opts, renderedPathsRef);
    return (
      <li {...props}>
        {children}
        {newPaths.length > 0 && (
          <BareImageAppend paths={newPaths} opts={opts} />
        )}
      </li>
    );
  };
}

type ImgOpts = {
  sessionId?: string | null;
  onOpenPreview?: (path: string) => void;
  onOpenLightbox?: (path: string) => void;
};

/// markdown 的 img 渲染器：本地相对路径走 read_attachment，其余原样裸渲。
/// 通过 ref 闭包读最新 opts（与 localImageBareParagraphComponent 同款稳定化手法），
/// 调用方可以把返回的组件用 useRef 缓存一次、身份跨渲染保持稳定。
/// 导出以便 LeadSummaryBlock 等自带 components 的渲染点复用同一份逻辑。
export function localImageMarkdownComponent(
  optsRef: React.MutableRefObject<ImgOpts>,
) {
  return function MarkdownImg({
    src,
    alt,
    className,
    node: _node,
    ...props
  }: React.ComponentProps<"img"> & { node?: unknown }) {
    const opts = optsRef.current;
    if (src && isLocalImagePath(src)) {
      return (
        <LocalMarkdownImage
          key={src}
          path={src}
          alt={alt}
          sessionId={opts.sessionId}
          onOpenPreview={opts.onOpenPreview}
          onOpenLightbox={opts.onOpenLightbox}
        />
      );
    }
    return (
      <img
        {...props}
        src={src || undefined}
        alt={alt ?? ""}
        className={className ? `al-chat-image ${className}` : "al-chat-image"}
      />
    );
  };
}

import React, { useEffect, useState } from "react";
import type { ExtraProps, UrlTransform } from "react-markdown";
import {
  getAttachmentDataUri,
  setAttachmentDataUri,
} from "../lib/attachmentCache";
import { useAttachmentPort } from "../lib/attachmentPortContext";

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
function stripFileScheme(url: string): string | null {
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

const BARE_IMAGE_EXT_RE = /\.(png|jpe?g|gif|webp|svg|bmp)$/i;

/// 单行裸路径判定：一行 trim 后若恰是一条本地绝对路径 / file: URL 且后缀为
/// 图片，返回剥壳解码后的路径；否则 null。行内空白（路径与文字混排）一律拒绝。
/// 非 file: 形态额外过 isLocalImagePath，把 `//host/a.png` 协议相对形态这类
/// 「看着像绝对路径其实不是本地文件」拒掉——但仍要求 `/` 开头，不放宽到相对路径
/// （`./a.png`、`assets/x.png` 这类裸路径不自动内联，维持既有语义）。
function bareLocalImagePathToken(text: string): string | null {
  const trimmed = text.trim();
  if (!trimmed || /\s/.test(trimmed)) return null;
  const stripped = stripFileScheme(trimmed);
  const candidate =
    stripped ??
    (trimmed.startsWith("/") && isLocalImagePath(trimmed) ? trimmed : null);
  if (candidate == null) return null;
  return BARE_IMAGE_EXT_RE.test(candidate) ? candidate : null;
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
        onClick={onOpenLightbox ? () => onOpenLightbox(decodedPath) : undefined}
        style={{
          maxWidth: "100%",
          cursor: onOpenLightbox ? "zoom-in" : undefined,
        }}
      />
    );
  }
  if (failed) {
    return <PreviewablePath path={decodedPath} onOpenPreview={onOpenPreview} />;
  }
  return (
    <span role="status" aria-label={alt || decodedPath}>
      {decodedPath}
    </span>
  );
}

type BareParagraphOpts = {
  sessionId?: string | null;
  onOpenPreview?: (path: string) => void;
  onOpenLightbox?: (path: string) => void;
  /// 流式输出中途关闭裸路径自动内联：半吐的路径行会闪图 + 空耗一次
  /// read_attachment invoke。`![]()` 语法渲染（localImageMarkdownComponent）
  /// 不受这个开关影响，只影响这条裸路径兜底通道。
  streaming?: boolean;
};

/// markdown 段落渲染器：段落是单个 text 节点时按 `\n` 按行切分（CommonMark 里
/// 「一句话↵路径」「连续两行两路径」这类无空行场景是同一个 text node 带 `\n`，
/// 不按行切分永远不会命中），逐行判是否为本地图片裸路径——命中的行渲成图，未
/// 命中的行原样保留文本、行序不变；整段无一行命中则原样渲 <p> 不动。命中的图
/// 仍包在 <p> 里（而不是直接顶掉 <p>），让裸路径图与 `![]()` 图上下间距一致。
/// 用于兜底 agent 忘写 ![]() 语法、只在正文裸写图片路径的情况（D5-G3/G4）。
export function localImageBareParagraphComponent(
  optsRef: React.MutableRefObject<BareParagraphOpts>,
) {
  return function MarkdownParagraph({
    children,
    node,
    ...props
  }: React.ComponentProps<"p"> & ExtraProps) {
    const opts = optsRef.current;
    if (opts.streaming) {
      return <p {...props}>{children}</p>;
    }
    const sole = node?.children.length === 1 ? node.children[0] : undefined;
    if (sole?.type !== "text") {
      return <p {...props}>{children}</p>;
    }
    const lines = sole.value.split("\n");
    const linePaths = lines.map((line) => bareLocalImagePathToken(line));
    if (!linePaths.some((path) => path != null)) {
      return <p {...props}>{children}</p>;
    }
    return (
      <p {...props}>
        {lines.map((line, i) => {
          const path = linePaths[i];
          const content =
            path != null ? (
              <LocalMarkdownImage
                key={`img-${i}`}
                path={path}
                sessionId={opts.sessionId}
                onOpenPreview={opts.onOpenPreview}
                onOpenLightbox={opts.onOpenLightbox}
              />
            ) : (
              <React.Fragment key={`txt-${i}`}>{line}</React.Fragment>
            );
          return i === 0 ? (
            content
          ) : (
            <React.Fragment key={`ln-${i}`}>
              <br />
              {content}
            </React.Fragment>
          );
        })}
      </p>
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
    style,
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
        style={{ ...style, maxWidth: "100%" }}
      />
    );
  };
}

import React, { useEffect, useState } from "react";
import type { UrlTransform } from "react-markdown";
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

/// react-markdown 的 urlTransform 工厂：本地路径豁免只作用于 <img src>（配合
/// localImageMarkdownComponent 使用），其余一切 URL（含 <a href>）一律交给调用方
/// 传入的 react-markdown 默认消毒器处理。用于避免「图片本地路径豁免」被误套用到
/// 链接 href 上、放行盘符形态（`C:\...`）或伪装盘符（`j:%5C...`）的路径。
export function makeImgOnlyUrlTransform(
  defaultUrlTransform: (url: string) => string,
): UrlTransform {
  return (url, key, node) => {
    if (key !== "src" || node?.tagName !== "img") {
      return defaultUrlTransform(url);
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

/// markdown 的 img 渲染器：本地相对路径走 read_attachment，其余原样裸渲。
/// 导出以便 LeadSummaryBlock 等自带 components 的渲染点复用同一份逻辑。
export function localImageMarkdownComponent(opts: {
  sessionId?: string | null;
  onOpenPreview?: (path: string) => void;
  onOpenLightbox?: (path: string) => void;
}) {
  return function MarkdownImg({
    src,
    alt,
    style,
    node: _node,
    ...props
  }: React.ComponentProps<"img"> & { node?: unknown }) {
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

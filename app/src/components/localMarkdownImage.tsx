import { invoke } from "@tauri-apps/api/core";
import React, { useEffect, useState } from "react";
import {
  getAttachmentDataUri,
  setAttachmentDataUri,
} from "../lib/attachmentCache";

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

function decodeLocalImagePath(path: string): string {
  try {
    return decodeURI(path);
  } catch {
    return path;
  }
}

type AttachmentContent = {
  kind: "text" | "image" | "binary";
  imageBase64?: string;
  mediaType?: string;
};

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

    void invoke<AttachmentContent>("read_attachment", {
      path: decodedPath,
      sessionId: sessionId ?? null,
    })
      .then((attachment) => {
        if (cancelled) return;
        if (attachment.imageBase64 && attachment.mediaType) {
          const nextDataUri = `data:${attachment.mediaType};base64,${attachment.imageBase64}`;
          setAttachmentDataUri(decodedPath, sessionId, nextDataUri);
          setDataUri(nextDataUri);
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
  }, [decodedPath, sessionId]);

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

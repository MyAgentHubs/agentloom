import { invoke } from "@tauri-apps/api/core";
import React, { useEffect, useMemo, useState } from "react";
import { createPortal } from "react-dom";
import Markdown, { defaultUrlTransform } from "react-markdown";
import remarkGfm from "remark-gfm";
import { openUrl } from "@tauri-apps/plugin-opener";
import { CodeBlock } from "./CodeBlock";
import { MermaidBlock } from "./MermaidBlock";
import { useI18n } from "../i18n";
import {
  getAttachmentDataUri,
  setAttachmentDataUri,
} from "../lib/attachmentCache";
import { renderBackendError } from "../lib/backendMsg";

// 内联代码若形如「带已知可预览后缀的文件路径」→ 可点开预览。
// 要求：无空白/反引号/圆括号（排掉 array.map()、foo.bar() 这类），且以已知后缀结尾。
const PREVIEWABLE_PATH =
  /^[^\s`()]+\.(md|markdown|mdx|txt|log|svg|png|jpe?g|gif|webp|bmp|ico|html?|json|ya?ml|toml|ini|cfg|conf|xml|csv|tsx?|jsx?|mjs|cjs|py|rs|go|java|kt|rb|php|c|cc|cpp|h|hpp|cs|swift|sh|bash|zsh|sql|css|scss|less|vue|svelte)$/i;
function isPreviewablePath(s: string): boolean {
  return s.length <= 512 && PREVIEWABLE_PATH.test(s);
}

function isLocalPreviewablePath(path: string): boolean {
  if (!isPreviewablePath(path)) return false;
  // 排除 mailto:、javascript: 等 URI scheme，同时保留 Windows 盘符路径。
  return !/^[a-z][a-z\d+.-]*:/i.test(path) || /^[a-z]:[\\/]/i.test(path);
}

function isHtmlPath(path: string): boolean {
  return /\.html?$/i.test(path);
}

function decodeFilePath(path: string): string {
  try {
    return decodeURIComponent(path);
  } catch {
    return path;
  }
}

function isLocalImagePath(src: string): boolean {
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

function PreviewablePath({
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

function LocalMarkdownImage({
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

type Props = {
  children: string;
  streaming: boolean;
  onOpenPreview?: (path: string) => void;
  onOpenLightbox?: (path: string) => void;
  sessionId?: string | null;
};

export const MarkdownBody = React.memo(function MarkdownBody({
  children,
  streaming,
  onOpenPreview,
  onOpenLightbox,
  sessionId,
}: Props) {
  const { t } = useI18n();
  const [attachmentOpenError, setAttachmentOpenError] = useState<string | null>(
    null,
  );

  useEffect(() => {
    if (!attachmentOpenError) return;
    const timeout = window.setTimeout(() => setAttachmentOpenError(null), 3000);
    return () => window.clearTimeout(timeout);
  }, [attachmentOpenError]);

  const components = useMemo(
    () => ({
      a({ children, href }: React.ComponentProps<"a">) {
        const external = !!href && /^https?:\/\//i.test(href);
        return (
          <a
            href={href}
            onClick={(event) => {
              event.preventDefault();
              if (external) {
                void openUrl(href).catch(() => {});
                return;
              }
              if (!href || !isLocalPreviewablePath(href)) return;

              const decodedPath = decodeFilePath(href);
              if (isHtmlPath(decodedPath)) {
                void invoke("open_attachment_external", {
                  sessionId: sessionId ?? null,
                  path: decodedPath,
                }).catch((error) => {
                  setAttachmentOpenError(renderBackendError(error, t));
                });
                return;
              }
              onOpenPreview?.(decodedPath);
            }}
          >
            {children}
          </a>
        );
      },
      code({ className, children, ...props }: React.ComponentProps<"code">) {
        const match = /language-([^\s]+)/.exec(className ?? "");
        const raw = String(children).replace(/\n$/, "");
        if (match) {
          if (match[1] === "mermaid")
            return <MermaidBlock code={raw} complete={!streaming} />;
          return <CodeBlock code={raw} lang={match[1]} />;
        }
        if (onOpenPreview && isPreviewablePath(raw)) {
          return <PreviewablePath path={raw} onOpenPreview={onOpenPreview} />;
        }
        return (
          <code className="inline" {...props}>
            {children}
          </code>
        );
      },
      img({
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
              sessionId={sessionId}
              onOpenPreview={onOpenPreview}
              onOpenLightbox={onOpenLightbox}
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
      },
      table({ children }: React.ComponentProps<"table">) {
        return (
          <div className="mm-table-wrap">
            <table>{children}</table>
          </div>
        );
      },
      td({ children, style, ...props }: React.ComponentProps<"td">) {
        return (
          <td {...props} style={{ ...style, textAlign: "left" }}>
            {children}
          </td>
        );
      },
      th({ children, style, ...props }: React.ComponentProps<"th">) {
        return (
          <th {...props} style={{ ...style, textAlign: "left" }}>
            {children}
          </th>
        );
      },
    }),
    [onOpenLightbox, onOpenPreview, sessionId, streaming, t],
  );

  return (
    <>
      <Markdown
        remarkPlugins={[remarkGfm]}
        skipHtml={true}
        urlTransform={(url) =>
          isLocalImagePath(url) ? url : defaultUrlTransform(url)
        }
        components={components}
      >
        {children}
      </Markdown>
      {attachmentOpenError &&
        createPortal(
          <div className="toast" role="status" aria-label={attachmentOpenError}>
            {attachmentOpenError}
          </div>,
          document.body,
        )}
    </>
  );
});

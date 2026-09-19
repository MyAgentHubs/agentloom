import React, { useEffect, useMemo, useRef, useState } from "react";
import { createPortal } from "react-dom";
import Markdown, { defaultUrlTransform } from "react-markdown";
import remarkGfm from "remark-gfm";
import { CodeBlock } from "./CodeBlock";
import { MermaidBlock } from "./MermaidBlock";
import { useI18n } from "../i18n";
import {
  localImageBareListItemComponent,
  localImageBareParagraphComponent,
  localImageMarkdownComponent,
  makeImgOnlyUrlTransform,
  PreviewablePath,
} from "./localMarkdownImage";
import { renderBackendError } from "../lib/backendMsg";
import { useAttachmentPort } from "../lib/attachmentPortContext";

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

type Props = {
  children: string;
  streaming: boolean;
  onOpenPreview?: (path: string) => void;
  onOpenLightbox?: (path: string) => void;
  sessionId?: string | null;
  /// 规则 B（正文里裸写的绝对路径自动出图）总开关，默认关闭。绝对路径不受
  /// sessionId/工作区边界限制（read_attachment 对绝对路径直接放行），只有
  /// 聊天流里 assistant 消息的正文渲染点才该打开——其余消费方（仓库文档 /
  /// 更新说明 / worker 子任务标题等）保持默认关，避免任意来源文本里提到的
  /// 一句绝对路径就被无条件读盘渲图。
  autoInlineImagePaths?: boolean;
};

export const MarkdownBody = React.memo(function MarkdownBody({
  children,
  streaming,
  onOpenPreview,
  onOpenLightbox,
  sessionId,
  autoInlineImagePaths = false,
}: Props) {
  const { t } = useI18n();
  const attachmentPort = useAttachmentPort();
  const [attachmentOpenError, setAttachmentOpenError] = useState<string | null>(
    null,
  );
  const bareParagraphOptsRef = useRef({
    sessionId,
    onOpenPreview,
    onOpenLightbox,
    streaming,
    sourceText: children,
    enabled: autoInlineImagePaths,
  });
  bareParagraphOptsRef.current = {
    sessionId,
    onOpenPreview,
    onOpenLightbox,
    streaming,
    sourceText: children,
    enabled: autoInlineImagePaths,
  };
  // 规则 B 的消息级去重集合：同一条消息内同一路径只出一次图，每次渲染
  // （即这条消息内容变化）重置。
  const renderedImagePathsRef = useRef(new Set<string>());
  renderedImagePathsRef.current = new Set<string>();
  const bareParagraphComponent = useRef(
    localImageBareParagraphComponent(
      bareParagraphOptsRef,
      renderedImagePathsRef,
    ),
  ).current;
  const bareListItemComponent = useRef(
    localImageBareListItemComponent(
      bareParagraphOptsRef,
      renderedImagePathsRef,
    ),
  ).current;

  const imgOptsRef = useRef({ sessionId, onOpenPreview, onOpenLightbox });
  imgOptsRef.current = { sessionId, onOpenPreview, onOpenLightbox };
  const imgComponent = useRef(localImageMarkdownComponent(imgOptsRef)).current;

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
                void attachmentPort.openUrl(href).catch(() => {});
                return;
              }
              if (!href || !isLocalPreviewablePath(href)) return;

              const decodedPath = decodeFilePath(href);
              if (isHtmlPath(decodedPath)) {
                void attachmentPort
                  .openExternal(decodedPath, sessionId ?? null)
                  .catch((error) => {
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
      img: imgComponent,
      p: bareParagraphComponent,
      li: bareListItemComponent,
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
    [
      attachmentPort,
      bareParagraphComponent,
      bareListItemComponent,
      imgComponent,
      onOpenPreview,
      sessionId,
      streaming,
      t,
    ],
  );

  return (
    <>
      <Markdown
        remarkPlugins={[remarkGfm]}
        skipHtml={true}
        urlTransform={makeImgOnlyUrlTransform(defaultUrlTransform)}
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

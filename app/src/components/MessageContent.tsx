import { invoke } from "@tauri-apps/api/core";
import {
  Fragment,
  memo,
  type KeyboardEvent as ReactKeyboardEvent,
  type MouseEvent as ReactMouseEvent,
  type ReactNode,
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import { createPortal } from "react-dom";
import type { Block } from "../types/agent";
import { groupToolBlocks } from "../lib/streamItems";
import { useMarkdown } from "../lib/useMarkdown";
import { LeadSummaryBlock } from "./LeadSummaryBlock";
import { ThinkingBlock } from "./ThinkingBlock";
import { ToolCard } from "./ToolCard";
import { RunCard } from "./RunCard";
import { GateCard } from "./GateCard";
import { DraftFailedCard } from "./DraftFailedCard";
import { BackgroundTaskStack } from "./BackgroundTaskStack";
import { CodingTaskBar } from "./CodingTaskBar";
import { DispatchCard } from "./DispatchCard";
import { ToolStepsFold } from "./ToolStepsFold";
import { ApprovalCard } from "./ApprovalCard";
import { ScopeChangeCard } from "./ScopeChangeCard";
import { RunTerminalCard } from "./RunTerminalCard";
import { useI18n } from "../i18n";
import {
  getAttachmentDataUri,
  setAttachmentDataUri,
} from "../lib/attachmentCache";
import { renderBackendError } from "../lib/backendMsg";

type Props = {
  blocks: Block[];
  streaming?: boolean;
  onViewRun?: (runId?: string) => void;
  onUndoRun?: (runId: string) => void;
  onOpenPreview?: (path: string) => void;
  onOpenLightbox?: (path: string) => void;
  onOpenMember?: (runId: string, assignmentId: string) => void;
  sessionId?: string | null;
  onTakeOver?: () => void;
  onCleanRedispatch?: (runId: string) => void;
  gateView?: import("../lib/gateView").GateView | null;
  leadName?: string;
  enabledAgents?: import("../types/agent").AgentProfile[];
  onGateAction?: (a: import("../lib/gateReducer").GateAction) => void;
  onGateFreeze?: () => void;
  onGateRedraft?: () => void;
  onGateRetry?: () => void;
  onGateManual?: () => void;
  onGateBackToNormal?: () => void;
  /** 冻结发起链 in-flight（P2-1·透传给 GateCard 禁用主按钮）。 */
  gateFreezing?: boolean;
  onConfirmVerify?: (runId: string, cmd: string) => void;
  onRetryVerify?: (runId: string) => void;
  onShelve?: (runId: string) => void;
  onContinueScope?: (text: string) => void;
  onOpenInspector?: (assignmentId: string) => void;
  readonlyReason?: string | null;
};

type AttachmentContent = {
  kind: "text" | "image" | "binary";
  imageBase64?: string;
  mediaType?: string;
};

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
  "file",
]);

function imagePathsFromTool(block: Extract<Block, { type: "tool" }>): string[] {
  if (SEARCH_TOOLS.has(block.tool) || CONTENT_TOOLS.has(block.tool)) return [];
  const tokens = `${block.summary}\n${block.output ?? ""}`.split(
    IMAGE_PATH_TOKEN_BOUNDARY,
  );
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
  return [...new Set(paths)].slice(0, MAX_IMAGE_PATHS_PER_TOOL_BLOCK);
}

function fileName(path: string): string {
  return path.split(/[\\/]/).pop() || path;
}

function isRelativeImagePath(path: string): boolean {
  return (
    !path.startsWith("/") &&
    !path.startsWith("~/") &&
    !/^[A-Za-z]:[\\/]/.test(path)
  );
}

function isHtmlPath(path: string): boolean {
  return /\.html?$/i.test(path);
}

function mediaTypeFromPath(path: string): string {
  const extension = path.split(".").pop()?.toLowerCase();
  if (extension === "jpg" || extension === "jpeg") return "image/jpeg";
  if (extension === "svg") return "image/svg+xml";
  if (extension === "webp") return "image/webp";
  if (extension === "gif") return "image/gif";
  if (extension === "bmp") return "image/bmp";
  return "image/png";
}

function useAttachmentImage(
  path: string,
  fallbackMediaType: string,
  sessionId?: string | null,
) {
  const [dataUri, setDataUri] = useState<string | null>(() =>
    getAttachmentDataUri(path, sessionId),
  );
  const [failed, setFailed] = useState(false);

  useEffect(() => {
    let cancelled = false;
    const cached = getAttachmentDataUri(path, sessionId);
    if (cached) {
      setDataUri(cached);
      setFailed(false);
      return;
    }
    setFailed(false);

    void invoke<AttachmentContent>("read_attachment", {
      path,
      sessionId: sessionId ?? null,
    })
      .then((attachment) => {
        if (cancelled) return;
        if (attachment.imageBase64) {
          const mediaType = attachment.mediaType || fallbackMediaType;
          const nextDataUri = `data:${mediaType};base64,${attachment.imageBase64}`;
          setAttachmentDataUri(path, sessionId, nextDataUri);
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
  }, [fallbackMediaType, path, sessionId]);

  return { dataUri, failed };
}

function dataUriToBlob(dataUri: string): Blob {
  const commaIndex = dataUri.indexOf(",");
  if (commaIndex < 0) throw new Error("Invalid image data URI");

  const metadata = dataUri.slice(0, commaIndex);
  const mediaType = metadata.match(/^data:([^;,]+)/)?.[1] || "image/png";
  const encoded = dataUri.slice(commaIndex + 1);
  const decoded = metadata.includes(";base64")
    ? atob(encoded)
    : decodeURIComponent(encoded);
  const bytes = Uint8Array.from(decoded, (character) =>
    character.charCodeAt(0),
  );
  return new Blob([bytes], { type: mediaType });
}

type ImageContextTriggerProps = {
  onContextMenu: (event: ReactMouseEvent<HTMLElement>) => void;
  onKeyDown: (event: ReactKeyboardEvent<HTMLElement>) => void;
};

const IMAGE_MENU_WIDTH = 180;
const IMAGE_MENU_HEIGHT = 96;
const IMAGE_MENU_GAP = 8;

export function computeImageMenuPosition(
  rect: Pick<DOMRect, "left" | "right" | "top" | "bottom">,
  cursor: { x: number; y: number } | undefined,
  viewport: { width: number; height: number },
): { left: number; top: number } {
  const clampLeft = (left: number) =>
    Math.max(
      IMAGE_MENU_GAP,
      Math.min(left, viewport.width - IMAGE_MENU_WIDTH - IMAGE_MENU_GAP),
    );
  const clampTop = (top: number) =>
    Math.max(
      IMAGE_MENU_GAP,
      Math.min(top, viewport.height - IMAGE_MENU_HEIGHT - IMAGE_MENU_GAP),
    );
  const right = rect.right + IMAGE_MENU_GAP;
  const left = rect.left - IMAGE_MENU_WIDTH - IMAGE_MENU_GAP;

  if (
    !cursor &&
    right + IMAGE_MENU_WIDTH > viewport.width - IMAGE_MENU_GAP &&
    left < IMAGE_MENU_GAP
  ) {
    const below = rect.bottom + IMAGE_MENU_GAP;
    const above = rect.top - IMAGE_MENU_HEIGHT - IMAGE_MENU_GAP;
    const verticalAnchor =
      below + IMAGE_MENU_HEIGHT <= viewport.height - IMAGE_MENU_GAP
        ? below
        : above >= IMAGE_MENU_GAP
          ? above
          : rect.top + 12;

    return {
      left: clampLeft(rect.left),
      top: clampTop(verticalAnchor),
    };
  }

  const horizontalAnchor =
    right + IMAGE_MENU_WIDTH <= viewport.width - IMAGE_MENU_GAP
      ? right
      : left >= IMAGE_MENU_GAP
        ? left
        : (cursor?.x ?? rect.left + 12);

  return {
    left: clampLeft(horizontalAnchor),
    top: clampTop(cursor?.y ?? rect.top + 12),
  };
}

function ImageContextTarget({
  path,
  dataUri,
  children,
}: {
  path: string;
  dataUri: string;
  children: (props: ImageContextTriggerProps) => ReactNode;
}) {
  const { t } = useI18n();
  const menuRef = useRef<HTMLDivElement>(null);
  const [menuPosition, setMenuPosition] = useState<{
    left: number;
    top: number;
  } | null>(null);
  const [feedback, setFeedback] = useState<string | null>(null);
  const clipboard =
    typeof navigator === "undefined" ? undefined : navigator.clipboard;
  const canCopyImage =
    typeof clipboard?.write === "function" &&
    typeof ClipboardItem !== "undefined";

  useEffect(() => {
    if (!menuPosition) return;

    menuRef.current
      ?.querySelector<HTMLButtonElement>("button:not(:disabled)")
      ?.focus();
    const closeOnOutsidePress = (event: PointerEvent) => {
      if (!menuRef.current?.contains(event.target as Node)) {
        setMenuPosition(null);
      }
    };
    const closeOnOutsideContextMenu = (event: MouseEvent) => {
      if (!menuRef.current?.contains(event.target as Node)) {
        setMenuPosition(null);
      }
    };
    const closeOnEscape = (event: KeyboardEvent) => {
      if (event.key === "Escape") setMenuPosition(null);
    };
    document.addEventListener("pointerdown", closeOnOutsidePress);
    document.addEventListener("contextmenu", closeOnOutsideContextMenu, true);
    document.addEventListener("keydown", closeOnEscape);
    return () => {
      document.removeEventListener("pointerdown", closeOnOutsidePress);
      document.removeEventListener(
        "contextmenu",
        closeOnOutsideContextMenu,
        true,
      );
      document.removeEventListener("keydown", closeOnEscape);
    };
  }, [menuPosition]);

  useEffect(() => {
    if (!feedback) return;
    const timeout = window.setTimeout(() => setFeedback(null), 2000);
    return () => window.clearTimeout(timeout);
  }, [feedback]);

  const openMenu = (
    rect: Pick<DOMRect, "left" | "right" | "top" | "bottom">,
    cursor?: { x: number; y: number },
  ) => {
    setFeedback(null);
    setMenuPosition(
      computeImageMenuPosition(rect, cursor, {
        width: window.innerWidth,
        height: window.innerHeight,
      }),
    );
  };
  const triggerProps: ImageContextTriggerProps = {
    onContextMenu: (event) => {
      event.preventDefault();
      event.stopPropagation();
      openMenu(event.currentTarget.getBoundingClientRect(), {
        x: event.clientX,
        y: event.clientY,
      });
    },
    onKeyDown: (event) => {
      if (
        event.key !== "ContextMenu" &&
        !(event.shiftKey && event.key === "F10")
      )
        return;
      event.preventDefault();
      const rect = event.currentTarget.getBoundingClientRect();
      openMenu(rect);
    },
  };

  const copyPath = async () => {
    setMenuPosition(null);
    try {
      if (typeof clipboard?.writeText !== "function") {
        throw new Error("Clipboard text API unavailable");
      }
      await clipboard.writeText(path);
      setFeedback(t("messageContent.imageMenu.pathCopied"));
    } catch {
      setFeedback(t("messageContent.imageMenu.copyFailed"));
    }
  };

  const copyImage = async () => {
    setMenuPosition(null);
    try {
      if (!canCopyImage) throw new Error("Clipboard image API unavailable");
      const blob = dataUriToBlob(dataUri);
      await clipboard.write([new ClipboardItem({ [blob.type]: blob })]);
      setFeedback(t("messageContent.imageMenu.imageCopied"));
    } catch {
      setFeedback(t("messageContent.imageMenu.copyFailed"));
    }
  };

  const menuItemStyle = {
    width: "100%",
    padding: "7px 10px",
    border: 0,
    borderRadius: 5,
    background: "transparent",
    color: "var(--ink-2)",
    cursor: "pointer",
    font: "inherit",
    fontSize: 12,
    textAlign: "left" as const,
  };

  return (
    <Fragment>
      {children(triggerProps)}
      {menuPosition &&
        createPortal(
          <div
            ref={menuRef}
            role="menu"
            aria-label={t("messageContent.imageMenu.label")}
            style={{
              position: "fixed",
              left: menuPosition.left,
              top: menuPosition.top,
              zIndex: 1000,
              width: IMAGE_MENU_WIDTH,
              boxSizing: "border-box",
              padding: 4,
              border: "1px solid var(--line)",
              borderRadius: 7,
              background: "var(--panel)",
              boxShadow: "0 8px 24px rgba(72, 54, 35, 0.16)",
            }}
          >
            <button
              type="button"
              role="menuitem"
              disabled={!canCopyImage}
              title={
                canCopyImage
                  ? undefined
                  : t("messageContent.imageMenu.imageUnavailable")
              }
              onClick={() => void copyImage()}
              style={{
                ...menuItemStyle,
                cursor: canCopyImage ? "pointer" : "not-allowed",
                opacity: canCopyImage ? 1 : 0.5,
              }}
            >
              {t("messageContent.imageMenu.copyImage")}
            </button>
            <button
              type="button"
              role="menuitem"
              onClick={() => void copyPath()}
              style={menuItemStyle}
            >
              {t("messageContent.imageMenu.copyPath")}
            </button>
          </div>,
          document.body,
        )}
      {feedback &&
        createPortal(
          <span
            role="status"
            style={{
              position: "fixed",
              right: 18,
              bottom: 18,
              zIndex: 1000,
              padding: "6px 10px",
              border: "1px solid var(--line)",
              borderRadius: 7,
              background: "var(--panel)",
              color: "var(--ink-2)",
              boxShadow: "0 6px 18px rgba(72, 54, 35, 0.14)",
              fontSize: 12,
            }}
          >
            {feedback}
          </span>,
          document.body,
        )}
    </Fragment>
  );
}

function PreviewableImagePath({
  path,
  onOpenPreview,
}: {
  path: string;
  onOpenPreview: (path: string) => void;
}) {
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

function ImageArtifactChips({
  paths,
  sessionId,
  onOpenPreview,
  onOpenLightbox,
}: {
  paths: string[];
  sessionId?: string | null;
  onOpenPreview: (path: string) => void;
  onOpenLightbox: (path: string) => void;
}) {
  const [contentByPath, setContentByPath] = useState<Record<string, string>>(
    {},
  );
  const onContent = useCallback((path: string, base64: string) => {
    setContentByPath((current) =>
      current[path] === base64 ? current : { ...current, [path]: base64 },
    );
  }, []);
  const winnerByContent = useMemo(() => {
    const winners = new Map<string, string>();
    paths.forEach((path) => {
      const content = contentByPath[path];
      if (!content) return;
      const current = winners.get(content);
      if (
        !current ||
        isRelativeImagePath(path) ||
        !isRelativeImagePath(current)
      ) {
        winners.set(content, path);
      }
    });
    return winners;
  }, [contentByPath, paths]);

  return (
    <div
      style={{
        display: "flex",
        flexWrap: "wrap",
        alignItems: "flex-start",
        gap: 6,
        marginTop: 6,
      }}
    >
      {paths.map((path) => (
        <ImageArtifactThumbnail
          key={path}
          path={path}
          sessionId={sessionId}
          onOpenPreview={onOpenPreview}
          onOpenLightbox={onOpenLightbox}
          onContent={onContent}
          hidden={
            contentByPath[path] != null &&
            winnerByContent.get(contentByPath[path]) !== path
          }
        />
      ))}
    </div>
  );
}

function ImageArtifactThumbnail({
  path,
  sessionId,
  onOpenPreview,
  onOpenLightbox,
  onContent,
  hidden,
}: {
  path: string;
  sessionId?: string | null;
  onOpenPreview: (path: string) => void;
  onOpenLightbox: (path: string) => void;
  onContent?: (path: string, base64: string) => void;
  hidden?: boolean;
}) {
  const { t } = useI18n();
  const name = fileName(path);
  const { dataUri, failed } = useAttachmentImage(
    path,
    mediaTypeFromPath(path),
    sessionId,
  );

  useEffect(() => {
    if (!dataUri || !onContent) return;
    const commaIndex = dataUri.indexOf(",");
    if (commaIndex >= 0) onContent(path, dataUri.slice(commaIndex + 1));
  }, [dataUri, onContent, path]);

  if (failed) {
    return <PreviewableImagePath path={path} onOpenPreview={onOpenPreview} />;
  }

  if (hidden) return null;

  if (!dataUri) {
    return (
      <div
        role="status"
        title={path}
        style={{
          boxSizing: "border-box",
          width: 160,
          maxWidth: "100%",
          minHeight: 96,
          padding: 10,
          border: "1px solid var(--line)",
          borderRadius: 8,
          background: "var(--panel)",
          color: "var(--ink-3)",
          display: "flex",
          flexDirection: "column",
          justifyContent: "flex-end",
          gap: 4,
          fontSize: 11,
        }}
      >
        <span style={{ color: "var(--ink-2)", overflowWrap: "anywhere" }}>
          {name}
        </span>
        <span>{t("messageContent.imageLoading")}</span>
      </div>
    );
  }

  return (
    <ImageContextTarget path={path} dataUri={dataUri}>
      {(contextProps) => (
        <button
          type="button"
          title={path}
          aria-label={t("messageContent.imageArtifact.preview", { name })}
          aria-haspopup="menu"
          onClick={() => onOpenLightbox(path)}
          {...contextProps}
          style={{
            boxSizing: "border-box",
            maxWidth: "100%",
            padding: 0,
            border: "1px solid var(--line)",
            borderRadius: 8,
            overflow: "hidden",
            background: "var(--panel)",
            color: "var(--ink-2)",
            cursor: "pointer",
            display: "inline-flex",
            flexDirection: "column",
            alignItems: "stretch",
          }}
        >
          <img
            src={dataUri}
            alt={name}
            style={{
              display: "block",
              maxHeight: 240,
              maxWidth: "100%",
              objectFit: "contain",
              height: "auto",
              cursor: "pointer",
            }}
          />
          <span
            style={{
              padding: "4px 7px",
              fontSize: 11,
              lineHeight: 1.3,
              textAlign: "left",
              overflowWrap: "anywhere",
            }}
          >
            {name}
          </span>
        </button>
      )}
    </ImageContextTarget>
  );
}

function ImageBlockContent({
  path,
  mediaType,
  sessionId,
  onOpenPreview,
  onOpenLightbox,
}: {
  path: string;
  mediaType: string;
  sessionId?: string | null;
  onOpenPreview?: (path: string) => void;
  onOpenLightbox?: (path: string) => void;
}) {
  const { t } = useI18n();
  const { dataUri, failed } = useAttachmentImage(path, mediaType, sessionId);

  if (dataUri) {
    return (
      <ImageContextTarget path={path} dataUri={dataUri}>
        {(contextProps) => (
          <img
            src={dataUri}
            alt={fileName(path)}
            tabIndex={0}
            aria-haspopup="menu"
            onClick={() => onOpenLightbox?.(path)}
            {...contextProps}
            style={{
              display: "block",
              maxHeight: 240,
              maxWidth: "100%",
              objectFit: "contain",
              height: "auto",
              cursor: onOpenLightbox ? "zoom-in" : undefined,
            }}
          />
        )}
      </ImageContextTarget>
    );
  }
  if (failed) {
    return onOpenPreview ? (
      <PreviewableImagePath path={path} onOpenPreview={onOpenPreview} />
    ) : (
      <em>{t("messageContent.imageLoadFailed")}</em>
    );
  }
  return <em role="status">{t("messageContent.imageLoading")}</em>;
}

function MessageContentImpl({
  blocks,
  streaming = false,
  onViewRun,
  onUndoRun,
  onOpenPreview,
  onOpenLightbox,
  onOpenMember,
  onTakeOver,
  onCleanRedispatch,
  gateView,
  leadName,
  enabledAgents,
  onGateAction,
  onGateFreeze,
  onGateRedraft,
  onGateRetry,
  onGateManual,
  onGateBackToNormal,
  gateFreezing,
  sessionId,
  onConfirmVerify,
  onRetryVerify,
  onShelve,
  onContinueScope,
  onOpenInspector,
  readonlyReason,
}: Props) {
  const { t } = useI18n();
  const MarkdownBody = useMarkdown();
  const contentRef = useRef<HTMLDivElement>(null);
  const [attachmentOpenError, setAttachmentOpenError] = useState<string | null>(
    null,
  );
  const readonly = readonlyReason != null;
  const grouped = useMemo(() => {
    const items = groupToolBlocks(blocks);
    const pathsByItem = items.map((item) => {
      if (item.kind === "toolgroup") {
        return item.blocks.flatMap(imagePathsFromTool);
      }
      return item.block.type === "tool" ? imagePathsFromTool(item.block) : [];
    });
    const allPaths = [...new Set(pathsByItem.flat())];
    const preferredPaths = new Set(
      allPaths.filter(
        (path) => !allPaths.some((other) => other.endsWith(`/${path}`)),
      ),
    );
    const seen = new Set<string>();
    return items.map((item, index) => {
      const fresh: string[] = [];
      pathsByItem[index].forEach((path) => {
        if (!preferredPaths.has(path) || seen.has(path)) return;
        seen.add(path);
        fresh.push(path);
      });
      return { item, imagePaths: fresh };
    });
  }, [blocks]);

  useEffect(() => {
    const buttons = contentRef.current?.querySelectorAll<HTMLElement>(
      "code.inline-path[role='button']",
    );
    buttons?.forEach((button) => {
      const path = button.textContent ?? "";
      if (!isHtmlPath(path)) {
        button.removeAttribute("aria-label");
        button.setAttribute("title", path);
        return;
      }
      const label = t("messageContent.html.openExternal", {
        name: fileName(path),
      });
      button.setAttribute("aria-label", label);
      button.setAttribute("title", label);
    });
  }, [MarkdownBody, grouped, t]);

  useEffect(() => {
    if (!attachmentOpenError) return;
    const timeout = window.setTimeout(() => setAttachmentOpenError(null), 3000);
    return () => window.clearTimeout(timeout);
  }, [attachmentOpenError]);

  const openPreviewOrExternal = onOpenPreview
    ? (path: string) => {
        if (!isHtmlPath(path)) {
          onOpenPreview(path);
          return;
        }
        void invoke("open_attachment_external", {
          sessionId: sessionId ?? null,
          path,
        }).catch((error) => {
          setAttachmentOpenError(renderBackendError(error, t));
        });
      }
    : undefined;

  return (
    <div className="turn__text" ref={contentRef}>
      {grouped.map(({ item, imagePaths }, i) => {
        if (item.kind === "toolgroup") {
          const groupKey = item.blocks[0]?.id ?? `toolgroup-${i}`;
          return (
            <Fragment key={`toolgroup-${groupKey}`}>
              <ToolStepsFold blocks={item.blocks} />
              {onOpenPreview && onOpenLightbox && imagePaths.length > 0 && (
                <ImageArtifactChips
                  paths={imagePaths}
                  sessionId={sessionId}
                  onOpenPreview={onOpenPreview}
                  onOpenLightbox={onOpenLightbox}
                />
              )}
            </Fragment>
          );
        }
        const block = item.block;
        if (block.type === "image")
          return (
            <ImageBlockContent
              key={`b-${i}`}
              path={block.attachment_id}
              mediaType={block.media_type}
              sessionId={sessionId}
              onOpenPreview={onOpenPreview}
              onOpenLightbox={onOpenLightbox}
            />
          );
        if (block.type === "tool") {
          return (
            <Fragment key={`b-${i}`}>
              <ToolCard block={block} compact />
              {onOpenPreview && onOpenLightbox && imagePaths.length > 0 && (
                <ImageArtifactChips
                  paths={imagePaths}
                  sessionId={sessionId}
                  onOpenPreview={onOpenPreview}
                  onOpenLightbox={onOpenLightbox}
                />
              )}
            </Fragment>
          );
        }
        if (block.type === "approval")
          return (
            <ApprovalCard
              key={`b-${i}`}
              block={block}
              sessionId={sessionId ?? ""}
            />
          );
        if (block.type === "thinking")
          return <ThinkingBlock key={`b-${i}`} text={block.text} />;
        if (block.type === "team_run")
          return (
            <BackgroundTaskStack
              key={`b-${i}`}
              runId={block.run_id}
              lead={block.lead}
              members={block.members}
              onOpenMember={onOpenMember}
              onUndoRun={onUndoRun}
            />
          );
        if (block.type === "gate_card" && gateView?.kind === "proposing")
          return (
            <div className="gate-proposing" key={`b-${i}`}>
              <span className="gate-proposing__dot" aria-hidden />
              {t("messageContent.gate.proposing")}
            </div>
          );
        if (block.type === "gate_card" && gateView?.kind === "draft")
          return (
            <GateCard
              key={`b-${i}`}
              draft={gateView.draft}
              leadName={leadName ?? "Lead"}
              enabledAgents={enabledAgents ?? []}
              onAction={(a) => onGateAction?.(a)}
              onFreeze={() => onGateFreeze?.()}
              onRedraft={() => onGateRedraft?.()}
              freezing={gateFreezing}
              readonlyReason={readonlyReason}
            />
          );
        if (block.type === "draft_failed" && gateView?.kind === "failed")
          return (
            <DraftFailedCard
              key={`b-${i}`}
              failure={gateView.failure}
              onRetry={() => onGateRetry?.()}
              onManual={() => onGateManual?.()}
              onBackToNormal={() => onGateBackToNormal?.()}
              disabled={readonly}
            />
          );
        if (block.type === "gate_card" || block.type === "draft_failed")
          return null; // gateView 不匹配（已清）→ 不渲
        // plan B3：内联变更卡——「查看」透传 onViewRun（App 里开右面板 Review tab）。
        if (block.type === "run_card")
          return (
            <RunCard
              key={`b-${i}`}
              block={block}
              onView={() => onViewRun?.()}
              onUndo={onUndoRun ? () => onUndoRun(block.run_id) : undefined}
            />
          );

        if (block.type === "lead_summary")
          return (
            <LeadSummaryBlock
              key={`b-${i}`}
              block={block}
              sessionId={sessionId}
              onViewRun={onViewRun}
              onTakeOver={readonly ? undefined : onTakeOver}
              onCleanRedispatch={
                readonly ? undefined : () => onCleanRedispatch?.(block.run_id)
              }
            />
          );

        if (block.type === "coding_task")
          return (
            <CodingTaskBar
              key={`b-${i}`}
              block={block}
              onOpenMember={onOpenMember}
              onConfirmVerify={readonly ? undefined : onConfirmVerify}
              onShelve={readonly ? undefined : onShelve}
              onRetryVerify={readonly ? undefined : onRetryVerify}
            />
          );

        if (block.type === "dispatch_card")
          return (
            <DispatchCard
              key={`b-${i}`}
              member={block.member}
              onOpenInspector={onOpenInspector}
            />
          );

        if (block.type === "scope_change")
          return (
            <ScopeChangeCard
              key={`b-${i}`}
              block={block}
              onContinue={onContinueScope ?? (() => {})}
            />
          );

        if (block.type === "run_terminal")
          return <RunTerminalCard key={`b-${i}`} block={block} />;

        if (block.type === "decision_card") return null; // 决策卡经 lead-turn 路径渲·不走 raw block 循环

        const key = `b-${i}${streaming ? "-streaming" : ""}`;
        if (!MarkdownBody)
          return (
            <div key={key} style={{ whiteSpace: "pre-wrap" }}>
              {block.text}
            </div>
          );
        return (
          <MarkdownBody
            key={key}
            streaming={streaming}
            onOpenPreview={openPreviewOrExternal}
            onOpenLightbox={onOpenLightbox}
            sessionId={sessionId}
          >
            {block.text}
          </MarkdownBody>
        );
      })}
      {attachmentOpenError &&
        createPortal(
          <div className="toast" role="status" aria-label={attachmentOpenError}>
            {attachmentOpenError}
          </div>,
          document.body,
        )}
    </div>
  );
}

export const MessageContent = memo(MessageContentImpl);

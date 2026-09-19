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
import {
  foldByVerbosity,
  groupToolBlocks,
  type Segment,
  type StreamItem,
  type Verbosity,
} from "../lib/streamItems";
import { imagePathsFromTool } from "../lib/imageArtifacts";
import { useMarkdown } from "../lib/useMarkdown";
import { ActivityFold } from "./ActivityFold";
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
import { ContextCompactedChip } from "./ContextCompactedChip";
import { useI18n } from "../i18n";
import {
  getAttachmentDataUri,
  setAttachmentDataUri,
} from "../lib/attachmentCache";
import { renderBackendError } from "../lib/backendMsg";
import { isSvgDataUri } from "../lib/imageClipboard";
import { canCopyImageInEnv } from "../lib/imageClipboardTauri";
import { copyImageToClipboard } from "../lib/imageClipboardTauri";
import "../styles/chatImage.css";

type Props = {
  blocks: Block[];
  streaming?: boolean;
  /** V3b：过程细节显示级别（设计稿 §2B）；默认 full = 现状零变化。 */
  verbosity?: Verbosity;
  /** V3b：ActivityFold 展开态递归渲染时抑制内部图片抽取——产物已由外层 artifacts 段渲染。 */
  suppressArtifacts?: boolean;
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
  /// 规则 B 总开关，透传给正文 `text` 块的 MarkdownBody（其余卡片类型不受
  /// 影响）。默认关闭；调用方（聊天流 MessageStream）按 message.role 决定
  /// 是否传 true——只有 assistant 消息才自动出图，user 消息保持关闭。
  autoInlineImagePaths?: boolean;
};

type AttachmentContent = {
  kind: "text" | "image" | "binary";
  imageBase64?: string;
  mediaType?: string;
};

// V3b：pass 段沿用 groupToolBlocks 输出 + 路径级去重后的图片列表；
// activity_fold / artifacts 段原样透传（原始 Segment，渲染层各自处理）。
type PassEntry = { item: StreamItem; imagePaths: string[] };
type RenderSegment =
  | { kind: "pass"; keyPrefix: string; entries: PassEntry[] }
  | {
      kind: "activity_fold";
      segment: Extract<Segment, { kind: "activity_fold" }>;
    }
  | { kind: "artifacts"; segment: Extract<Segment, { kind: "artifacts" }> };

// 超过阈值的文本块整体走 markdown 同步解析会阻塞主线程数秒·折叠默认（T7）。
// 阈值降到 5 万（D3 整盘审 P2①）：实测 remark 解析 99k≈205ms、50k≈25ms，
// 流式场景每个 chunk 都会重解析一次，WKWebView 比桌面 Chrome 更慢，原 10 万阈值偏松。
const HUGE_TEXT_BLOCK_CHARS = 50_000;
// 折叠态预览字符数——够看清粘贴的是什么内容，不整段渲染。
const HUGE_TEXT_PREVIEW_CHARS = 4000;

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
  const canCopyImage = canCopyImageInEnv(clipboard);

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
      await copyImageToClipboard(dataUri, clipboard, canCopyImage);
      setFeedback(t("messageContent.imageMenu.imageCopied"));
    } catch (e) {
      console.warn("copyImage failed:", e instanceof Error ? e.name : e);
      // svg 栅格化 / 写剪贴板失败时改复制路径兜底，别让用户干等一个死结的
      // 「复制失败」——路径好歹能贴给别人当索引。
      if (isSvgDataUri(dataUri) && typeof clipboard?.writeText === "function") {
        try {
          await clipboard.writeText(path);
          setFeedback(t("messageContent.imageMenu.svgCopyFallback"));
          return;
        } catch {
          // 兜底也失败，落到下面的通用失败提示。
        }
      }
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
            className="al-chat-image"
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
            className="al-chat-image"
            style={{
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

// 巨型文本块（贴入几十万到 1MB 字符）折叠默认渲染：整体走 markdown 同步解析
// 会阻塞主线程数秒，收起态只给纯文本预览，展开也不整体走 markdown（粘贴的
// 几乎都是日志/代码，富渲染不值一次数秒卡·此为拍板行为）。
function HugeTextBlock({ text }: { text: string }) {
  const { t } = useI18n();
  const [open, setOpen] = useState(false);
  let preview = text.slice(0, HUGE_TEXT_PREVIEW_CHARS);
  // 硬切在代理对中间会劈半渲出 U+FFFD——若末字符是高位代理，整个字符退让给下一批。
  if (/[\uD800-\uDBFF]$/.test(preview)) {
    preview = preview.slice(0, -1);
  }
  return (
    <div className="huge-text">
      <div className="huge-text__body">{open ? text : preview}</div>
      <button
        type="button"
        className="huge-text__toggle"
        onClick={() => setOpen((v) => !v)}
        aria-expanded={open}
      >
        {open
          ? t("chat.hugeTextExpanded")
          : t("chat.hugeTextCollapsed", { chars: text.length })}
      </button>
    </div>
  );
}

function MessageContentImpl({
  blocks,
  streaming = false,
  verbosity = "full",
  suppressArtifacts = false,
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
  autoInlineImagePaths = false,
}: Props) {
  const { t } = useI18n();
  const MarkdownBody = useMarkdown();
  const contentRef = useRef<HTMLDivElement>(null);
  const [attachmentOpenError, setAttachmentOpenError] = useState<string | null>(
    null,
  );
  const readonly = readonlyReason != null;

  // V3b：折算位置（设计稿 §2B）——先按 verbosity 把 blocks 切成 Segment[]，
  // pass 段沿用现状 groupToolBlocks + 路径级图片抽取/去重（full 档恒单 pass 段
  // = 现状路径零变化）；activity_fold / artifacts 段原样透传给渲染层。
  const renderSegments = useMemo<RenderSegment[]>(() => {
    const segments = foldByVerbosity(blocks, verbosity, !!streaming);
    return segments.map((segment): RenderSegment => {
      if (segment.kind === "activity_fold") {
        return { kind: "activity_fold", segment };
      }
      if (segment.kind === "artifacts") {
        return { kind: "artifacts", segment };
      }
      const items = groupToolBlocks(segment.blocks);
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
      const entries = items.map((item, index) => {
        const fresh: string[] = [];
        pathsByItem[index].forEach((path) => {
          if (!preferredPaths.has(path) || seen.has(path)) return;
          seen.add(path);
          fresh.push(path);
        });
        return { item, imagePaths: fresh };
      });
      return {
        kind: "pass",
        keyPrefix: `seg${segment.sourceStartIndex}`,
        entries,
      };
    });
  }, [blocks, verbosity, streaming]);

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
  }, [MarkdownBody, renderSegments, t]);

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

  // 原始逐类型分发表（full 档零改）：抽成具名函数以便按 pass 段分别调用，
  // key 前缀 keyPrefix 区分 summary/minimal 档下同一消息内的多个 pass 段。
  const renderPassEntry = (
    item: StreamItem,
    imagePaths: string[],
    i: number,
    keyPrefix: string,
  ): ReactNode => {
    if (item.kind === "toolgroup") {
      const groupKey = item.blocks[0]?.id ?? `toolgroup-${i}`;
      return (
        <Fragment key={`${keyPrefix}-toolgroup-${groupKey}`}>
          <ToolStepsFold blocks={item.blocks} />
          {!suppressArtifacts &&
            onOpenPreview &&
            onOpenLightbox &&
            imagePaths.length > 0 && (
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
          key={`${keyPrefix}-b-${i}`}
          path={block.attachment_id}
          mediaType={block.media_type}
          sessionId={sessionId}
          onOpenPreview={onOpenPreview}
          onOpenLightbox={onOpenLightbox}
        />
      );
    if (block.type === "tool") {
      return (
        <Fragment key={`${keyPrefix}-b-${i}`}>
          <ToolCard block={block} compact />
          {!suppressArtifacts &&
            onOpenPreview &&
            onOpenLightbox &&
            imagePaths.length > 0 && (
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
          key={`${keyPrefix}-b-${i}`}
          block={block}
          sessionId={sessionId ?? ""}
        />
      );
    if (block.type === "thinking")
      return <ThinkingBlock key={`${keyPrefix}-b-${i}`} text={block.text} />;
    if (block.type === "team_run")
      return (
        <BackgroundTaskStack
          key={`${keyPrefix}-b-${i}`}
          runId={block.run_id}
          lead={block.lead}
          members={block.members}
          onOpenMember={onOpenMember}
          onUndoRun={onUndoRun}
        />
      );
    if (block.type === "gate_card" && gateView?.kind === "proposing")
      return (
        <div className="gate-proposing" key={`${keyPrefix}-b-${i}`}>
          <span className="gate-proposing__dot" aria-hidden />
          {t("messageContent.gate.proposing")}
        </div>
      );
    if (block.type === "gate_card" && gateView?.kind === "draft")
      return (
        <GateCard
          key={`${keyPrefix}-b-${i}`}
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
          key={`${keyPrefix}-b-${i}`}
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
          key={`${keyPrefix}-b-${i}`}
          block={block}
          onView={() => onViewRun?.()}
          onUndo={onUndoRun ? () => onUndoRun(block.run_id) : undefined}
        />
      );

    if (block.type === "lead_summary")
      return (
        <LeadSummaryBlock
          key={`${keyPrefix}-b-${i}`}
          block={block}
          sessionId={sessionId}
          onViewRun={onViewRun}
          onOpenPreview={onOpenPreview}
          onOpenLightbox={onOpenLightbox}
          onTakeOver={readonly ? undefined : onTakeOver}
          onCleanRedispatch={
            readonly ? undefined : () => onCleanRedispatch?.(block.run_id)
          }
        />
      );

    if (block.type === "coding_task")
      return (
        <CodingTaskBar
          key={`${keyPrefix}-b-${i}`}
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
          key={`${keyPrefix}-b-${i}`}
          member={block.member}
          onOpenInspector={onOpenInspector}
        />
      );

    if (block.type === "scope_change")
      return (
        <ScopeChangeCard
          key={`${keyPrefix}-b-${i}`}
          block={block}
          onContinue={onContinueScope ?? (() => {})}
        />
      );

    if (block.type === "run_terminal")
      return <RunTerminalCard key={`${keyPrefix}-b-${i}`} block={block} />;

    if (
      block.type === "context_compacted" ||
      block.type === "context_truncated"
    )
      return (
        <ContextCompactedChip
          key={`${keyPrefix}-b-${i}`}
          blockType={block.type}
        />
      );

    if (block.type === "decision_card") return null; // 决策卡经 lead-turn 路径渲·不走 raw block 循环

    const key = `${keyPrefix}-b-${i}${streaming ? "-streaming" : ""}`;
    // msgfix2 F2 S1（M0 §10.11「不识别的块类型不崩溃」）：走到这里的块理论上只剩 `text`——
    // 但这个联合类型只是前端已知的形状，后端可能发出一个这里没有任何 `if` 分支认识的新块
    // 类型（如未来新增的 `activity_summary_v99`），运行时它照样落到这里，`block.text` 实际是
    // `undefined`，不是类型标注承诺的 `string`。原先直接 `block.text.length` 对 undefined
    // 取 `.length` 会抛 TypeError，且 remote-web 当时没有任何 ErrorBoundary 兜底，会整页白屏。
    // 这里加运行时守卫（类型层面看似恒假，但这就是防的正是"类型跟运行时对不上"这件事本身）：
    // 不是字符串就降级渲染一行提示，不再往下访问 `.length`。
    if (typeof block.text !== "string")
      return (
        <div key={key} className="turn__unknown-block">
          {t("messageContent.unknownBlock")}
        </div>
      );
    if (block.text.length > HUGE_TEXT_BLOCK_CHARS)
      return <HugeTextBlock key={key} text={block.text} />;
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
        autoInlineImagePaths={autoInlineImagePaths}
      >
        {block.text}
      </MarkdownBody>
    );
  };

  return (
    <div className="turn__text" ref={contentRef}>
      {renderSegments.map((rs) => {
        if (rs.kind === "pass") {
          return (
            <Fragment key={`${rs.keyPrefix}-pass`}>
              {rs.entries.map(({ item, imagePaths }, i) =>
                renderPassEntry(item, imagePaths, i, rs.keyPrefix),
              )}
            </Fragment>
          );
        }
        if (rs.kind === "activity_fold") {
          return (
            <ActivityFold
              key={`${rs.segment.sourceStartIndex}:${verbosity}`}
              fold={rs.segment}
              verbosity={verbosity}
              sessionId={sessionId}
              onOpenPreview={onOpenPreview}
              onOpenLightbox={onOpenLightbox}
            />
          );
        }
        if (suppressArtifacts || !onOpenPreview || !onOpenLightbox) return null;
        return (
          <ImageArtifactChips
            key={`${rs.segment.sourceStartIndex}:artifacts`}
            paths={rs.segment.imagePaths}
            sessionId={sessionId}
            onOpenPreview={onOpenPreview}
            onOpenLightbox={onOpenLightbox}
          />
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

import { invoke } from "@tauri-apps/api/core";
import { memo, useEffect, useMemo, useRef, useState } from "react";
import { createPortal } from "react-dom";
import type { Block } from "../types/agent";
import {
  buildRenderSegments,
  type RenderSegment,
  type Verbosity,
} from "../lib/streamItems";
import { useMarkdown } from "../lib/useMarkdown";
import { useI18n } from "../i18n";
import { renderBackendError } from "../lib/backendMsg";
import { fileName } from "./ChatImages";
import { renderSegment, type BlockRenderContext } from "./MessageBlockRenderer";

export type Props = {
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

function isHtmlPath(path: string): boolean {
  return /\.html?$/i.test(path);
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

  const renderSegments = useMemo<RenderSegment[]>(
    () => buildRenderSegments(blocks, verbosity, !!streaming),
    [blocks, verbosity, streaming],
  );

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

  const ctx: BlockRenderContext = {
    t,
    MarkdownBody,
    openPreviewOrExternal,
    readonly,
    streaming,
    verbosity,
    suppressArtifacts,
    onOpenPreview,
    onOpenLightbox,
    sessionId,
    onOpenMember,
    onUndoRun,
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
    readonlyReason,
    onViewRun,
    onTakeOver,
    onCleanRedispatch,
    onConfirmVerify,
    onRetryVerify,
    onShelve,
    onContinueScope,
    onOpenInspector,
    autoInlineImagePaths,
  };

  return (
    <div className="turn__text" ref={contentRef}>
      {renderSegments.map((rs) => renderSegment(ctx, rs))}
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

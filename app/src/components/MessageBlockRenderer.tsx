import { Fragment, type ReactNode } from "react";
import type { Block } from "../types/agent";
import type { PassEntry } from "../lib/streamItems";
import type { useMarkdown } from "../lib/useMarkdown";
import type { useI18n } from "../i18n";
import type { Props } from "./MessageContent";
import { ToolStepsFold } from "./ToolStepsFold";
import { ToolCard } from "./ToolCard";
import { ImageArtifactChips, ImageBlockContent } from "./ChatImages";
import { ApprovalCard } from "./ApprovalCard";
import { ThinkingBlock } from "./ThinkingBlock";
import { BackgroundTaskStack } from "./BackgroundTaskStack";
import { GateCard } from "./GateCard";
import { DraftFailedCard } from "./DraftFailedCard";
import { RunCard } from "./RunCard";
import { LeadSummaryBlock } from "./LeadSummaryBlock";
import { CodingTaskBar } from "./CodingTaskBar";
import { DispatchCard } from "./DispatchCard";
import { ScopeChangeCard } from "./ScopeChangeCard";
import { RunTerminalCard } from "./RunTerminalCard";
import { ContextCompactedChip } from "./ContextCompactedChip";
import { HUGE_TEXT_BLOCK_CHARS, HugeTextBlock } from "./HugeTextBlock";
import { ActivityFold } from "./ActivityFold";
import type { RenderSegment, Verbosity } from "../lib/streamItems";

export type BlockRenderContext = Pick<
  Props,
  | "suppressArtifacts"
  | "onOpenPreview"
  | "onOpenLightbox"
  | "sessionId"
  | "onOpenMember"
  | "onUndoRun"
  | "gateView"
  | "leadName"
  | "enabledAgents"
  | "onGateAction"
  | "onGateFreeze"
  | "onGateRedraft"
  | "onGateRetry"
  | "onGateManual"
  | "onGateBackToNormal"
  | "gateFreezing"
  | "readonlyReason"
  | "onViewRun"
  | "onTakeOver"
  | "onCleanRedispatch"
  | "onConfirmVerify"
  | "onRetryVerify"
  | "onShelve"
  | "onContinueScope"
  | "onOpenInspector"
  | "autoInlineImagePaths"
> & {
  t: ReturnType<typeof useI18n>["t"];
  MarkdownBody: ReturnType<typeof useMarkdown>;
  openPreviewOrExternal: ((path: string) => void) | undefined;
  readonly: boolean;
  streaming: boolean;
  verbosity: Verbosity;
};

function renderTools(
  ctx: BlockRenderContext,
  item: PassEntry["item"],
  imagePaths: PassEntry["imagePaths"],
  i: number,
  keyPrefix: string,
): ReactNode | undefined {
  const { suppressArtifacts, onOpenPreview, onOpenLightbox, sessionId } = ctx;
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
  return undefined;
}

function renderApproval(
  ctx: BlockRenderContext,
  block: Block,
  _imagePaths: PassEntry["imagePaths"],
  i: number,
  keyPrefix: string,
): ReactNode | undefined {
  const { sessionId } = ctx;
  if (block.type === "approval")
    return (
      <ApprovalCard
        key={`${keyPrefix}-b-${i}`}
        block={block}
        sessionId={sessionId ?? ""}
      />
    );
  return undefined;
}

function renderThinking(
  _ctx: BlockRenderContext,
  block: Block,
  _imagePaths: PassEntry["imagePaths"],
  i: number,
  keyPrefix: string,
): ReactNode | undefined {
  if (block.type === "thinking")
    return <ThinkingBlock key={`${keyPrefix}-b-${i}`} text={block.text} />;
  return undefined;
}

function renderTeamRun(
  ctx: BlockRenderContext,
  block: Block,
  _imagePaths: PassEntry["imagePaths"],
  i: number,
  keyPrefix: string,
): ReactNode | undefined {
  const { onOpenMember, onUndoRun } = ctx;
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
  return undefined;
}

function renderGate(
  ctx: BlockRenderContext,
  block: Block,
  _imagePaths: PassEntry["imagePaths"],
  i: number,
  keyPrefix: string,
): ReactNode | undefined {
  const {
    gateView,
    t,
    leadName,
    enabledAgents,
    onGateAction,
    onGateFreeze,
    onGateRedraft,
    gateFreezing,
    readonlyReason,
    onGateRetry,
    onGateManual,
    onGateBackToNormal,
    readonly,
  } = ctx;
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
  if (block.type === "gate_card" || block.type === "draft_failed") return null; // A cleared or mismatched gate view renders nothing.
  return undefined;
}

function renderRun(
  ctx: BlockRenderContext,
  block: Block,
  _imagePaths: PassEntry["imagePaths"],
  i: number,
  keyPrefix: string,
): ReactNode | undefined {
  const {
    onViewRun,
    onUndoRun,
    sessionId,
    onOpenPreview,
    onOpenLightbox,
    onTakeOver,
    onCleanRedispatch,
    readonly,
    onOpenMember,
    onConfirmVerify,
    onShelve,
    onRetryVerify,
    onOpenInspector,
    onContinueScope,
  } = ctx;
  // Inline change card: "View" forwards onViewRun to open the Review tab in the right panel.
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
  return undefined;
}

function renderContext(
  _ctx: BlockRenderContext,
  block: Block,
  _imagePaths: PassEntry["imagePaths"],
  i: number,
  keyPrefix: string,
): ReactNode | undefined {
  if (block.type === "context_compacted" || block.type === "context_truncated")
    return (
      <ContextCompactedChip
        key={`${keyPrefix}-b-${i}`}
        blockType={block.type}
      />
    );

  if (block.type === "decision_card") return null; // Decision cards render through the lead-turn path, not the raw block loop.
  return undefined;
}

function renderTextFallback(
  ctx: BlockRenderContext,
  block: Extract<Block, { type: "text" }>,
  _imagePaths: PassEntry["imagePaths"],
  i: number,
  keyPrefix: string,
): ReactNode | undefined {
  const {
    streaming,
    t,
    MarkdownBody,
    openPreviewOrExternal,
    onOpenLightbox,
    sessionId,
    autoInlineImagePaths,
  } = ctx;
  const key = `${keyPrefix}-b-${i}${streaming ? "-streaming" : ""}`;
  // The frontend union promises that only text blocks remain here, but the
  // backend can send a new block type that none of the branches above recognize.
  // At runtime, its text is undefined rather than the string declared by the
  // type. Previously, reading block.text.length in that case threw a TypeError;
  // remote-web had no ErrorBoundary to catch it, so the entire page went blank.
  // Keep this seemingly redundant runtime guard or that blank-page bug returns.
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
}

// This is the original block-type dispatch table, extracted into a named
// function so each pass segment can call it separately. keyPrefix distinguishes
// multiple pass segments within one message at the summary and minimal detail
// levels.
export function renderPassEntry(
  ctx: BlockRenderContext,
  item: PassEntry["item"],
  imagePaths: PassEntry["imagePaths"],
  i: number,
  keyPrefix: string,
): ReactNode {
  const tools = renderTools(ctx, item, imagePaths, i, keyPrefix);
  if (tools !== undefined) return tools;
  if (item.kind !== "block") return undefined;
  const block = item.block;
  const approval = renderApproval(ctx, block, imagePaths, i, keyPrefix);
  if (approval !== undefined) return approval;
  const thinking = renderThinking(ctx, block, imagePaths, i, keyPrefix);
  if (thinking !== undefined) return thinking;
  const teamRun = renderTeamRun(ctx, block, imagePaths, i, keyPrefix);
  if (teamRun !== undefined) return teamRun;
  const gate = renderGate(ctx, block, imagePaths, i, keyPrefix);
  if (gate !== undefined) return gate;
  const run = renderRun(ctx, block, imagePaths, i, keyPrefix);
  if (run !== undefined) return run;
  const context = renderContext(ctx, block, imagePaths, i, keyPrefix);
  if (context !== undefined) return context;
  return renderTextFallback(
    ctx,
    block as Extract<Block, { type: "text" }>,
    imagePaths,
    i,
    keyPrefix,
  );
}

export function renderSegment(
  ctx: BlockRenderContext,
  rs: RenderSegment,
): ReactNode {
  const {
    verbosity,
    sessionId,
    onOpenPreview,
    onOpenLightbox,
    suppressArtifacts,
  } = ctx;
  if (rs.kind === "pass") {
    return (
      <Fragment key={`${rs.keyPrefix}-pass`}>
        {rs.entries.map(({ item, imagePaths }, i) =>
          renderPassEntry(ctx, item, imagePaths, i, rs.keyPrefix),
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
}

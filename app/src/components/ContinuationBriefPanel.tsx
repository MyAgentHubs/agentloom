import { useEffect, useState } from "react";
import { useI18n } from "../i18n";
import { useMarkdownLib } from "../lib/useMarkdown";

export type ContinuationStartPayload = {
  parentSessionId: string;
  handoffDoc: string;
  suggestedTitle?: string;
};

type Props = {
  parentSessionId: string;
  parentTitle?: string;
  draftState: ContinuationDraftState;
  starting?: boolean;
  onRetry: () => void;
  onCancel: () => void;
  onStart: (p: ContinuationStartPayload) => void;
};

export type ContinuationDraftState = {
  status: "loading" | "ready" | "error";
  draft?: string;
  suggestedTitle?: string;
  warnings?: string[];
  error?: string;
};

function classifyGenerationError(
  err: string,
  t: ReturnType<typeof useI18n>["t"],
) {
  const lower = err.toLowerCase();
  if (lower.includes("session_busy") || lower.includes("session busy")) {
    return t("continuation.panel.v3.errorBusy");
  }
  if (lower.includes("key") || err.includes("密钥")) {
    return t("continuation.panel.v3.errorKey");
  }
  if (lower.includes("parse") || err.includes("解析")) {
    return t("continuation.panel.v3.errorParser");
  }
  return t("continuation.panel.v3.errorBackend");
}

export function ContinuationBriefPanel({
  parentSessionId,
  parentTitle,
  draftState,
  starting = false,
  onRetry,
  onCancel,
  onStart,
}: Props) {
  const { t } = useI18n();
  const markdownLib = useMarkdownLib();
  const [docText, setDocText] = useState(
    draftState.status === "ready" ? (draftState.draft ?? "") : "",
  );
  const [titleInput, setTitleInput] = useState(
    draftState.status === "ready" ? (draftState.suggestedTitle ?? "") : "",
  );
  const [editMode, setEditMode] = useState(false);

  useEffect(() => {
    if (draftState.status !== "ready") return;
    setDocText(draftState.draft ?? "");
    setTitleInput(draftState.suggestedTitle ?? "");
    setEditMode(false);
  }, [draftState]);

  const canStart =
    draftState.status === "ready" && !starting && docText.trim().length > 0;

  function start() {
    if (!canStart) return;
    onStart({
      parentSessionId,
      handoffDoc: docText,
      suggestedTitle: titleInput.trim() || undefined,
    });
  }

  function renderRetryButton() {
    return (
      <button type="button" onClick={onRetry}>
        {t("continuation.panel.v3.retry")}
      </button>
    );
  }

  function renderDoneBody() {
    return (
      <>
        <label className="cc-field" htmlFor="cc-suggested-title">
          <span>{t("continuation.panel.v3.suggestedTitleLabel")}</span>
          <input
            id="cc-suggested-title"
            aria-label={t("continuation.panel.v3.suggestedTitleLabel")}
            value={titleInput}
            onChange={(e) => setTitleInput(e.target.value)}
          />
        </label>
        <div className="cc-organize-row">
          <button type="button" onClick={() => setEditMode((v) => !v)}>
            {editMode
              ? t("continuation.panel.v3.doneEditing")
              : t("continuation.panel.v3.editToggle")}
          </button>
          {renderRetryButton()}
        </div>
        {(draftState.warnings?.length ?? 0) > 0 ? (
          <div className="cc-warning-block">
            <span className="k">
              {t("continuation.panel.v3.warningsLabel")}
            </span>
            <ul className="cc-warnings">
              {draftState.warnings?.map((warning, index) => (
                <li key={`${warning}-${index}`}>{warning}</li>
              ))}
            </ul>
          </div>
        ) : null}
        {editMode ? (
          <textarea
            aria-label="handoff-doc-edit"
            value={docText}
            onChange={(e) => setDocText(e.target.value)}
          />
        ) : (
          <div className="cc-handoff-doc">
            {markdownLib ? (
              <markdownLib.Markdown remarkPlugins={[markdownLib.remarkGfm]}>
                {docText}
              </markdownLib.Markdown>
            ) : (
              <div style={{ whiteSpace: "pre-wrap" }}>{docText}</div>
            )}
          </div>
        )}
      </>
    );
  }

  function renderBody() {
    if (draftState.status === "loading") {
      return (
        <div className="cc-loading-state" role="status">
          <span className="cc-spinner" aria-hidden="true" />
          <span className="cc-loading-main">
            {t("continuation.panel.v3.loading")}
          </span>
          <span className="cc-loading-sub">
            {t("continuation.panel.v3.loadingSub")}
          </span>
        </div>
      );
    }

    if (draftState.status === "error") {
      const message = draftState.error ?? "";
      return (
        <div className="cc-error-block" role="alert">
          <p className="cc-error-main">{classifyGenerationError(message, t)}</p>
          <p className="cc-error-detail">{message}</p>
          <div className="cc-organize-row">{renderRetryButton()}</div>
        </div>
      );
    }

    return renderDoneBody();
  }

  return (
    <section className="cc-brief" aria-label={t("continuation.panel.label")}>
      <div className="cc-brief-h">
        <span className="ic" aria-hidden="true">
          <svg viewBox="0 0 24 24">
            <path d="M14 2H6a2 2 0 00-2 2v16a2 2 0 002 2h12a2 2 0 002-2V8z" />
            <path d="M14 2v6h6M9 13h6M9 17h4" />
          </svg>
        </span>
        <span className="t">{t("continuation.panel.headerTitle")}</span>
        <span className="editbadge">
          {editMode
            ? t("continuation.panel.editable")
            : t("continuation.panel.v3.readOnly")}
        </span>
      </div>
      <div className="cc-brief-b">
        {renderBody()}
        {parentTitle ? (
          <div className="cc-parent">
            {t("continuation.panel.parent")}: <b>{parentTitle}</b>
          </div>
        ) : null}
      </div>
      <div className="cc-brief-f">
        <button type="button" className="btn ghost" onClick={onCancel}>
          {t("continuation.panel.cancel")}
        </button>
        <button
          type="button"
          className="btn primary"
          disabled={!canStart}
          title={
            docText.trim().length === 0
              ? t("continuation.panel.v3.startDisabledHint")
              : undefined
          }
          onClick={start}
        >
          {starting
            ? t("continuation.panel.starting")
            : t("continuation.panel.start")}
        </button>
      </div>
    </section>
  );
}

import { memo, useCallback, useMemo } from "react";
import {
  useRepoDocument,
  type RepoDocumentKind,
} from "../hooks/useRepoDocument";
import { useI18n, type I18nKey } from "../i18n";
import { useMarkdown } from "../lib/useMarkdown";

export type RepoDocumentPanelProps = {
  repoId: string | null;
  agentId: string;
  kind: RepoDocumentKind;
};

const KIND_KEYS = {
  intro: {
    emptyTitle: "repoDoc.empty.title.intro",
    emptyDesc: "repoDoc.empty.desc.intro",
    emptyCta: "repoDoc.empty.cta.intro",
    generatingTitle: "repoDoc.generating.title.intro",
  },
  daily: {
    emptyTitle: "repoDoc.empty.title.daily",
    emptyDesc: "repoDoc.empty.desc.daily",
    emptyCta: "repoDoc.empty.cta.daily",
    generatingTitle: "repoDoc.generating.title.daily",
  },
} as const satisfies Record<RepoDocumentKind, Record<string, I18nKey>>;

function DocumentIcon() {
  return (
    <svg viewBox="0 0 24 24" aria-hidden="true">
      <path d="M6 3.5h8l4 4V20.5H6z" />
      <path d="M14 3.5v4h4M9 12h6M9 16h6" />
    </svg>
  );
}
function LockIcon() {
  return (
    <svg viewBox="0 0 24 24" aria-hidden="true">
      <rect x="5" y="10" width="14" height="10" rx="2" />
      <path d="M8 10V7a4 4 0 0 1 8 0v3" />
    </svg>
  );
}
function WarningIcon() {
  return (
    <svg viewBox="0 0 24 24" aria-hidden="true">
      <path d="M12 3 2.8 20h18.4zM12 9v5M12 17.5v.1" />
    </svg>
  );
}

export const RepoDocumentPanel = memo(function RepoDocumentPanel({
  repoId,
  agentId,
  kind,
}: RepoDocumentPanelProps) {
  const { t } = useI18n();
  const MarkdownBody = useMarkdown();
  const { doc, loading, generating, liveText, error, generate } =
    useRepoDocument(repoId, kind);
  const canGenerate = repoId !== null && agentId.trim().length > 0;
  const keys = KIND_KEYS[kind];
  const handleGenerate = useCallback(() => {
    if (canGenerate) generate(agentId);
  }, [agentId, canGenerate, generate]);
  const generatedAt = useMemo(
    () => (doc ? new Date(doc.generated_at * 1000).toLocaleString() : ""),
    [doc],
  );
  const shortSha = doc?.head_sha.slice(0, 8) ?? "";

  if (error && !generating) {
    return (
      <section className="repo-doc repo-doc__error" role="alert">
        <div>
          <strong>{t("repoDoc.error")}</strong>
          <span>{error}</span>
        </div>
        <button type="button" onClick={handleGenerate} disabled={!canGenerate}>
          {t("repoDoc.retry")}
        </button>
      </section>
    );
  }
  if (generating) {
    return (
      <section className="repo-doc repo-doc__generating" aria-live="polite">
        <span className="repo-doc__spinner" aria-hidden="true" />
        <h2>{t(keys.generatingTitle)}</h2>
        <p>{t("repoDoc.generating.lede")}</p>
        {liveText ? <pre className="repo-doc__live">{liveText}</pre> : null}
      </section>
    );
  }
  if (!doc && loading) {
    return (
      <section className="repo-doc repo-doc__loading" role="status">
        <span className="repo-doc__spinner" aria-hidden="true" />
        {t("repoDoc.loading")}
      </section>
    );
  }
  if (!doc) {
    return (
      <section className="repo-doc repo-doc__empty">
        <span className="repo-doc__empty-icon">
          <DocumentIcon />
        </span>
        <h2>{t(keys.emptyTitle)}</h2>
        <p>{t(keys.emptyDesc)}</p>
        <button
          className="repo-doc__primary"
          type="button"
          onClick={handleGenerate}
          disabled={!canGenerate}
        >
          {t(keys.emptyCta)}
        </button>
        <div className="repo-doc__readonly">
          <LockIcon />
          <span>{t("repoDoc.readonly")}</span>
        </div>
      </section>
    );
  }
  return (
    <article className="repo-doc repo-doc__complete">
      {doc.stale ? (
        <div className="repo-doc__stale" role="status">
          <WarningIcon />
          <span>{t("repoDoc.stale", { sha: shortSha })}</span>
          <button
            type="button"
            onClick={handleGenerate}
            disabled={!canGenerate}
          >
            {t("repoDoc.regenerate")}
          </button>
        </div>
      ) : null}
      <header className="repo-doc__header">
        <span>
          {t("repoDoc.generatedAt")} {generatedAt}
        </span>
        <button type="button" onClick={handleGenerate} disabled={!canGenerate}>
          {t("repoDoc.regenerate")}
        </button>
      </header>
      <div className="repo-doc__content">
        {MarkdownBody ? (
          <MarkdownBody streaming={false}>{doc.content}</MarkdownBody>
        ) : (
          <pre>{doc.content}</pre>
        )}
      </div>
      <footer className="repo-doc__meta">
        <span>{t("repoDoc.disclaimer")}</span>
        <span>{t("repoDoc.commit", { sha: shortSha })}</span>
        <span>{generatedAt}</span>
      </footer>
    </article>
  );
});

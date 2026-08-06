import { useMemo } from "react";
import type { Components } from "react-markdown";
import type { LeadSummaryBlock as LSB, Finding } from "../types/agent";
import { useI18n } from "../i18n";
import { useMarkdownLib } from "../lib/useMarkdown";
import { localImageMarkdownComponent } from "./localMarkdownImage";
import type * as MarkdownLib from "../lib/markdownLib";
import type {
  KeyedFinding,
  KeyedSummarySection,
  SummaryI18nText,
  SummarySectionId,
} from "../lib/leadSummary";
import {
  memberFailureReasonText,
  type MemberFailureReason,
} from "../lib/memberFailure";

type Props = {
  block: LSB;
  stopNotice?: boolean;
  onViewRun?: () => void;
  onTakeOver?: () => void;
  onCleanRedispatch?: () => void;
  sessionId?: string | null;
};

const leadMarkdownComponents: Components = {
  table({ children }) {
    return (
      <div className="mm-table-wrap">
        <table>{children}</table>
      </div>
    );
  },
  td({ children, style, ...props }) {
    return (
      <td {...props} style={{ ...style, textAlign: "left" }}>
        {children}
      </td>
    );
  },
  th({ children, style, ...props }) {
    return (
      <th {...props} style={{ ...style, textAlign: "left" }}>
        {children}
      </th>
    );
  },
};

function LeadMarkdown({
  children,
  markdownLib,
  components,
}: {
  children: string;
  markdownLib: typeof MarkdownLib | null;
  components: Components;
}) {
  if (!markdownLib) {
    return <div style={{ whiteSpace: "pre-wrap" }}>{children}</div>;
  }
  return (
    <markdownLib.Markdown
      remarkPlugins={[markdownLib.remarkGfm]}
      skipHtml={true}
      components={components}
    >
      {children}
    </markdownLib.Markdown>
  );
}

function statusLine(
  s: LSB["status"],
  t: ReturnType<typeof useI18n>["t"],
): string | null {
  if (s.kind === "all_succeeded") return null;
  const values = { succeeded: s.succeeded_count, total: s.total };
  if (s.kind === "failed") return t("leadSummary.status.failed", values);
  return t("leadSummary.status.partial", values);
}

function failureAdvice(
  findings: Finding[],
  t: ReturnType<typeof useI18n>["t"],
  extraText = "",
): string {
  if (
    findings.some(
      (finding) => (finding as KeyedFinding).failure_reason?.code === "quota",
    )
  ) {
    return t("leadSummary.advice.rateLimit");
  }
  const missText = findings
    .filter((f) => f.status === "miss")
    .map((f) => f.text)
    .concat(extraText)
    .join("\n");
  if (/额度|频控|限流|rate limit|quota|429/i.test(missText)) {
    return t("leadSummary.advice.rateLimit");
  }
  return t("leadSummary.advice.default");
}

function stripMarkdown(text: string): string {
  return text
    .replace(/`([^`]+)`/g, "$1")
    .replace(/\*\*([^*]+)\*\*/g, "$1")
    .replace(/__([^_]+)__/g, "$1")
    .replace(/\s+/g, " ")
    .trim();
}

function conciseFailure(
  block: LSB,
  t: ReturnType<typeof useI18n>["t"],
): string | null {
  const structuredReason =
    block.findings
      .map((finding) => (finding as KeyedFinding).failure_reason)
      .find((reason) => reason != null) ??
    block.sections
      .map((section) => (section as KeyedSummarySection).failure_reason)
      .find((reason) => reason != null);
  if (structuredReason != null) {
    return memberFailureReasonText(structuredReason, t);
  }

  const miss = block.findings.find((f) => f.status === "miss")?.text;
  const body = block.sections.find((s) => s.body_richtext)?.body_richtext;
  const raw = stripMarkdown(miss ?? body ?? "");
  const reason =
    raw.match(/worker 调用失败[:：]\s*([^（]+)/)?.[1]?.trim() ??
    raw
      .replace(/^[^:：]{1,24}[:：]\s*/, "")
      .replace(/（见 trace）$/, "")
      .trim();
  if (reason) return t("leadSummary.failure.withReason", { reason });
  return t("leadSummary.failure.noResult");
}

function findingText(
  finding: Finding,
  t: ReturnType<typeof useI18n>["t"],
): string | null {
  const keyed = finding as KeyedFinding;
  if (keyed.text_i18n == null) return finding.text;
  const values = { ...keyed.text_i18n.values };
  if (keyed.failure_reason != null) {
    const reason = memberFailureReasonText(keyed.failure_reason, t);
    if (reason == null) return null;
    values.reason = reason;
  }
  return t(keyed.text_i18n.key, values);
}

const SECTION_HEADING_KEYS: Partial<Record<SummarySectionId, string>> = {
  changes: "leadSummary.section.changes",
  verify: "leadSummary.section.verify",
  risk: "leadSummary.section.risk",
  fallback: "leadSummary.section.fallback",
};

function sectionHeading(
  section: LSB["sections"][number],
  t: ReturnType<typeof useI18n>["t"],
): string {
  const keyed = section as KeyedSummarySection;
  if (keyed.id === "llm" || keyed.id === "result") return section.heading;
  const expectedKey = SECTION_HEADING_KEYS[keyed.id];
  if (expectedKey == null || section.heading !== expectedKey) {
    return section.heading;
  }
  return t(section.heading as Parameters<typeof t>[0], keyed.heading_values);
}

function translatedBodyPart(
  part: SummaryI18nText,
  reason: MemberFailureReason | undefined,
  t: ReturnType<typeof useI18n>["t"],
): string | null {
  const values = { ...part.values };
  if (reason != null) {
    const reasonText = memberFailureReasonText(reason, t);
    if (reasonText == null) return null;
    values.reason = reasonText;
  }
  return t(part.key, values);
}

function sectionBody(
  section: LSB["sections"][number],
  t: ReturnType<typeof useI18n>["t"],
): string {
  const keyed = section as KeyedSummarySection;
  const translated = (keyed.body_i18n ?? [])
    .map((part) => translatedBodyPart(part, keyed.failure_reason, t))
    .filter((part): part is string => part != null)
    .join("\n");
  return [section.body_richtext, translated].filter(Boolean).join("\n\n");
}

function FindingGroup({
  title,
  items,
  t,
}: {
  title: string;
  items: Finding[];
  t: ReturnType<typeof useI18n>["t"];
}) {
  const renderedItems = items
    .map((finding) => ({ finding, text: findingText(finding, t) }))
    .filter(
      (item): item is { finding: Finding; text: string } => item.text != null,
    );
  if (!renderedItems.length) return null;
  return (
    <div className="lead-summary__fgroup">
      <div className="lead-summary__h">{title}</div>
      {renderedItems.map(({ finding: f, text }, i) => (
        <div className={`lead-summary__find is-${f.status}`} key={i}>
          <span className={`lead-summary__fst is-${f.status}`} aria-hidden />
          <span className="lead-summary__fc">{text}</span>
          <span className="lead-summary__src">{f.assignment_id} · drill ›</span>
        </div>
      ))}
    </div>
  );
}

export function LeadSummaryBlock({
  block,
  stopNotice = false,
  onViewRun,
  sessionId,
}: Props) {
  const { t } = useI18n();
  const markdownLib = useMarkdownLib();
  const components = useMemo(
    () => ({
      ...leadMarkdownComponents,
      img: localImageMarkdownComponent({ sessionId }),
    }),
    [sessionId],
  );

  if (block.summary_source === "pending") {
    return (
      <div className="lead-summary lead-summary--pending" aria-live="polite">
        <span className="lead-summary__pending-dot" aria-hidden />
        <span className="lead-summary__pending-txt">
          {t("leadSummary.pending")}
        </span>
      </div>
    );
  }

  if (stopNotice) {
    return (
      <div className="lead-summary">
        <p className="lead-summary__status">
          {t("leadSummary.stopped.status")}
        </p>
        <div className="lead-summary__say">
          <p>{t("leadSummary.stopped.message")}</p>
        </div>
      </div>
    );
  }

  const line = statusLine(block.status, t);
  const done = block.findings.filter((f) => f.status === "done");
  const miss = block.findings.filter((f) => f.status === "miss");
  if (block.status.kind === "failed") {
    const failure = conciseFailure(block, t);
    return (
      <div className="lead-summary lead-summary--failed">
        {line && <p className="lead-summary__status">{line}</p>}
        <div className="lead-summary__failbox" role="status">
          {failure != null && (
            <div className="lead-summary__failtitle">{failure}</div>
          )}
          <p className="lead-summary__failadvice">
            {failureAdvice(block.findings, t, failure ?? "")}
          </p>
        </div>
      </div>
    );
  }

  return (
    <div className="lead-summary">
      {line && <p className="lead-summary__status">{line}</p>}
      {block.sections.map((sec, i) => {
        const heading = sectionHeading(sec, t);
        const body = sectionBody(sec, t);
        return heading === "" ? (
          <div className="lead-summary__say" key={i}>
            {body !== "" && (
              <LeadMarkdown markdownLib={markdownLib} components={components}>
                {body}
              </LeadMarkdown>
            )}
          </div>
        ) : (
          <section className="lead-summary__sec" id={`lead-sec-${i}`} key={i}>
            <h4 className="lead-summary__h">{heading}</h4>
            {body !== "" && (
              <div className="lead-summary__body">
                <LeadMarkdown markdownLib={markdownLib} components={components}>
                  {body}
                </LeadMarkdown>
              </div>
            )}
          </section>
        );
      })}
      <FindingGroup title={t("leadSummary.findings.done")} items={done} t={t} />
      <FindingGroup title={t("leadSummary.findings.miss")} items={miss} t={t} />
      {block.artifact_refs
        .filter((a) => a.kind === "code_diff")
        .map((a, i) => (
          <button key={i} className="lead-summary__artiref" onClick={onViewRun}>
            {a.label}
          </button>
        ))}
      {block.status.kind !== "all_succeeded" && (
        <p className="lead-summary__advice">
          {failureAdvice(block.findings, t)}
        </p>
      )}
    </div>
  );
}

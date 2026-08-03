import { memo, useState } from "react";
import type { CSSProperties } from "react";
import type { Block, ScopeChangeItem } from "../types/agent";
import { useI18n, type I18nKey } from "../i18n";

type ScopeBlock = Extract<Block, { type: "scope_change" }>;

type Props = {
  block: ScopeBlock;
  onContinue: (draft: string) => void;
};

const KIND_LABEL: Record<string, I18nKey> = {
  scope: "scopeChange.kind.scope",
  objective: "scopeChange.kind.objective",
  constraint: "scopeChange.kind.constraint",
};

type Translate = (
  key: I18nKey,
  values?: Record<string, string | number>,
) => string;

const palette = {
  panel: "#FFFEFB",
  bg: "#F5F2EC",
  sunken: "#EDE9DE",
  ink: "#1A1814",
  ink2: "#4A463E",
  ink3: "#7A7568",
  ink4: "#ACA697",
  line: "#E5DDC9",
  lineSoft: "#EFE8D4",
  accent: "#D97757",
  accentSoft: "#F4E1D2",
  green: "#6A9B5C",
  red: "#B85048",
};

const styles: Record<string, CSSProperties> = {
  card: {
    border: `1px solid ${palette.accent}`,
    borderRadius: 8,
    overflow: "hidden",
    margin: "10px 0 2px",
    background: palette.panel,
    boxShadow: "0 0 0 3px rgba(217,119,87,0.07)",
  },
  cardHandled: {
    borderColor: palette.line,
    boxShadow: "none",
  },
  head: {
    display: "flex",
    alignItems: "center",
    gap: 9,
    padding: "9px 12px",
    borderBottom: `1px solid ${palette.lineSoft}`,
  },
  headHandled: {
    borderBottom: "none",
  },
  icon: {
    width: 16,
    height: 16,
    flexShrink: 0,
    color: palette.accent,
  },
  title: {
    fontSize: 12.5,
    fontWeight: 700,
    color: palette.ink,
  },
  spacer: {
    flex: 1,
  },
  pill: {
    display: "inline-flex",
    alignItems: "center",
    gap: 5,
    fontSize: 9.5,
    fontWeight: 700,
    padding: "2px 8px",
    borderRadius: 10,
    textTransform: "uppercase",
    letterSpacing: "0.03em",
    whiteSpace: "nowrap",
    background: palette.accentSoft,
    color: "#A85A3C",
  },
  pillMuted: {
    background: palette.sunken,
    color: palette.ink3,
  },
  dot: {
    width: 5,
    height: 5,
    borderRadius: "50%",
    background: "currentColor",
  },
  body: {
    padding: 12,
  },
  ctx: {
    fontSize: 12,
    color: palette.ink2,
    margin: "0 0 12px",
    lineHeight: 1.55,
  },
  changes: {
    display: "flex",
    flexDirection: "column",
    gap: 9,
  },
  change: {
    display: "flex",
    gap: 10,
    alignItems: "flex-start",
    background: palette.sunken,
    borderRadius: 7,
    padding: "9px 11px",
  },
  kind: {
    flexShrink: 0,
    fontSize: 10,
    fontWeight: 700,
    padding: "2px 9px",
    borderRadius: 5,
    background: palette.panel,
    border: `1px solid ${palette.line}`,
    color: palette.ink2,
    marginTop: 1,
    whiteSpace: "nowrap",
  },
  detail: {
    fontSize: 12.5,
    color: palette.ink,
    lineHeight: 1.5,
  },
  summary: {
    fontWeight: 600,
  },
  text: {
    color: palette.ink2,
    marginTop: 2,
  },
  finalizeNote: {
    display: "flex",
    gap: 8,
    alignItems: "flex-start",
    marginTop: 13,
    padding: "8px 10px",
    border: `1px dashed ${palette.line}`,
    borderRadius: 6,
    fontSize: 11,
    color: palette.ink3,
    lineHeight: 1.5,
  },
  finalizeIcon: {
    width: 13,
    height: 13,
    flexShrink: 0,
    marginTop: 2,
    color: palette.ink4,
  },
  footer: {
    display: "flex",
    alignItems: "center",
    gap: 9,
    padding: "10px 12px",
    borderTop: `1px solid ${palette.lineSoft}`,
    background: palette.bg,
    flexWrap: "wrap",
  },
  hint: {
    fontSize: 11,
    color: palette.ink3,
    flex: "1 1 220px",
    lineHeight: 1.5,
  },
  button: {
    fontSize: 12,
    fontWeight: 600,
    padding: "7px 16px",
    borderRadius: 7,
    cursor: "pointer",
    border: "1px solid transparent",
    display: "inline-flex",
    alignItems: "center",
    gap: 6,
    lineHeight: 1.2,
  },
  secondary: {
    background: palette.panel,
    color: palette.ink2,
    borderColor: palette.line,
  },
  primary: {
    background: palette.accent,
    color: "#fff",
  },
  handledNote: {
    display: "flex",
    alignItems: "center",
    gap: 8,
    padding: "4px 12px 10px",
    fontSize: 11.5,
    color: palette.ink3,
    cursor: "pointer",
  },
  handledIcon: {
    width: 14,
    height: 14,
    flexShrink: 0,
    color: palette.ink4,
  },
};

export function scopeKindLabel(kind: string, t: Translate): string {
  const key = KIND_LABEL[kind];
  return key ? t(key) : kind;
}

export function buildContinueDraft(
  changes: ScopeChangeItem[],
  t: Translate,
): string {
  const lines = changes.map(
    (c) => `[${scopeKindLabel(c.kind, t)}] ${c.detail_text}`,
  );
  return t("scopeChange.continueDraft", { changes: lines.join("\n") });
}

function ScopeShiftIcon({
  style,
  strokeWidth = 2,
}: {
  style?: CSSProperties;
  strokeWidth?: number;
}) {
  return (
    <svg
      viewBox="0 0 24 24"
      style={{ display: "block", ...style }}
      fill="none"
      stroke="currentColor"
      strokeWidth={strokeWidth}
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden="true"
    >
      <path d="M4 6h11" />
      <path d="M4 6l3-3M4 6l3 3" />
      <path d="M20 18H9" />
      <path d="M20 18l-3-3M20 18l-3 3" />
    </svg>
  );
}

function AlertIcon() {
  return (
    <svg
      viewBox="0 0 24 24"
      style={{ display: "block", width: 13, height: 13 }}
      fill="none"
      stroke="currentColor"
      strokeWidth="2"
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden="true"
    >
      <circle cx="12" cy="12" r="9" />
      <path d="M12 8v4" />
      <path d="M12 16h.01" />
    </svg>
  );
}

function ArrowIcon() {
  return (
    <svg
      viewBox="0 0 24 24"
      style={{ display: "block", width: 13, height: 13 }}
      fill="none"
      stroke="currentColor"
      strokeWidth="2.2"
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden="true"
    >
      <path d="M5 12h14" />
      <path d="M13 6l6 6-6 6" />
    </svg>
  );
}

function ChevronIcon() {
  return (
    <svg
      viewBox="0 0 24 24"
      style={{ display: "block", width: 14, height: 14 }}
      fill="none"
      stroke="currentColor"
      strokeWidth="2"
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden="true"
    >
      <path d="M9 6l6 6-6 6" />
    </svg>
  );
}

function ScopeChangeCardImpl({ block, onContinue }: Props) {
  const { t } = useI18n();
  const [collapsed, setCollapsed] = useState(false);
  const multi = block.changes.length > 1;

  if (collapsed) {
    return (
      <div style={{ ...styles.card, ...styles.cardHandled }}>
        <div style={{ ...styles.head, ...styles.headHandled }}>
          <span style={{ ...styles.icon, color: palette.ink4 }}>
            <ScopeShiftIcon style={{ width: 16, height: 16 }} />
          </span>
          <span style={{ ...styles.title, color: palette.ink3 }}>
            {t("scopeChange.collapsedTitle")}
          </span>
          <span style={styles.spacer} />
          <span style={{ ...styles.pill, ...styles.pillMuted }}>
            <span style={styles.dot} />
            {t("scopeChange.collapsedStatus")}
          </span>
        </div>
        <div style={styles.handledNote} onClick={() => setCollapsed(false)}>
          <span style={styles.handledIcon}>
            <ChevronIcon />
          </span>
          <span>{t("scopeChange.expand")}</span>
        </div>
      </div>
    );
  }

  return (
    <div style={styles.card}>
      <div style={styles.head}>
        <span style={styles.icon}>
          <ScopeShiftIcon style={{ width: 16, height: 16 }} />
        </span>
        <span style={styles.title}>
          {multi
            ? t("scopeChange.title.multi", { count: block.changes.length })
            : t("scopeChange.title.single")}
        </span>
        <span style={styles.spacer} />
        <span style={styles.pill}>
          <span style={styles.dot} />
          {t("scopeChange.pending")}
        </span>
      </div>
      <div style={styles.body}>
        <p style={styles.ctx}>
          {multi
            ? t("scopeChange.description.multi", {
                count: block.changes.length,
              })
            : t("scopeChange.description.single")}
        </p>
        <div style={styles.changes}>
          {block.changes.map((c) => (
            <div style={styles.change} key={c.proposal_id}>
              <span style={styles.kind}>{scopeKindLabel(c.kind, t)}</span>
              <div style={styles.detail}>
                {c.detail_summary ? (
                  <div style={styles.summary}>{c.detail_summary}</div>
                ) : null}
                <div style={styles.text}>{c.detail_text}</div>
              </div>
            </div>
          ))}
        </div>
        <div style={styles.finalizeNote}>
          <span style={styles.finalizeIcon}>
            <AlertIcon />
          </span>
          <span>{t("scopeChange.finalizeNote")}</span>
        </div>
      </div>
      <div style={styles.footer}>
        <span style={styles.hint}>{t("scopeChange.continueHint")}</span>
        <button
          type="button"
          style={{ ...styles.button, ...styles.secondary }}
          onClick={() => setCollapsed(true)}
        >
          {t("scopeChange.collapse")}
        </button>
        <button
          type="button"
          style={{ ...styles.button, ...styles.primary }}
          onClick={() => onContinue(buildContinueDraft(block.changes, t))}
        >
          <ArrowIcon />
          {t("scopeChange.acceptAndContinue")}
        </button>
      </div>
    </div>
  );
}

export const ScopeChangeCard = memo(ScopeChangeCardImpl);

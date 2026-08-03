import { memo } from "react";
import type { CSSProperties } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useI18n } from "../i18n";
import type { Block } from "../types/agent";
import { humanizeToolName } from "../lib/toolLabel";

type ApprovalBlock = Extract<Block, { type: "approval" }>;

type Props = {
  block: ApprovalBlock;
  sessionId: string;
};

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
  cardResolved: {
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
  icon: {
    width: 15,
    height: 15,
    flexShrink: 0,
    color: palette.accent,
  },
  title: {
    fontSize: 12,
    fontWeight: 700,
    color: palette.ink,
  },
  tool: {
    fontFamily: "SF Mono, ui-monospace, Menlo, monospace",
    fontSize: 10,
    fontWeight: 600,
    padding: "2px 7px",
    borderRadius: 5,
    background: palette.sunken,
    color: palette.ink2,
    overflowWrap: "anywhere",
  },
  spacer: {
    flex: 1,
  },
  state: {
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
  },
  dot: {
    width: 5,
    height: 5,
    borderRadius: "50%",
  },
  body: {
    padding: "11px 12px",
  },
  row: {
    display: "flex",
    gap: 9,
    alignItems: "baseline",
    marginBottom: 7,
  },
  key: {
    fontSize: 9.5,
    color: palette.ink4,
    textTransform: "uppercase",
    letterSpacing: "0.05em",
    fontWeight: 600,
    width: 54,
    flexShrink: 0,
    paddingTop: 2,
  },
  command: {
    fontFamily: "SF Mono, ui-monospace, Menlo, monospace",
    fontSize: 11.5,
    color: palette.ink,
    background: palette.sunken,
    padding: "7px 10px",
    borderRadius: 6,
    flex: 1,
    lineHeight: 1.5,
    wordBreak: "break-all",
    whiteSpace: "pre-wrap",
  },
  cwd: {
    fontFamily: "SF Mono, ui-monospace, Menlo, monospace",
    fontSize: 11,
    color: palette.ink3,
    flex: 1,
    paddingTop: 2,
    overflowWrap: "anywhere",
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
    flex: "1 1 240px",
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
  deny: {
    background: palette.panel,
    color: palette.ink2,
    borderColor: palette.line,
  },
  allow: {
    background: palette.accent,
    color: "#fff",
  },
  resolvedNote: {
    display: "flex",
    alignItems: "center",
    gap: 8,
    padding: "9px 12px",
    fontSize: 11.5,
  },
  seq: {
    marginLeft: "auto",
    fontFamily: "SF Mono, ui-monospace, Menlo, monospace",
    fontSize: 9.5,
    color: palette.ink4,
  },
};

function isCriterion(block: ApprovalBlock) {
  return block.request_kind === "criterion";
}

function ShieldIcon({ status }: { status: ApprovalBlock["status"] }) {
  return (
    <svg
      viewBox="0 0 24 24"
      style={{ display: "block", width: 15, height: 15 }}
      stroke="currentColor"
      fill="none"
      strokeWidth="2"
      aria-hidden="true"
    >
      <path d="M12 3l8 4v5c0 4.5-3 7.5-8 9-5-1.5-8-4.5-8-9V7l8-4z" />
      {status === "approved" ? (
        <path d="M9 12l2 2 4-4" />
      ) : status === "rejected" ? (
        <path d="M9.5 9.5l5 5M14.5 9.5l-5 5" />
      ) : (
        <>
          <path d="M12 8v4" />
          <path d="M12 15.5v.5" />
        </>
      )}
    </svg>
  );
}

function MarkIcon({ kind }: { kind: "allow" | "deny" }) {
  return (
    <svg
      viewBox="0 0 24 24"
      style={{ display: "block", width: 13, height: 13 }}
      stroke="currentColor"
      fill="none"
      strokeWidth="2.2"
      aria-hidden="true"
    >
      {kind === "allow" ? (
        <path d="M5 12l5 5L20 7" />
      ) : (
        <path d="M18 6L6 18M6 6l12 12" />
      )}
    </svg>
  );
}

function stateMeta(block: ApprovalBlock, t: ReturnType<typeof useI18n>["t"]) {
  const criterion = isCriterion(block);
  switch (block.status) {
    case "approved":
      return {
        title: criterion
          ? t("approvalCard.approvedCriterion")
          : t("approvalCard.approvedCommand"),
        label: criterion
          ? t("approvalCard.approvedCriterion")
          : t("approvalCard.approvedCommand"),
        color: palette.green,
        bg: "rgba(106,155,92,0.14)",
        note: criterion
          ? t("approvalCard.approvedCriterionNote")
          : t("approvalCard.approvedCommandNote"),
      };
    case "rejected":
      return {
        title: criterion
          ? t("approvalCard.rejectedCriterion")
          : t("approvalCard.rejectedCommand"),
        label: criterion
          ? t("approvalCard.rejectedCriterion")
          : t("approvalCard.rejectedCommand"),
        color: palette.red,
        bg: "rgba(184,80,72,0.12)",
        note: criterion
          ? t("approvalCard.rejectedCriterionNote")
          : t("approvalCard.rejectedCommandNote"),
      };
    case "cancelled":
      return {
        title: t("approvalCard.cancelled"),
        label: t("approvalCard.cancelled"),
        color: palette.ink3,
        bg: "rgba(122,117,104,0.12)",
        note: t("approvalCard.cancelledNote"),
      };
    case "pending":
      return {
        title: criterion
          ? t("approvalCard.pendingCriterionTitle")
          : t("approvalCard.pendingCommandTitle"),
        label: t("approvalCard.pendingLabel"),
        color: palette.accent,
        bg: "rgba(217,119,87,0.14)",
        note: "",
      };
  }
}

function resolve(
  block: ApprovalBlock,
  sessionId: string,
  decision: "approved" | "rejected",
) {
  void invoke("resolve_approval", {
    sessionId,
    runId: block.run_id,
    approvalId: block.approval_id,
    decision,
  }).catch((error) => {
    console.warn("[approval] resolve_approval failed", error);
  });
}

function ApprovalCardImpl({ block, sessionId }: Props) {
  const { t } = useI18n();
  const pending = block.status === "pending";
  const meta = stateMeta(block, t);

  return (
    <div
      style={{
        ...styles.card,
        ...(!pending ? styles.cardResolved : null),
      }}
    >
      <div style={styles.head}>
        <span
          style={{
            ...styles.icon,
            color: pending ? palette.accent : palette.ink4,
          }}
        >
          <ShieldIcon status={block.status} />
        </span>
        <span style={styles.title}>
          {pending
            ? meta.title
            : isCriterion(block)
              ? t("approvalCard.criterionProposal")
              : humanizeToolName(block.tool, t)}
        </span>
        <span style={styles.tool}>
          {isCriterion(block)
            ? t("approvalCard.criterionProposal")
            : humanizeToolName(block.tool, t)}
        </span>
        <span style={styles.spacer} />
        <span
          style={{
            ...styles.state,
            color: meta.color,
            background: meta.bg,
          }}
        >
          <span style={{ ...styles.dot, background: meta.color }} />
          {meta.label}
        </span>
      </div>
      {pending ? (
        <>
          <div style={styles.body}>
            <div
              style={
                isCriterion(block)
                  ? { ...styles.row, marginBottom: 0 }
                  : styles.row
              }
            >
              <span style={styles.key}>
                {isCriterion(block)
                  ? t("approvalCard.criterionLabel")
                  : t("approvalCard.commandLabel")}
              </span>
              <div style={styles.command}>
                <span style={{ color: palette.accent }}>$ </span>
                {block.summary || block.command}
              </div>
            </div>
            {!isCriterion(block) && (
              <div style={{ ...styles.row, marginBottom: 0 }}>
                <span style={styles.key}>{t("approvalCard.directory")}</span>
                <span style={styles.cwd}>{block.cwd}</span>
              </div>
            )}
          </div>
          <div style={styles.footer}>
            <span style={styles.hint}>
              {isCriterion(block)
                ? t("approvalCard.criterionHint")
                : t("approvalCard.commandHint")}
            </span>
            <button
              type="button"
              style={{ ...styles.button, ...styles.deny }}
              onClick={() => resolve(block, sessionId, "rejected")}
            >
              <MarkIcon kind="deny" />
              {isCriterion(block)
                ? t("approvalCard.denyCriterion")
                : t("approvalCard.denyCommand")}
            </button>
            <button
              type="button"
              style={{ ...styles.button, ...styles.allow }}
              onClick={() => resolve(block, sessionId, "approved")}
            >
              <MarkIcon kind="allow" />
              {isCriterion(block)
                ? t("approvalCard.allowCriterion")
                : t("approvalCard.allowCommand")}
            </button>
          </div>
        </>
      ) : (
        <div style={{ ...styles.resolvedNote, color: meta.color }}>
          <MarkIcon kind={block.status === "approved" ? "allow" : "deny"} />
          {meta.note}
          <span style={styles.seq}>{block.approval_id}</span>
        </div>
      )}
    </div>
  );
}

export const ApprovalCard = memo(ApprovalCardImpl);

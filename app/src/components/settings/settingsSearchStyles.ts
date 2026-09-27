import type { CSSProperties } from "react";

export const styles = {
  root: {
    maxWidth: 760,
  },
  header: {
    alignItems: "flex-start",
    gap: 16,
    justifyContent: "space-between",
    marginBottom: 12,
  },
  headerCopy: {
    display: "flex",
    flexDirection: "column",
    gap: 2,
    minWidth: 0,
  },
  description: {
    color: "var(--ink-3)",
    fontSize: 11.5,
    lineHeight: 1.5,
    marginTop: 3,
    maxWidth: 640,
  },
  field: {
    display: "flex",
    flexDirection: "column",
    gap: 3,
    marginBottom: 10,
  },
  label: {
    color: "var(--ink-3)",
    fontSize: 10.5,
    fontWeight: 600,
  },
  input: {
    background: "var(--panel)",
    border: "1px solid var(--line)",
    borderRadius: 6,
    color: "var(--ink)",
    font: "inherit",
    fontSize: 12,
    padding: "6px 10px",
    width: "100%",
  },
  monoInput: {
    fontFamily: '"SF Mono", monospace',
    fontSize: 11,
  },
  hint: {
    color: "var(--ink-3)",
    fontSize: 10.5,
    marginTop: 1,
  },
  statusGroup: {
    alignItems: "center",
    display: "inline-flex",
    flexShrink: 0,
    gap: 6,
  },
  status: {
    alignItems: "center",
    background: "var(--panel)",
    border: "1px solid var(--line)",
    borderRadius: 6,
    color: "var(--ink-2)",
    display: "inline-flex",
    fontSize: 11,
    gap: 6,
    padding: "5px 9px",
    whiteSpace: "nowrap",
  },
  checkBtn: {
    background: "transparent",
    border: "1px solid var(--line)",
    borderRadius: 6,
    color: "var(--ink-2)",
    fontSize: 11,
    fontWeight: 600,
    padding: "4px 10px",
    cursor: "pointer",
    whiteSpace: "nowrap",
  },
  dot: {
    borderRadius: "50%",
    height: 7,
    width: 7,
  },
  dotReady: {
    background: "var(--green)",
    boxShadow: "0 0 0 2px rgba(106, 155, 92, 0.13)",
  },
  dotMissing: {
    background: "var(--ink-4)",
  },
  actions: {
    display: "flex",
    gap: 8,
    justifyContent: "flex-end",
    marginTop: 4,
  },
  error: {
    color: "var(--red)",
    fontSize: 11,
    textAlign: "right",
  },
  ddgRow: {
    alignItems: "center",
    display: "flex",
    gap: 10,
    justifyContent: "space-between",
  },
} satisfies Record<string, CSSProperties>;

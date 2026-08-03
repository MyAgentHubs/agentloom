export type WorkerReport = {
  agent: string;
  assignment_id: string;
  status: string;
  statusDetail?: string;
  changed_files: string[];
  final_text: string;
};

const WORKER_REPORT_PREFIX = "[Worker report]";

type FieldMatch = {
  labelStart: number;
  valueStart: number;
};

function findField(
  text: string,
  field: string,
  fromIndex: number,
): FieldMatch | null {
  const escaped = field.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  const matcher = new RegExp(`(^|\\s)${escaped}:[\\t ]*`, "g");
  matcher.lastIndex = fromIndex;
  const match = matcher.exec(text);
  if (!match) return null;
  return {
    labelStart: match.index + match[1].length,
    valueStart: matcher.lastIndex,
  };
}

function stripLeadingLineBreak(text: string): string {
  return text.replace(/^(?:\r\n|\n|\r)/, "");
}

/** Parses the durable, plain-text report emitted for an agent-team worker. */
export function parseWorkerReport(text: string): WorkerReport | null {
  if (!text.startsWith(WORKER_REPORT_PREFIX)) return null;

  const agentField = findField(text, "agent", WORKER_REPORT_PREFIX.length);
  if (!agentField) return null;
  const assignmentField = findField(
    text,
    "assignment_id",
    agentField.valueStart,
  );
  if (!assignmentField) return null;
  const statusField = findField(text, "status", assignmentField.valueStart);
  if (!statusField) return null;
  const changedFilesField = findField(
    text,
    "changed_files",
    statusField.valueStart,
  );
  if (!changedFilesField) return null;
  const finalTextField = findField(
    text,
    "final_text",
    changedFilesField.valueStart,
  );
  if (!finalTextField) return null;

  const agent = text
    .slice(agentField.valueStart, assignmentField.labelStart)
    .trim();
  const assignmentId = text
    .slice(assignmentField.valueStart, statusField.labelStart)
    .trim();
  const statusAndMetadata = text
    .slice(statusField.valueStart, changedFilesField.labelStart)
    .trim();
  const statusMatch = /^(\S+)/.exec(statusAndMetadata);
  if (!agent || !assignmentId || !statusMatch) return null;
  const statusDetail = statusAndMetadata.slice(statusMatch[0].length).trim();

  const changedFilesText = stripLeadingLineBreak(
    text.slice(changedFilesField.valueStart, finalTextField.labelStart),
  ).trimEnd();
  const changedFileLines = changedFilesText
    .split(/\r\n|\n|\r/)
    .filter((line) => line.trim() !== "");
  if (
    changedFileLines.length === 0 ||
    changedFileLines.some((line) => !/^\s*-\s+\S/.test(line))
  ) {
    return null;
  }

  const changedFileEntries = changedFileLines.map((line) =>
    line.replace(/^\s*-\s+/, ""),
  );
  const noneEntries = changedFileEntries.filter((entry) => entry === "(none)");
  if (
    noneEntries.length > 0 &&
    (noneEntries.length !== 1 || changedFileEntries.length !== 1)
  ) {
    return null;
  }

  return {
    agent,
    assignment_id: assignmentId,
    status: statusMatch[1],
    ...(statusDetail ? { statusDetail } : {}),
    changed_files: noneEntries.length === 1 ? [] : changedFileEntries,
    final_text: stripLeadingLineBreak(text.slice(finalTextField.valueStart)),
  };
}

import { memo, useState } from "react";
import Anser from "anser";
import type { Block } from "../types/agent";
import { useI18n, type I18nKey } from "../i18n";
import { humanizeToolName } from "../lib/toolLabel";

type ToolBlock = Extract<Block, { type: "tool" }>;

const TAIL_LINES = 30;

function badge(status: ToolBlock["status"]): string {
  switch (status) {
    case "running":
      return "run";
    case "ok":
      return "done";
    case "failed":
      return "fail";
    case "interrupted":
      return "intr";
  }
}

const badgeStatusKey: Record<ToolBlock["status"], I18nKey> = {
  running: "toolCard.status.running",
  ok: "toolCard.status.done",
  failed: "toolCard.status.failed",
  interrupted: "toolCard.status.interrupted",
};

function AnsiPre({ text }: { text: string }) {
  try {
    const html = Anser.ansiToHtml(Anser.escapeForHtml(text));
    return (
      <pre
        className="toolcard__pre"
        dangerouslySetInnerHTML={{ __html: html }}
      />
    );
  } catch {
    return <pre className="toolcard__pre">{text}</pre>;
  }
}

function ToolCardImpl({
  block,
  compact,
}: {
  block: ToolBlock;
  compact?: boolean;
}) {
  const { t } = useI18n();
  const isFailed = block.status === "failed";
  const [open, setOpen] = useState(isFailed);
  const [showAll, setShowAll] = useState(false);
  const cls = badge(block.status);

  if (compact || block.card === "compact") {
    return (
      <div className="toolcard toolcard--compact">
        <span className="toolcard__tool">
          {humanizeToolName(block.tool, t)}
        </span>
        {block.summary !== block.tool && (
          <span className="toolcard__summary">{block.summary}</span>
        )}
        <span className={`toolcard__badge toolcard__badge--${cls}`}>
          {t(badgeStatusKey[block.status])}
        </span>
      </div>
    );
  }

  const output = block.output ?? "";
  const allLines = output.split("\n");
  const truncated = !showAll && allLines.length > TAIL_LINES;
  const shownLines = truncated ? allLines.slice(-TAIL_LINES) : allLines;
  const hiddenCount = allLines.length - shownLines.length;

  return (
    <div className="toolcard toolcard--command">
      <button
        type="button"
        className="toolcard__head"
        onClick={() => setOpen((v) => !v)}
        aria-expanded={open}
      >
        <span className="toolcard__cmd">{block.summary}</span>
        <span className={`toolcard__badge toolcard__badge--${cls}`}>
          {t(badgeStatusKey[block.status])}
        </span>
        {block.exit_code !== null && (
          <span className="toolcard__exit">exit {block.exit_code}</span>
        )}
        <span
          className={`toolcard__chev${open ? " toolcard__chev--open" : ""}`}
        >
          ⌄
        </span>
      </button>
      {open && output !== "" && (
        <div className="toolcard__out">
          {truncated && (
            <button
              type="button"
              className="toolcard__more"
              onClick={() => setShowAll(true)}
            >
              {t("toolCard.hiddenLinesAbove", { n: hiddenCount })}
            </button>
          )}
          <AnsiPre text={shownLines.join("\n")} />
        </div>
      )}
    </div>
  );
}

export const ToolCard = memo(ToolCardImpl);

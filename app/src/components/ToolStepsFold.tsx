import { useState } from "react";
import { useI18n } from "../i18n";
import type { ToolBlock } from "../lib/streamItems";
import { ToolCard } from "./ToolCard";

export function ToolStepsFold({
  blocks,
  defaultOpen,
}: {
  blocks: ToolBlock[];
  defaultOpen?: boolean;
}) {
  const { t } = useI18n();
  const [open, setOpen] = useState(defaultOpen ?? false);
  return (
    <details
      className="toolfold"
      open={open}
      onToggle={(event) => setOpen(event.currentTarget.open)}
    >
      <summary className="toolfold__sum">
        <svg className="toolfold__chevron" viewBox="0 0 24 24" aria-hidden>
          <path d="M4 17l6-6-6-6" />
        </svg>
        <span className="toolfold__label">
          {t("stream.toolFold.steps", { n: blocks.length })}
        </span>
        <span className="toolcard__badge toolcard__badge--done">
          {t("toolCard.status.done")}
        </span>
      </summary>
      <div className="toolfold__list">
        {blocks.map((block) => (
          <ToolCard key={block.id} block={block} compact />
        ))}
      </div>
    </details>
  );
}

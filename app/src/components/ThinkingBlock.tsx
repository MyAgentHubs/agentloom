import { memo, useState } from "react";
import { useI18n } from "../i18n";

function ThinkingBlockImpl({ text }: { text: string }) {
  const { t } = useI18n();
  const [open, setOpen] = useState(false);

  return (
    <div className="thinking">
      <button
        type="button"
        className="thinking__head"
        onClick={() => setOpen((v) => !v)}
        aria-expanded={open}
      >
        <span className="thinking__lb">
          thinking · {open ? t("thinking.collapse") : t("thinking.expand")}
        </span>
      </button>
      {open && <div className="thinking__body">{text}</div>}
    </div>
  );
}

export const ThinkingBlock = memo(ThinkingBlockImpl);

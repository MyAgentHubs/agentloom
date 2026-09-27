import { useState } from "react";
import { useI18n } from "../i18n";

// Huge text blocks default to collapsed: parsing the whole thing as markdown synchronously can block the main thread for seconds.
// The threshold was lowered to 50k: remark took about 205ms for 99k characters
// versus 25ms for 50k. Streaming reparses every chunk, and WKWebView is slower.
export const HUGE_TEXT_BLOCK_CHARS = 50_000;
// Show enough text to identify pasted content without rendering the whole block.
const HUGE_TEXT_PREVIEW_CHARS = 4000;

// Huge pasted text defaults to a collapsed plain-text preview. Parsing the
// entire block as markdown can block the main thread for seconds. Even when
// expanded, pasted logs or code remain plain text to avoid that pause.
export function HugeTextBlock({ text }: { text: string }) {
  const { t } = useI18n();
  const [open, setOpen] = useState(false);
  let preview = text.slice(0, HUGE_TEXT_PREVIEW_CHARS);
  // Avoid splitting a surrogate pair, which would render U+FFFD.
  if (/[\uD800-\uDBFF]$/.test(preview)) {
    preview = preview.slice(0, -1);
  }
  return (
    <div className="huge-text">
      <div className="huge-text__body">{open ? text : preview}</div>
      <button
        type="button"
        className="huge-text__toggle"
        onClick={() => setOpen((v) => !v)}
        aria-expanded={open}
      >
        {open
          ? t("chat.hugeTextExpanded")
          : t("chat.hugeTextCollapsed", { chars: text.length })}
      </button>
    </div>
  );
}

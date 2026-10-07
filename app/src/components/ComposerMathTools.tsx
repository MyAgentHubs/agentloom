import { useEffect, useRef, useState, type RefObject } from "react";
import { useI18n } from "../i18n";
import { insertText } from "../lib/insertText";
import { insertMath } from "../lib/mathEdit";
import { chatMathExamples } from "../lib/chatMathExamples";
import { MarkdownBody } from "./MarkdownBody";
import "../styles/composerMath.css";

export function ComposerMathTools({
  textarea,
  composing,
  disabled,
}: {
  textarea: RefObject<HTMLTextAreaElement | null>;
  composing: RefObject<boolean>;
  disabled: boolean;
}) {
  const { t } = useI18n();
  const [open, setOpen] = useState(false);
  const root = useRef<HTMLDivElement>(null);
  const trigger = useRef<HTMLButtonElement>(null);
  useEffect(() => {
    if (!open) return;
    const outside = (event: PointerEvent) => {
      if (!root.current?.contains(event.target as Node)) setOpen(false);
    };
    document.addEventListener("pointerdown", outside);
    return () => document.removeEventListener("pointerdown", outside);
  }, [open]);
  const insert = (display: boolean) => {
    const el = textarea.current;
    if (!el || disabled || composing.current) return;
    insertText(
      el,
      insertMath(el.value, el.selectionStart, el.selectionEnd, display),
    );
    setOpen(false);
  };
  return (
    <div
      ref={root}
      className="composer-math"
      onKeyDown={(event) => {
        if (event.key === "Escape" && open) {
          event.stopPropagation();
          setOpen(false);
          trigger.current?.focus();
        }
      }}
      onBlur={(event) => {
        if (
          event.relatedTarget &&
          !event.currentTarget.contains(event.relatedTarget)
        )
          setOpen(false);
      }}
    >
      <button
        ref={trigger}
        type="button"
        className="composer__icon"
        disabled={disabled}
        aria-label={t("composer.math.label")}
        title={t("composer.math.label")}
        aria-expanded={open}
        onMouseDown={(event) => event.preventDefault()}
        onClick={() => {
          if (composing.current) return;
          setOpen(!open);
          trigger.current?.focus();
        }}
      >
        <svg viewBox="0 0 24 24" aria-hidden="true">
          <path d="M18 4H6l8 8-8 8h12" />
        </svg>
      </button>
      {open && !disabled && (
        <div className="composer-math__popover">
          <button
            type="button"
            onMouseDown={(event) => event.preventDefault()}
            onClick={() => insert(false)}
          >
            {t("composer.math.inline")} <kbd>⌘/Ctrl+Shift+M</kbd>
          </button>
          <button
            type="button"
            onMouseDown={(event) => event.preventDefault()}
            onClick={() => insert(true)}
          >
            {t("composer.math.display")} <kbd>⌘/Ctrl+Shift+E</kbd>
          </button>
          <p>{t("composer.math.hint")}</p>
          <details>
            <summary tabIndex={0}>{t("composer.math.examples")}</summary>
            {chatMathExamples.map((source) => (
              <div key={source} className="composer-math__example">
                <pre>{source}</pre>
                <MarkdownBody streaming={false}>{source}</MarkdownBody>
              </div>
            ))}
          </details>
        </div>
      )}
    </div>
  );
}

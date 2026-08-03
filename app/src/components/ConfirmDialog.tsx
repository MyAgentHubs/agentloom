import { useEffect, useId, useRef } from "react";
import { useI18n } from "../i18n";

type Props = {
  open: boolean;
  title: string;
  body?: React.ReactNode;
  confirmLabel: string;
  cancelLabel?: string;
  tone?: "danger";
  onConfirm: () => void;
  onCancel: () => void;
};

export function ConfirmDialog({
  open,
  title,
  body,
  confirmLabel,
  cancelLabel,
  onConfirm,
  onCancel,
}: Props) {
  const { t } = useI18n();
  const titleId = useId();
  const bodyId = useId();
  const cancelRef = useRef<HTMLButtonElement>(null);

  useEffect(() => {
    if (open) cancelRef.current?.focus();
  }, [open]);

  useEffect(() => {
    if (!open) return;
    const handler = (e: KeyboardEvent) => {
      if (e.key === "Escape") onCancel();
    };
    document.addEventListener("keydown", handler);
    return () => document.removeEventListener("keydown", handler);
  }, [open, onCancel]);

  if (!open) return null;

  return (
    <div className="dialog__backdrop" onClick={onCancel}>
      <div
        className="dialog"
        role="dialog"
        aria-modal="true"
        aria-labelledby={titleId}
        aria-describedby={body ? bodyId : undefined}
        onClick={(e) => e.stopPropagation()}
      >
        <h2 className="dialog__title" id={titleId}>
          {title}
        </h2>
        {body && (
          <p className="dialog__body" id={bodyId}>
            {body}
          </p>
        )}
        <div className="dialog__actions">
          <button
            ref={cancelRef}
            type="button"
            className="dialog__btn"
            onClick={onCancel}
          >
            {cancelLabel ?? t("confirmDialog.cancel")}
          </button>
          <button
            type="button"
            className="dialog__btn dialog__btn--danger"
            onClick={onConfirm}
          >
            {confirmLabel}
          </button>
        </div>
      </div>
    </div>
  );
}

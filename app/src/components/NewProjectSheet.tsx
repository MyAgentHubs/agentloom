import { useEffect, useRef, useState, type KeyboardEvent } from "react";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import { useI18n } from "../i18n";

export type NewProjectArgs = {
  name: string;
  newUnderDefault: boolean;
  existingPath: string | null;
  icon: string | null;
};

type Props = {
  open: boolean;
  onClose: () => void;
  onCreate?: (args: NewProjectArgs) => void | Promise<void>;
  mode?: "create" | "edit";
  initial?: { name: string; icon: string | null };
  onSave?: (args: {
    name: string;
    icon: string | null;
  }) => void | Promise<void>;
  onRemove?: () => void;
};

export const PROJECT_EMOJIS = ["📕", "📝", "📊", "🎨", "🐍", "🚀", "📁", "💡"];

export function NewProjectSheet({
  open,
  onClose,
  onCreate,
  mode = "create",
  initial,
  onSave,
  onRemove,
}: Props) {
  const { t } = useI18n();
  const sheetRef = useRef<HTMLDivElement>(null);
  const nameRef = useRef<HTMLInputElement>(null);
  const [name, setName] = useState("");
  const [existingPath, setExistingPath] = useState<string | null>(null);
  const [newUnderDefault, setNewUnderDefault] = useState(true);
  const [icon, setIcon] = useState<string | null>(PROJECT_EMOJIS[0]);
  const [submitting, setSubmitting] = useState(false);

  useEffect(() => {
    if (!open) return;
    setName(mode === "edit" ? (initial?.name ?? "") : "");
    setExistingPath(null);
    setNewUnderDefault(true);
    setIcon(mode === "edit" ? (initial?.icon ?? null) : PROJECT_EMOJIS[0]);
    setSubmitting(false);
    nameRef.current?.focus();
  }, [initial?.icon, initial?.name, mode, open]);

  if (!open) return null;

  const trimmedName = name.trim();
  const previewName = trimmedName || t("newProject.name.placeholder");

  async function chooseExistingFolder() {
    try {
      const selected = await openDialog({ directory: true, multiple: false });
      if (typeof selected !== "string") return;
      setExistingPath(selected);
      setNewUnderDefault(false);
    } catch {
      // Closing or failing to open the native picker leaves the current choice intact.
    }
  }

  function handleKeyDown(event: KeyboardEvent<HTMLDivElement>) {
    if (event.key === "Escape" && !submitting) {
      event.preventDefault();
      onClose();
      return;
    }
    if (event.key !== "Tab") return;
    const focusable = Array.from(
      sheetRef.current?.querySelectorAll<HTMLElement>(
        'button:not([disabled]), input:not([disabled]), [tabindex]:not([tabindex="-1"])',
      ) ?? [],
    );
    if (focusable.length === 0) return;
    const first = focusable[0];
    const last = focusable[focusable.length - 1];
    if (event.shiftKey && document.activeElement === first) {
      event.preventDefault();
      last.focus();
    } else if (!event.shiftKey && document.activeElement === last) {
      event.preventDefault();
      first.focus();
    }
  }

  async function handleSubmit() {
    if (!trimmedName || submitting) return;
    setSubmitting(true);
    try {
      if (mode === "edit") {
        await onSave?.({ name: trimmedName, icon });
      } else {
        await onCreate?.({
          name: trimmedName,
          newUnderDefault,
          existingPath: newUnderDefault ? null : existingPath,
          icon,
        });
      }
      onClose();
    } catch {
      setSubmitting(false);
    }
  }

  return (
    <div
      className="new-project-backdrop"
      onMouseDown={(event) => {
        if (event.target === event.currentTarget && !submitting) onClose();
      }}
    >
      <div
        ref={sheetRef}
        className="new-project-sheet"
        role="dialog"
        aria-modal="true"
        aria-labelledby="new-project-title"
        onKeyDown={handleKeyDown}
      >
        <h2 id="new-project-title">
          {t(mode === "edit" ? "newProject.editTitle" : "newProject.title")}
        </h2>
        <p className="new-project-sheet__hint">{t("newProject.hint")}</p>

        <div className="new-project-field">
          <label htmlFor="new-project-name">{t("newProject.name.label")}</label>
          <input
            ref={nameRef}
            id="new-project-name"
            type="text"
            value={name}
            placeholder={t("newProject.name.placeholder")}
            disabled={submitting}
            onChange={(event) => setName(event.target.value)}
          />
        </div>

        {mode === "create" && (
          <div className="new-project-field">
            <span className="new-project-field__label">
              {t("newProject.location.label")}
            </span>
            <div
              className="new-project-location"
              role="radiogroup"
              aria-label={t("newProject.location.label")}
            >
              <button
                type="button"
                className={`new-project-location__option${newUnderDefault ? " selected" : ""}`}
                role="radio"
                aria-checked={newUnderDefault}
                disabled={submitting}
                onClick={() => setNewUnderDefault(true)}
              >
                <span
                  className="new-project-location__radio"
                  aria-hidden="true"
                />
                <span>
                  <span className="new-project-location__title">
                    {t("newProject.location.newFolder")}
                  </span>
                  <span className="new-project-location__detail">
                    {t("newProject.location.willCreate", {
                      path: `~/AgentLoom/${previewName}`,
                    })}
                  </span>
                </span>
              </button>
              <button
                type="button"
                className={`new-project-location__option${!newUnderDefault ? " selected" : ""}`}
                role="radio"
                aria-checked={!newUnderDefault}
                disabled={submitting}
                onClick={chooseExistingFolder}
              >
                <span
                  className="new-project-location__radio"
                  aria-hidden="true"
                />
                <span>
                  <span className="new-project-location__title">
                    {t("newProject.location.existingFolder")}
                  </span>
                  <span className="new-project-location__detail">
                    {existingPath ?? t("newProject.location.existingHint")}
                  </span>
                </span>
              </button>
            </div>
          </div>
        )}

        <div className="new-project-field">
          <span className="new-project-field__label">
            {t("newProject.identity.label")}
          </span>
          <div
            className="new-project-emojis"
            role="radiogroup"
            aria-label={t("newProject.identity.label")}
          >
            {PROJECT_EMOJIS.map((projectEmoji) => (
              <button
                key={projectEmoji}
                type="button"
                className={`new-project-emoji${icon === projectEmoji ? " selected" : ""}`}
                role="radio"
                aria-checked={icon === projectEmoji}
                aria-label={t("newProject.identity.option", {
                  emoji: projectEmoji,
                })}
                disabled={submitting}
                onClick={() => setIcon(projectEmoji)}
              >
                {projectEmoji}
              </button>
            ))}
          </div>
        </div>

        <div className="new-project-sheet__actions">
          {mode === "edit" && onRemove && (
            <button
              type="button"
              className="danger"
              disabled={submitting}
              onClick={onRemove}
            >
              {t("newProject.cta.remove")}
            </button>
          )}
          <button type="button" disabled={submitting} onClick={onClose}>
            {t("newProject.cta.cancel")}
          </button>
          <button
            type="button"
            className="primary"
            disabled={!trimmedName || submitting}
            onClick={handleSubmit}
          >
            {t(
              mode === "edit" ? "newProject.cta.save" : "newProject.cta.create",
            )}
          </button>
        </div>
      </div>
    </div>
  );
}

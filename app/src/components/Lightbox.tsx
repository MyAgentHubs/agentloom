import { useEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { useI18n } from "../i18n";
import {
  getAttachmentDataUri,
  setAttachmentDataUri,
} from "../lib/attachmentCache";
import { useAttachmentPort } from "../lib/attachmentPortContext";

type Props = {
  path: string;
  sessionId?: string | null;
  onClose: () => void;
};

export function Lightbox({ path, sessionId, onClose }: Props) {
  const { t } = useI18n();
  const attachmentPort = useAttachmentPort();
  const closeRef = useRef<HTMLButtonElement>(null);
  const [dataUri, setDataUri] = useState<string | null>(() =>
    getAttachmentDataUri(path, sessionId),
  );
  const [failed, setFailed] = useState(false);

  useEffect(() => {
    const previouslyFocused = document.activeElement as HTMLElement | null;
    closeRef.current?.focus();
    const closeOnEscape = (event: KeyboardEvent) => {
      if (event.key === "Escape") onClose();
    };
    document.addEventListener("keydown", closeOnEscape);
    return () => {
      document.removeEventListener("keydown", closeOnEscape);
      previouslyFocused?.focus();
    };
  }, [onClose]);

  useEffect(() => {
    let cancelled = false;
    const cached = getAttachmentDataUri(path, sessionId);
    if (cached) {
      setDataUri(cached);
      setFailed(false);
      return;
    }

    setFailed(false);
    void attachmentPort
      .resolveImageSrc(path, sessionId)
      .then((src) => {
        if (cancelled) return;
        if (src) {
          setAttachmentDataUri(path, sessionId, src);
          setDataUri(src);
        } else {
          setFailed(true);
        }
      })
      .catch(() => {
        if (!cancelled) setFailed(true);
      });

    return () => {
      cancelled = true;
    };
  }, [attachmentPort, path, sessionId]);

  return createPortal(
    <div
      data-testid="lightbox-backdrop"
      role="dialog"
      aria-modal="true"
      aria-label={t("lightbox.label")}
      onClick={(event) => {
        if (event.currentTarget === event.target) onClose();
      }}
      style={{
        position: "fixed",
        inset: 0,
        zIndex: 2000,
        display: "flex",
        alignItems: "center",
        justifyContent: "center",
        padding: 24,
        boxSizing: "border-box",
        background: "rgba(31, 24, 18, 0.88)",
        backdropFilter: "blur(3px)",
      }}
    >
      <button
        ref={closeRef}
        type="button"
        aria-label={t("lightbox.close")}
        onClick={onClose}
        style={{
          position: "fixed",
          top: 18,
          right: 18,
          width: 36,
          height: 36,
          border: "1px solid rgba(255, 236, 210, 0.34)",
          borderRadius: 18,
          background: "rgba(73, 55, 39, 0.82)",
          color: "#fff3df",
          cursor: "pointer",
          fontSize: 22,
          lineHeight: 1,
        }}
      >
        ×
      </button>
      {dataUri ? (
        <img
          src={dataUri}
          alt={t("lightbox.imageAlt")}
          onClick={(event) => event.stopPropagation()}
          style={{
            display: "block",
            maxWidth: "90vw",
            maxHeight: "90vh",
            objectFit: "contain",
            borderRadius: 6,
            boxShadow: "0 20px 70px rgba(0, 0, 0, 0.48)",
          }}
        />
      ) : (
        <span role="status" style={{ color: "#fff3df" }}>
          {failed ? t("lightbox.loadFailed") : t("lightbox.loading")}
        </span>
      )}
    </div>,
    document.body,
  );
}

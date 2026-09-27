import { invoke } from "@tauri-apps/api/core";
import { useCallback, useEffect, useMemo, useState } from "react";
import { useI18n } from "../i18n";
import {
  getAttachmentDataUri,
  setAttachmentDataUri,
} from "../lib/attachmentCache";
import { ImageContextTarget } from "./ImageContextMenu";
import { PreviewablePath } from "./localMarkdownImage";
import "../styles/chatImage.css";

type AttachmentContent = {
  kind: "text" | "image" | "binary";
  imageBase64?: string;
  mediaType?: string;
};

export function fileName(path: string): string {
  return path.split(/[\\/]/).pop() || path;
}

function isRelativeImagePath(path: string): boolean {
  return (
    !path.startsWith("/") &&
    !path.startsWith("~/") &&
    !/^[A-Za-z]:[\\/]/.test(path)
  );
}

function mediaTypeFromPath(path: string): string {
  const extension = path.split(".").pop()?.toLowerCase();
  if (extension === "jpg" || extension === "jpeg") return "image/jpeg";
  if (extension === "svg") return "image/svg+xml";
  if (extension === "webp") return "image/webp";
  if (extension === "gif") return "image/gif";
  if (extension === "bmp") return "image/bmp";
  return "image/png";
}

function useAttachmentImage(
  path: string,
  fallbackMediaType: string,
  sessionId?: string | null,
) {
  const [dataUri, setDataUri] = useState<string | null>(() =>
    getAttachmentDataUri(path, sessionId),
  );
  const [failed, setFailed] = useState(false);

  useEffect(() => {
    let cancelled = false;
    const cached = getAttachmentDataUri(path, sessionId);
    if (cached) {
      setDataUri(cached);
      setFailed(false);
      return;
    }
    setFailed(false);

    void invoke<AttachmentContent>("read_attachment", {
      path,
      sessionId: sessionId ?? null,
    })
      .then((attachment) => {
        if (cancelled) return;
        if (attachment.imageBase64) {
          const mediaType = attachment.mediaType || fallbackMediaType;
          const nextDataUri = `data:${mediaType};base64,${attachment.imageBase64}`;
          setAttachmentDataUri(path, sessionId, nextDataUri);
          setDataUri(nextDataUri);
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
  }, [fallbackMediaType, path, sessionId]);

  return { dataUri, failed };
}
export function ImageArtifactChips({
  paths,
  sessionId,
  onOpenPreview,
  onOpenLightbox,
}: {
  paths: string[];
  sessionId?: string | null;
  onOpenPreview: (path: string) => void;
  onOpenLightbox: (path: string) => void;
}) {
  const [contentByPath, setContentByPath] = useState<Record<string, string>>(
    {},
  );
  const onContent = useCallback((path: string, base64: string) => {
    setContentByPath((current) =>
      current[path] === base64 ? current : { ...current, [path]: base64 },
    );
  }, []);
  const winnerByContent = useMemo(() => {
    const winners = new Map<string, string>();
    paths.forEach((path) => {
      const content = contentByPath[path];
      if (!content) return;
      const current = winners.get(content);
      if (
        !current ||
        isRelativeImagePath(path) ||
        !isRelativeImagePath(current)
      ) {
        winners.set(content, path);
      }
    });
    return winners;
  }, [contentByPath, paths]);

  return (
    <div
      style={{
        display: "flex",
        flexWrap: "wrap",
        alignItems: "flex-start",
        gap: 6,
        marginTop: 6,
      }}
    >
      {paths.map((path) => (
        <ImageArtifactThumbnail
          key={path}
          path={path}
          sessionId={sessionId}
          onOpenPreview={onOpenPreview}
          onOpenLightbox={onOpenLightbox}
          onContent={onContent}
          hidden={
            contentByPath[path] != null &&
            winnerByContent.get(contentByPath[path]) !== path
          }
        />
      ))}
    </div>
  );
}

function ImageArtifactThumbnail({
  path,
  sessionId,
  onOpenPreview,
  onOpenLightbox,
  onContent,
  hidden,
}: {
  path: string;
  sessionId?: string | null;
  onOpenPreview: (path: string) => void;
  onOpenLightbox: (path: string) => void;
  onContent?: (path: string, base64: string) => void;
  hidden?: boolean;
}) {
  const { t } = useI18n();
  const name = fileName(path);
  const { dataUri, failed } = useAttachmentImage(
    path,
    mediaTypeFromPath(path),
    sessionId,
  );

  useEffect(() => {
    if (!dataUri || !onContent) return;
    const commaIndex = dataUri.indexOf(",");
    if (commaIndex >= 0) onContent(path, dataUri.slice(commaIndex + 1));
  }, [dataUri, onContent, path]);

  if (failed) {
    return <PreviewablePath path={path} onOpenPreview={onOpenPreview} />;
  }

  if (hidden) return null;

  if (!dataUri) {
    return (
      <div
        role="status"
        title={path}
        style={{
          boxSizing: "border-box",
          width: 160,
          maxWidth: "100%",
          minHeight: 96,
          padding: 10,
          border: "1px solid var(--line)",
          borderRadius: 8,
          background: "var(--panel)",
          color: "var(--ink-3)",
          display: "flex",
          flexDirection: "column",
          justifyContent: "flex-end",
          gap: 4,
          fontSize: 11,
        }}
      >
        <span style={{ color: "var(--ink-2)", overflowWrap: "anywhere" }}>
          {name}
        </span>
        <span>{t("messageContent.imageLoading")}</span>
      </div>
    );
  }

  return (
    <ImageContextTarget path={path} dataUri={dataUri}>
      {(contextProps) => (
        <button
          type="button"
          title={path}
          aria-label={t("messageContent.imageArtifact.preview", { name })}
          aria-haspopup="menu"
          onClick={() => onOpenLightbox(path)}
          {...contextProps}
          style={{
            boxSizing: "border-box",
            maxWidth: "100%",
            padding: 0,
            border: "1px solid var(--line)",
            borderRadius: 8,
            overflow: "hidden",
            background: "var(--panel)",
            color: "var(--ink-2)",
            cursor: "pointer",
            display: "inline-flex",
            flexDirection: "column",
            alignItems: "stretch",
          }}
        >
          <img
            className="al-chat-image"
            src={dataUri}
            alt={name}
            style={{
              display: "block",
              maxHeight: 240,
              maxWidth: "100%",
              objectFit: "contain",
              height: "auto",
              cursor: "pointer",
            }}
          />
          <span
            style={{
              padding: "4px 7px",
              fontSize: 11,
              lineHeight: 1.3,
              textAlign: "left",
              overflowWrap: "anywhere",
            }}
          >
            {name}
          </span>
        </button>
      )}
    </ImageContextTarget>
  );
}

export function ImageBlockContent({
  path,
  mediaType,
  sessionId,
  onOpenPreview,
  onOpenLightbox,
}: {
  path: string;
  mediaType: string;
  sessionId?: string | null;
  onOpenPreview?: (path: string) => void;
  onOpenLightbox?: (path: string) => void;
}) {
  const { t } = useI18n();
  const { dataUri, failed } = useAttachmentImage(path, mediaType, sessionId);

  if (dataUri) {
    return (
      <ImageContextTarget path={path} dataUri={dataUri}>
        {(contextProps) => (
          <img
            src={dataUri}
            alt={fileName(path)}
            tabIndex={0}
            aria-haspopup="menu"
            onClick={() => onOpenLightbox?.(path)}
            {...contextProps}
            className="al-chat-image"
            style={{
              cursor: onOpenLightbox ? "zoom-in" : undefined,
            }}
          />
        )}
      </ImageContextTarget>
    );
  }
  if (failed) {
    return onOpenPreview ? (
      <PreviewablePath path={path} onOpenPreview={onOpenPreview} />
    ) : (
      <em>{t("messageContent.imageLoadFailed")}</em>
    );
  }
  return <em role="status">{t("messageContent.imageLoading")}</em>;
}

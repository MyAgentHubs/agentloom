import { invoke } from "@tauri-apps/api/core";
import { useEffect, useState } from "react";
import { useI18n } from "../i18n";
import { renderBackendError } from "../lib/backendMsg";
import type { Block } from "../types/agent";
import { CodeBlock } from "./CodeBlock";
import { MessageContent } from "./MessageContent";

type AttachmentContent = {
  name: string;
  kind: "text" | "image" | "binary";
  content: string;
  truncated: boolean;
  byteLen: number;
  imageBase64?: string;
  mediaType?: string;
};

const MAX_ERROR_DETAIL_LENGTH = 300;

function extOf(p: string): string {
  const name = p.split(/[\\/]/).pop() ?? "";
  const dot = name.lastIndexOf(".");
  return dot > 0 && dot < name.length - 1
    ? name.slice(dot + 1).toLowerCase()
    : "";
}

function langHint(ext: string): string {
  const languages: Record<string, string> = {
    ts: "typescript",
    tsx: "tsx",
    js: "javascript",
    jsx: "jsx",
    py: "python",
    rs: "rust",
    json: "json",
    sh: "bash",
    bash: "bash",
    yml: "yaml",
    yaml: "yaml",
    html: "html",
    css: "css",
    toml: "toml",
    md: "markdown",
    markdown: "markdown",
  };
  return languages[ext] ?? "";
}

export function PreviewPanel({
  path,
  sessionId,
}: {
  path: string | null;
  sessionId?: string | null;
}) {
  const { t } = useI18n();
  const [data, setData] = useState<AttachmentContent | null>(null);
  const [err, setErr] = useState<{ raw: unknown } | null>(null);
  const [loading, setLoading] = useState(false);

  useEffect(() => {
    if (path == null) {
      setData(null);
      setErr(null);
      setLoading(false);
      return;
    }

    let cancelled = false;
    setLoading(true);
    setErr(null);
    void invoke<AttachmentContent>("read_attachment", {
      path,
      sessionId: sessionId ?? null,
    })
      .then((res) => {
        if (cancelled) return;
        setData(res);
        setErr(null);
      })
      .catch((error: unknown) => {
        if (cancelled) return;
        setErr({ raw: error });
        setData(null);
      })
      .finally(() => {
        if (cancelled) return;
        setLoading(false);
      });

    return () => {
      cancelled = true;
    };
  }, [path, sessionId]);

  if (path == null) {
    return (
      <div className="preview-panel">
        <div className="preview-panel__empty">{t("preview.empty")}</div>
      </div>
    );
  }

  if (loading) {
    return (
      <div className="preview-panel">
        <div className="preview-panel__empty">{t("preview.loading")}</div>
      </div>
    );
  }

  if (err) {
    const rawDetail =
      err.raw == null || String(err.raw).trim() === "" ? null : err.raw;
    const detail =
      rawDetail !== null
        ? renderBackendError(rawDetail, t).slice(0, MAX_ERROR_DETAIL_LENGTH)
        : null;

    return (
      <div className="preview-panel">
        <div className="preview-panel__empty">
          <div>
            {t("preview.error")}: {path}
          </div>
          {detail ? (
            <div className="preview-panel__error-detail">{detail}</div>
          ) : null}
        </div>
      </div>
    );
  }

  const ext = extOf(path);

  return (
    <div className="preview-panel">
      {data ? (
        <>
          <div className="preview-panel__head">
            {data.name}
            {data.truncated ? ` · ${t("preview.truncated")}` : ""}
          </div>
          {data.kind === "image" ? (
            data.imageBase64 && data.mediaType ? (
              <img
                className="preview-panel__svg"
                src={`data:${data.mediaType};base64,${data.imageBase64}`}
                alt={data.name}
              />
            ) : (
              <div className="preview-panel__empty">
                {t("preview.imageUnavailable")}
              </div>
            )
          ) : data.kind === "binary" ? (
            <div className="preview-panel__empty">{t("preview.binary")}</div>
          ) : ext === "md" || ext === "markdown" ? (
            <MessageContent
              blocks={[{ type: "text", text: data.content } as Block]}
              streaming={false}
            />
          ) : ext === "svg" ? (
            <img
              className="preview-panel__svg"
              src={`data:image/svg+xml;utf8,${encodeURIComponent(data.content)}`}
              alt={data.name}
            />
          ) : (
            <CodeBlock code={data.content} lang={langHint(ext)} />
          )}
        </>
      ) : null}
    </div>
  );
}

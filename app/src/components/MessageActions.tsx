import { invoke } from "@tauri-apps/api/core";
import { save } from "@tauri-apps/plugin-dialog";
import { useState } from "react";
import { messageToMarkdown } from "../lib/messageMarkdown";
import type { ChatMessage } from "../types/agent";
import { useI18n } from "../i18n";

export function MessageActions({
  message,
  canQuote = false,
  onQuote,
}: {
  message: ChatMessage;
  canQuote?: boolean;
  onQuote?: () => void;
}) {
  const { t } = useI18n();
  const [copied, setCopied] = useState(false);

  function copy() {
    navigator.clipboard?.writeText(messageToMarkdown(message, t));
    setCopied(true);
    window.setTimeout(() => setCopied(false), 1200);
  }

  async function exportMd() {
    const content = messageToMarkdown(message, t);
    const path = await save({
      defaultPath: "message.md",
      filters: [{ name: "Markdown", extensions: ["md"] }],
    });
    if (!path) return;

    try {
      await invoke("write_text_file", { path, content });
    } catch (error) {
      console.warn("导出失败", error);
    }
  }

  return (
    <div className="turn__actions">
      <button
        type="button"
        className="turn__act"
        aria-label={
          copied ? t("messageActions.copied") : t("messageActions.copy")
        }
        title={copied ? t("messageActions.copied") : t("messageActions.copy")}
        onClick={copy}
      >
        {copied ? (
          <svg
            viewBox="0 0 24 24"
            fill="none"
            stroke="currentColor"
            strokeWidth="2"
            strokeLinecap="round"
            strokeLinejoin="round"
            aria-hidden="true"
          >
            <polyline points="20 6 9 17 4 12" />
          </svg>
        ) : (
          <svg
            viewBox="0 0 24 24"
            fill="none"
            stroke="currentColor"
            strokeWidth="2"
            strokeLinecap="round"
            strokeLinejoin="round"
            aria-hidden="true"
          >
            <rect x="9" y="9" width="13" height="13" rx="2" ry="2" />
            <path d="M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1" />
          </svg>
        )}
      </button>
      <button
        type="button"
        className="turn__act"
        aria-label={t("messageActions.exportMarkdown")}
        title={t("messageActions.exportMarkdown")}
        onClick={() => void exportMd()}
      >
        <svg
          viewBox="0 0 24 24"
          fill="none"
          stroke="currentColor"
          strokeWidth="2"
          strokeLinecap="round"
          strokeLinejoin="round"
          aria-hidden="true"
        >
          <path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4" />
          <polyline points="7 10 12 15 17 10" />
          <line x1="12" y1="15" x2="12" y2="3" />
        </svg>
      </button>
      {canQuote && (
        <button
          type="button"
          className="turn__act"
          aria-label={t("messageActions.quote")}
          title={t("messageActions.quote")}
          onClick={onQuote}
        >
          <svg
            viewBox="0 0 24 24"
            fill="none"
            stroke="currentColor"
            strokeWidth="2"
            strokeLinecap="round"
            strokeLinejoin="round"
            aria-hidden="true"
          >
            <path d="M7 8h4v4l-2 4H7l2-4H7zM15 8h4v4l-2 4h-2l2-4h-2z" />
          </svg>
        </button>
      )}
    </div>
  );
}

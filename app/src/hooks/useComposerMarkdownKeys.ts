import { useCallback, type KeyboardEvent } from "react";
import { insertText } from "../lib/insertText";
import {
  autoCloseFence,
  continueList,
  indentListLine,
  toggleWrap,
  type Edit,
} from "../lib/markdownEdit";

function isMac(): boolean {
  return /mac/i.test(navigator.platform);
}

function editFor(e: KeyboardEvent<HTMLTextAreaElement>): Edit | null {
  const { value, selectionStart: s, selectionEnd: end } = e.currentTarget;
  const key = e.key.toLowerCase();
  // Ctrl+B / Ctrl+E are native cursor moves on macOS, so only Cmd counts there.
  const mod = e.metaKey || (e.ctrlKey && !isMac());
  if (mod && !e.shiftKey && (key === "b" || key === "e")) {
    return toggleWrap(value, s, end, key === "b" ? "**" : "`");
  }
  if (e.metaKey || e.ctrlKey) return null;
  // Enter sends; Shift+Enter is the line break, so list and fence help hooks in there.
  if (key === "enter" && e.shiftKey) {
    return autoCloseFence(value, s, end) ?? continueList(value, s, end);
  }
  if (key === "tab") return indentListLine(value, s, end, e.shiftKey ? -1 : 1);
  return null;
}

/**
 * Keyboard helpers that make the plain composer textarea friendlier for
 * Markdown. Returns true (after preventDefault) when the key was handled;
 * false means the caller should carry on with its normal handling.
 */
export function useComposerMarkdownKeys() {
  return useCallback(
    (e: KeyboardEvent<HTMLTextAreaElement>, composing: boolean): boolean => {
      if (composing || e.altKey) return false;
      const edit = editFor(e);
      if (!edit) return false;
      e.preventDefault();
      insertText(e.currentTarget, edit);
      return true;
    },
    [],
  );
}

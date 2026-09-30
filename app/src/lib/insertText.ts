import type { Edit } from "./markdownEdit";

function tryExecCommand(edit: Edit): boolean {
  if (typeof document.execCommand !== "function") return false;
  try {
    return edit.insert === ""
      ? document.execCommand("delete")
      : document.execCommand("insertText", false, edit.insert);
  } catch {
    return false;
  }
}

/**
 * Apply one replacement to a textarea without losing native undo.
 * Assigning textarea.value wipes the undo stack, so the primary path selects
 * the range and runs execCommand("insertText"), which keeps Cmd+Z working and
 * fires the input event itself. Some engines report success yet leave a
 * different result (smart-delete, auto-correct), so the outcome is compared
 * with the expected text and repaired through setRangeText, which is also the
 * fallback when execCommand is missing (jsdom). Whether setRangeText keeps
 * undo differs between engines, so nothing may rely on it.
 */
export function insertText(el: HTMLTextAreaElement, edit: Edit): void {
  if (edit.from === edit.to && edit.insert === "") return;
  const before = el.value;
  const expected =
    before.slice(0, edit.from) + edit.insert + before.slice(edit.to);
  el.focus();
  el.setSelectionRange(edit.from, edit.to);
  if (!tryExecCommand(edit) || el.value !== expected) {
    if (el.value !== before) el.value = before;
    el.setRangeText(edit.insert, edit.from, edit.to, "end");
    el.dispatchEvent(new Event("input", { bubbles: true }));
  }
  el.setSelectionRange(edit.selStart, edit.selEnd);
}

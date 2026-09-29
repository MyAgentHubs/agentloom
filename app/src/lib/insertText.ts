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
 * fires the input event itself. setRangeText is only a fallback (jsdom or an
 * engine without execCommand); whether it keeps undo differs between engines,
 * so nothing may rely on it.
 */
export function insertText(el: HTMLTextAreaElement, edit: Edit): void {
  if (edit.from === edit.to && edit.insert === "") return;
  el.focus();
  el.setSelectionRange(edit.from, edit.to);
  if (!tryExecCommand(edit)) {
    el.setRangeText(edit.insert, edit.from, edit.to, "end");
    el.dispatchEvent(new Event("input", { bubbles: true }));
  }
  el.setSelectionRange(edit.selStart, edit.selEnd);
}

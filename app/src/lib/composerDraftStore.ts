const PREFIX = "agentloom.draft.";
const NEW_SESSION_KEY = "__new__";

export type ComposerAttachment = { path: string; name: string };
export type ComposerDraft = { text: string; attachments: ComposerAttachment[] };

const EMPTY: ComposerDraft = { text: "", attachments: [] };

function storageKey(sessionId: string | null): string {
  return PREFIX + (sessionId ?? NEW_SESSION_KEY);
}

/** Read the stored draft for a session (null = the new-session page). Corrupt or foreign data reads as no draft. */
export function loadDraft(sessionId: string | null): ComposerDraft {
  try {
    const raw = localStorage.getItem(storageKey(sessionId));
    if (!raw) return { ...EMPTY };
    const data: unknown = JSON.parse(raw);
    if (typeof data !== "object" || data === null) return { ...EMPTY };
    const rec = data as Record<string, unknown>;
    if (rec.v !== 1 || typeof rec.text !== "string") return { ...EMPTY };
    const attachments = Array.isArray(rec.attachments)
      ? rec.attachments.filter(
          (a): a is ComposerAttachment =>
            typeof a === "object" &&
            a !== null &&
            typeof (a as ComposerAttachment).path === "string" &&
            typeof (a as ComposerAttachment).name === "string",
        )
      : [];
    return {
      text: rec.text,
      attachments: attachments.map(({ path, name }) => ({ path, name })),
    };
  } catch {
    return { ...EMPTY };
  }
}

/** Persist a draft; an empty draft removes the key instead of storing an empty value. */
export function saveDraft(
  sessionId: string | null,
  draft: ComposerDraft,
): void {
  try {
    if (draft.text === "" && draft.attachments.length === 0) {
      localStorage.removeItem(storageKey(sessionId));
      return;
    }
    localStorage.setItem(
      storageKey(sessionId),
      JSON.stringify({
        v: 1,
        text: draft.text,
        attachments: draft.attachments,
      }),
    );
  } catch {
    // storage unavailable or full — drafts are best-effort
  }
}

export function clearDraft(sessionId: string | null): void {
  try {
    localStorage.removeItem(storageKey(sessionId));
  } catch {
    // storage unavailable — nothing to clear
  }
}

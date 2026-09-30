const PREFIX = "agentloom.draft.";
const NEW_SESSION_KEY = "__new__";

export type ComposerAttachment = { path: string; name: string };
export type ComposerDraft = { text: string; attachments: ComposerAttachment[] };

// Sessions deleted this run: late flushes must not resurrect their draft (ids are never reused).
const discarded = new Set<string>();

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
  if (sessionId !== null && discarded.has(sessionId)) return;
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

/** Session deleted: drop its draft and refuse any later write for it. */
export function discardDraft(sessionId: string): void {
  discarded.add(sessionId);
  clearDraft(sessionId);
}

/**
 * Drop drafts whose session no longer exists (deleted through paths that bypass discardDraft).
 * Keys are collected before deleting so removal cannot shift the iteration; the new-session draft stays.
 * Returns the number of removed drafts.
 */
export function pruneDrafts(liveIds: ReadonlySet<string>): number {
  let removed = 0;
  try {
    const orphans: string[] = [];
    for (let i = 0; i < localStorage.length; i++) {
      const key = localStorage.key(i);
      if (key === null || !key.startsWith(PREFIX)) continue;
      const id = key.slice(PREFIX.length);
      if (id !== NEW_SESSION_KEY && !liveIds.has(id)) orphans.push(key);
    }
    for (const key of orphans) {
      localStorage.removeItem(key);
      removed++;
    }
  } catch {
    // storage unavailable — leftover drafts are harmless
  }
  return removed;
}

/** Pass-through for a successfully loaded full session list (archived included): prune, then hand the list back. */
export function pruneDraftsForSessions<T extends { id: string }>(
  sessions: T[],
): T[] {
  pruneDrafts(new Set(sessions.map((s) => s.id)));
  return sessions;
}

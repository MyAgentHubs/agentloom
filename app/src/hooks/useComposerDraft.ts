import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
} from "react";
import {
  clearDraft,
  loadDraft,
  saveDraft,
  type ComposerAttachment,
  type ComposerDraft,
} from "../lib/composerDraftStore";

const SAVE_DEBOUNCE_MS = 300;

type Updater<T> = T | ((prev: T) => T);

/**
 * Per-session composer draft with debounced autosave. Every write targets the
 * key the content was typed under (keyRef), so switching sessions can never
 * leak one session's text into another.
 */
export function useComposerDraft(sessionId: string | null) {
  const [state, setState] = useState<ComposerDraft>(() => loadDraft(sessionId));
  const keyRef = useRef<string | null>(sessionId);
  const latestRef = useRef<ComposerDraft>(state);
  // Bumps whenever a stored draft is loaded for a new session, so callers can re-measure the textarea.
  const [loadSeq, setLoadSeq] = useState(0);
  const timerRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  const cancelTimer = useCallback(() => {
    if (timerRef.current !== null) {
      clearTimeout(timerRef.current);
      timerRef.current = null;
    }
  }, []);

  const flush = useCallback(() => {
    cancelTimer();
    saveDraft(keyRef.current, latestRef.current);
  }, [cancelTimer]);

  const update = useCallback(
    (next: ComposerDraft) => {
      latestRef.current = next;
      setState(next);
      cancelTimer();
      timerRef.current = setTimeout(flush, SAVE_DEBOUNCE_MS);
    },
    [cancelTimer, flush],
  );

  const setText = useCallback(
    (v: Updater<string>) => {
      const cur = latestRef.current;
      update({ ...cur, text: typeof v === "function" ? v(cur.text) : v });
    },
    [update],
  );

  const setAttachments = useCallback(
    (v: Updater<ComposerAttachment[]>) => {
      const cur = latestRef.current;
      update({
        ...cur,
        attachments: typeof v === "function" ? v(cur.attachments) : v,
      });
    },
    [update],
  );

  /**
   * Apply an edit to the draft of the session an async job started in. `live`
   * tells the callback whether that session is still the one on screen.
   */
  const editFor = useCallback(
    (
      key: string | null,
      fn: (d: ComposerDraft, live: boolean) => ComposerDraft,
    ) => {
      if (key === keyRef.current) return update(fn(latestRef.current, true));
      saveDraft(key, fn(loadDraft(key), false));
    },
    [update],
  );

  /**
   * Empty the composer and drop the stored draft immediately (after a send).
   * Pass the session the send started in: if the user has since switched away,
   * only that session's stored draft is dropped and the live composer is kept
   * (returns false in that case).
   */
  const clear = useCallback(
    (key?: string | null) => {
      if (key !== undefined && key !== keyRef.current) {
        clearDraft(key);
        return false;
      }
      cancelTimer();
      latestRef.current = { text: "", attachments: [] };
      setState(latestRef.current);
      clearDraft(keyRef.current);
      return true;
    },
    [cancelTimer],
  );

  // Session switch: persist the old content under the old key first, then load the new one.
  useLayoutEffect(() => {
    if (keyRef.current === sessionId) return;
    flush();
    keyRef.current = sessionId;
    latestRef.current = loadDraft(sessionId);
    setState(latestRef.current);
    setLoadSeq((n) => n + 1);
  }, [sessionId, flush]);

  // Flush on unmount and when the page is going away.
  useEffect(() => {
    window.addEventListener("pagehide", flush);
    window.addEventListener("beforeunload", flush);
    return () => {
      window.removeEventListener("pagehide", flush);
      window.removeEventListener("beforeunload", flush);
      flush();
    };
  }, [flush]);

  return {
    draft: state.text,
    attachments: state.attachments,
    setDraft: setText,
    setAttachments,
    clear,
    editFor,
    loadSeq,
  };
}

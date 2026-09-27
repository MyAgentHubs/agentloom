import { useEffect, useRef, type RefObject } from "react";
import { invoke } from "@tauri-apps/api/core";
import { sealStreamTail } from "../lib/streamBlocks";
import type { ChatMessage } from "../types/agent";

const POLL_INTERVAL_MS = 15000;
const MIN_RUN_AGE_MS = 10000;

type SessionRunState = { status: string; updatedAt: number } | null;

export function useRunStateReconcile(params: {
  runningSessionsRef: RefObject<Map<string, { startedAt: number }>>;
  hasRunningSessions: boolean;
  setRun: (sid: string, info: null) => void;
  mutate: (sid: string, fn: (msgs: ChatMessage[]) => ChatMessage[]) => void;
  setSessionDotStatus: (sid: string, status: "done") => void;
}) {
  const paramsRef = useRef(params);
  paramsRef.current = params;

  useEffect(() => {
    const reconcileOne = (sid: string, startedAt: number) => {
      void invoke<SessionRunState>("get_session_run_state", { sessionId: sid })
        .then((state) => {
          const { runningSessionsRef, setRun, mutate, setSessionDotStatus } =
            paramsRef.current;
          const current = runningSessionsRef.current?.get(sid);
          if (!current || current.startedAt !== startedAt) return;
          if (
            state &&
            state.status === "idle" &&
            state.updatedAt * 1000 >= startedAt
          ) {
            mutate(sid, (msgs) => sealStreamTail(msgs));
            setRun(sid, null);
            setSessionDotStatus(sid, "done");
          }
        })
        .catch(() => {});
    };

    const reconcileAll = () => {
      const running = paramsRef.current.runningSessionsRef.current;
      if (!running) return;
      const now = Date.now();
      for (const [sid, run] of running) {
        if (now - run.startedAt < MIN_RUN_AGE_MS) continue;
        reconcileOne(sid, run.startedAt);
      }
    };

    let timer: number | undefined;
    if (params.hasRunningSessions) {
      timer = window.setInterval(reconcileAll, POLL_INTERVAL_MS);
    }
    const onFocus = () => reconcileAll();
    const onVisibility = () => {
      if (document.visibilityState === "visible") reconcileAll();
    };
    window.addEventListener("focus", onFocus);
    document.addEventListener("visibilitychange", onVisibility);
    return () => {
      if (timer !== undefined) window.clearInterval(timer);
      window.removeEventListener("focus", onFocus);
      document.removeEventListener("visibilitychange", onVisibility);
    };
  }, [params.hasRunningSessions]);
}

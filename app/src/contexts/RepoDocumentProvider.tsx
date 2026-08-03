import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import {
  createContext,
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  type ReactNode,
} from "react";
import type {
  GeneratedDocumentView,
  GenerationEvent,
  GenerationRun,
} from "../types/repoDocument";

export type RepoDocumentKind = "intro" | "daily";

export const COMMANDS = {
  intro: {
    feature: "project_intro",
    get: "get_project_intro",
    generate: "generate_project_intro",
  },
  daily: {
    feature: "daily",
    get: "get_daily",
    generate: "generate_daily",
  },
} as const;

export type RepoDocumentEntry = {
  doc: GeneratedDocumentView | null;
  loading: boolean;
  generating: boolean;
  liveText: string;
  error: string | null;
  runId: string | null;
};

type RepoDocumentContextValue = {
  entries: Record<string, RepoDocumentEntry>;
  ensureLoaded: (repoId: string, kind: RepoDocumentKind) => void;
  generate: (repoId: string, kind: RepoDocumentKind, agentId: string) => void;
};

const EMPTY_ENTRY: RepoDocumentEntry = {
  doc: null,
  loading: false,
  generating: false,
  liveText: "",
  error: null,
  runId: null,
};

export const RepoDocumentContext =
  createContext<RepoDocumentContextValue | null>(null);

export function repoDocumentKey(repoId: string, kind: RepoDocumentKind) {
  return `${COMMANDS[kind].feature}:${repoId}`;
}

function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

export function RepoDocumentProvider({ children }: { children: ReactNode }) {
  const [entries, setEntries] = useState<Record<string, RepoDocumentEntry>>({});
  const entriesRef = useRef(entries);
  const pendingEventsRef = useRef<Record<string, GenerationEvent[]>>({});
  const generationVersionsRef = useRef<Record<string, number>>({});
  const listenerReadyRef = useRef<Promise<void>>(Promise.resolve());

  const updateEntry = useCallback(
    (
      key: string,
      update: (current: RepoDocumentEntry) => RepoDocumentEntry,
    ) => {
      const next = update(entriesRef.current[key] ?? EMPTY_ENTRY);
      entriesRef.current = { ...entriesRef.current, [key]: next };
      setEntries(entriesRef.current);
    },
    [],
  );

  const applyEvent = useCallback(
    (key: string, event: GenerationEvent) => {
      const entry = entriesRef.current[key];
      if (!entry?.generating) return;
      if (entry.runId === null) {
        (pendingEventsRef.current[key] ??= []).push(event);
        return;
      }
      if (event.run_id !== entry.runId) return;

      if (event.phase === "delta" && event.delta !== undefined) {
        updateEntry(key, (current) => ({
          ...current,
          liveText: current.liveText + event.delta,
        }));
      } else if (event.phase === "completed" && event.document) {
        updateEntry(key, (current) => ({
          ...current,
          doc: { ...event.document!, stale: false },
          generating: false,
          liveText: "",
        }));
        delete pendingEventsRef.current[key];
      } else if (event.phase === "error") {
        updateEntry(key, (current) => ({
          ...current,
          error: event.message ?? "Generation failed",
          generating: false,
        }));
        delete pendingEventsRef.current[key];
      }
    },
    [updateEntry],
  );

  useEffect(() => {
    let disposed = false;
    let unlisten: (() => void) | null = null;
    listenerReadyRef.current = listen<GenerationEvent>(
      "agent://event",
      ({ payload }) => {
        applyEvent(`${payload.feature}:${payload.repo_id}`, payload);
      },
    ).then((stop) => {
      if (disposed) stop();
      else unlisten = stop;
    });
    return () => {
      disposed = true;
      unlisten?.();
    };
  }, [applyEvent]);

  const ensureLoaded = useCallback(
    (repoId: string, kind: RepoDocumentKind) => {
      const key = repoDocumentKey(repoId, kind);
      if (entriesRef.current[key]) return;
      updateEntry(key, () => ({ ...EMPTY_ENTRY, loading: true }));
      void invoke<GeneratedDocumentView | null>(COMMANDS[kind].get, { repoId })
        .then((storedDoc) => {
          updateEntry(key, (current) =>
            current.generating ? current : { ...current, doc: storedDoc },
          );
        })
        .catch((loadError: unknown) => {
          updateEntry(key, (current) =>
            current.generating
              ? current
              : { ...current, error: errorMessage(loadError) },
          );
        })
        .finally(() => {
          updateEntry(key, (current) => ({ ...current, loading: false }));
        });
    },
    [updateEntry],
  );

  const generate = useCallback(
    (repoId: string, kind: RepoDocumentKind, agentId: string) => {
      const key = repoDocumentKey(repoId, kind);
      const version = (generationVersionsRef.current[key] ?? 0) + 1;
      generationVersionsRef.current[key] = version;
      pendingEventsRef.current[key] = [];
      updateEntry(key, (current) => ({
        ...current,
        generating: true,
        liveText: "",
        error: null,
        runId: null,
      }));
      void (async () => {
        try {
          await listenerReadyRef.current;
          const run = await invoke<GenerationRun>(COMMANDS[kind].generate, {
            repoId,
            agentId,
          });
          if (generationVersionsRef.current[key] !== version) return;
          const current = entriesRef.current[key];
          if (!current?.generating || current.runId !== null) return;
          updateEntry(key, (entry) => ({ ...entry, runId: run.run_id }));
          pendingEventsRef.current[key]
            ?.splice(0)
            .forEach((event) => applyEvent(key, event));
        } catch (generationError: unknown) {
          if (generationVersionsRef.current[key] !== version) return;
          const current = entriesRef.current[key];
          if (!current?.generating || current.runId !== null) return;
          updateEntry(key, (entry) => ({
            ...entry,
            error: errorMessage(generationError),
            generating: false,
          }));
          delete pendingEventsRef.current[key];
        }
      })();
    },
    [applyEvent, updateEntry],
  );

  const value = useMemo(
    () => ({ entries, ensureLoaded, generate }),
    [ensureLoaded, entries, generate],
  );
  return (
    <RepoDocumentContext.Provider value={value}>
      {children}
    </RepoDocumentContext.Provider>
  );
}

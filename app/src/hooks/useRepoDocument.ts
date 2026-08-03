import { useCallback, useContext, useEffect, useMemo } from "react";
import {
  RepoDocumentContext,
  repoDocumentKey,
  type RepoDocumentKind,
} from "../contexts/RepoDocumentProvider";
import type { GeneratedDocumentView } from "../types/repoDocument";

export type { RepoDocumentKind } from "../contexts/RepoDocumentProvider";

export type UseRepoDocumentResult = {
  doc: GeneratedDocumentView | null;
  loading: boolean;
  generating: boolean;
  liveText: string;
  error: string | null;
  generate: (agentId: string) => void;
};

const EMPTY_RESULT = {
  doc: null,
  loading: false,
  generating: false,
  liveText: "",
  error: null,
};

export function useRepoDocument(
  repoId: string | null,
  kind: RepoDocumentKind,
): UseRepoDocumentResult {
  const context = useContext(RepoDocumentContext);
  if (context === null) {
    throw new Error("useRepoDocument must be used within RepoDocumentProvider");
  }

  const { entries, ensureLoaded, generate: generateDocument } = context;
  useEffect(() => {
    if (repoId !== null) ensureLoaded(repoId, kind);
  }, [ensureLoaded, kind, repoId]);

  const entry =
    repoId === null
      ? EMPTY_RESULT
      : (entries[repoDocumentKey(repoId, kind)] ?? EMPTY_RESULT);
  const generate = useCallback(
    (agentId: string) => {
      if (repoId !== null) generateDocument(repoId, kind, agentId);
    },
    [generateDocument, kind, repoId],
  );

  return useMemo(
    () => ({
      doc: entry.doc,
      loading: entry.loading,
      generating: entry.generating,
      liveText: entry.liveText,
      error: entry.error,
      generate,
    }),
    [entry, generate],
  );
}

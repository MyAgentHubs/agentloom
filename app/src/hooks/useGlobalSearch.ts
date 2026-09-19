import { useCallback, useState } from "react";
import type { GlobalSearchResult } from "../components/GlobalSearch";
import type { TFn } from "../i18n";

export type GlobalSearchTarget = {
  sessionId: string;
  messageId: number;
};

export function useGlobalSearch(params: {
  currentId: string | null;
  loading: boolean;
  t: TFn;
  setToast: (message: string | null) => void;
  onSelectSession: (id: string) => void;
}) {
  const { currentId, loading, t, setToast, onSelectSession } = params;
  const [open, setOpen] = useState(false);
  const [target, setTarget] = useState<GlobalSearchTarget | null>(null);

  const openSearch = useCallback(() => setOpen(true), []);
  const closeSearch = useCallback(() => setOpen(false), []);
  const onSelect = useCallback(
    (result: GlobalSearchResult) => {
      setTarget(
        result.message_id == null
          ? null
          : { sessionId: result.session_id, messageId: result.message_id },
      );
      onSelectSession(result.session_id);
    },
    [onSelectSession],
  );
  const onTargetResolved = useCallback(
    (found: boolean) => {
      setTarget(null);
      if (!found) setToast(t("globalSearch.messageNotFound"));
    },
    [t, setToast],
  );

  const searchTargetMessageId =
    target?.sessionId === currentId && !loading ? target.messageId : null;

  return {
    open,
    openSearch,
    closeSearch,
    onSelect,
    onTargetResolved,
    searchTargetMessageId,
  };
}

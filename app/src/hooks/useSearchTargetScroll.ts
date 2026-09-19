import { useLayoutEffect, type RefObject } from "react";

export function useSearchTargetScroll({
  contentRef,
  searchTargetMessageId,
  searchTargetIndex,
  onSearchTargetResolved,
}: {
  contentRef: RefObject<HTMLElement | null>;
  searchTargetMessageId: number | null;
  searchTargetIndex: number;
  onSearchTargetResolved?: (found: boolean) => void;
}) {
  useLayoutEffect(() => {
    if (searchTargetMessageId == null) return;
    if (searchTargetIndex < 0) {
      onSearchTargetResolved?.(false);
      return;
    }
    const target = contentRef.current?.querySelector<HTMLElement>(
      `[data-message-id="${searchTargetMessageId}"]`,
    );
    if (!target) {
      onSearchTargetResolved?.(false);
      return;
    }
    target.scrollIntoView({ behavior: "smooth", block: "center" });
    target.classList.add("turn--search-target");
    window.setTimeout(
      () => target.classList.remove("turn--search-target"),
      1600,
    );
    onSearchTargetResolved?.(true);
  }, [onSearchTargetResolved, searchTargetIndex, searchTargetMessageId]);
}

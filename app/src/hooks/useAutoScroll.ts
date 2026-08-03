import { useEffect, useRef, type RefObject } from "react";

const BOTTOM_THRESHOLD_PX = 80;

function isAtBottom(target: HTMLElement): boolean {
  return (
    target.scrollHeight - target.scrollTop - target.clientHeight <
    BOTTOM_THRESHOLD_PX
  );
}

export function useAutoScroll(
  scrollRef: RefObject<HTMLElement | null>,
  contentKey: number,
  contentRef?: RefObject<HTMLElement | null>,
) {
  const stickRef = useRef(true);

  useEffect(() => {
    const target = scrollRef.current;
    if (!target) return;

    const updateStick = () => {
      stickRef.current = isAtBottom(target);
    };

    updateStick();
    target.addEventListener("scroll", updateStick, { passive: true });

    let resizeObserver: ResizeObserver | undefined;
    const content = contentRef?.current;
    if (content && typeof ResizeObserver !== "undefined") {
      resizeObserver = new ResizeObserver(() => {
        if (stickRef.current) {
          target.scrollTop = target.scrollHeight;
        }
      });
      resizeObserver.observe(content);
    }

    return () => {
      target.removeEventListener("scroll", updateStick);
      resizeObserver?.disconnect();
    };
  }, [scrollRef, contentRef]);

  useEffect(() => {
    const target = scrollRef.current;
    if (!target) return;
    const wasAtBottom = stickRef.current;
    if (wasAtBottom) {
      target.scrollTop = target.scrollHeight;
      stickRef.current = true;
    } else {
      stickRef.current = isAtBottom(target);
    }
  }, [scrollRef, contentKey]);

  function scrollToBottom() {
    const target = scrollRef.current;
    stickRef.current = true;
    if (target)
      target.scrollTo({ top: target.scrollHeight, behavior: "smooth" });
  }

  return { stickRef, scrollToBottom };
}

import { useEffect, useState, type RefObject } from "react";
import { useI18n } from "../i18n";

type Props = {
  scrollRef: RefObject<HTMLElement | null>;
  scrollToBottom: () => void;
};

export function ScrollButtons({ scrollRef, scrollToBottom }: Props) {
  const { t } = useI18n();
  const [showTop, setShowTop] = useState(false);
  const [showBottom, setShowBottom] = useState(false);

  useEffect(() => {
    const el = scrollRef.current;
    if (!el) return;

    const update = () => {
      setShowBottom(el.scrollHeight - el.scrollTop - el.clientHeight > 120);
      setShowTop(el.scrollTop > el.clientHeight);
    };

    update();
    el.addEventListener("scroll", update, { passive: true });
    return () => el.removeEventListener("scroll", update);
  }, [scrollRef]);

  function toTop() {
    scrollRef.current?.scrollTo({ top: 0, behavior: "smooth" });
  }

  return (
    <div className="scrollbtns">
      {showTop && (
        <button
          type="button"
          className="scrollbtn"
          aria-label={t("scrollButtons.top")}
          title={t("scrollButtons.top")}
          onClick={toTop}
        >
          <svg
            viewBox="0 0 24 24"
            fill="none"
            stroke="currentColor"
            strokeWidth="2"
            strokeLinecap="round"
            strokeLinejoin="round"
            aria-hidden="true"
          >
            <path d="M18 15l-6-6-6 6" />
          </svg>
        </button>
      )}
      {showBottom && (
        <button
          type="button"
          className="scrollbtn"
          aria-label={t("scrollButtons.bottom")}
          title={t("scrollButtons.bottom")}
          onClick={scrollToBottom}
        >
          <svg
            viewBox="0 0 24 24"
            fill="none"
            stroke="currentColor"
            strokeWidth="2"
            strokeLinecap="round"
            strokeLinejoin="round"
            aria-hidden="true"
          >
            <path d="M6 9l6 6 6-6" />
          </svg>
        </button>
      )}
    </div>
  );
}

import {
  type HTMLAttributes,
  type ReactNode,
  useEffect,
  useRef,
  useState,
} from "react";
import { createPortal } from "react-dom";
import { useI18n } from "../i18n";
import "../styles/pathContextMenu.css";

type TriggerProps = Pick<
  HTMLAttributes<HTMLElement>,
  "onContextMenu" | "onKeyDown"
>;

// Copying a reference never reads the file or grants permission to open it.
export function PathContextTarget({
  path,
  children,
}: {
  path: string;
  children: (props: TriggerProps) => ReactNode;
}) {
  const { t } = useI18n();
  const [position, setPosition] = useState<{ x: number; y: number } | null>(
    null,
  );
  const [feedback, setFeedback] = useState<string | null>(null);
  const menu = useRef<HTMLDivElement>(null);
  const trigger = useRef<HTMLElement | null>(null);

  const close = () => {
    setPosition(null);
    trigger.current?.focus();
  };

  useEffect(() => {
    if (!position) return;
    menu.current?.querySelector("button")?.focus();
    const outside = (event: Event) => {
      if (!menu.current?.contains(event.target as Node)) setPosition(null);
    };
    const keyboard = (event: KeyboardEvent) => {
      if (event.key === "Escape") close();
      if (event.key === "Tab") setPosition(null);
    };
    document.addEventListener("pointerdown", outside);
    document.addEventListener("contextmenu", outside, true);
    document.addEventListener("keydown", keyboard);
    return () => {
      document.removeEventListener("pointerdown", outside);
      document.removeEventListener("contextmenu", outside, true);
      document.removeEventListener("keydown", keyboard);
    };
  }, [position]);

  useEffect(() => {
    if (!feedback) return;
    const timer = window.setTimeout(() => setFeedback(null), 2000);
    return () => window.clearTimeout(timer);
  }, [feedback]);

  const open = (element: HTMLElement, x: number, y: number) => {
    trigger.current = element;
    setFeedback(null);
    setPosition({
      x: Math.max(8, Math.min(x, window.innerWidth - 188)),
      y: Math.max(8, Math.min(y, window.innerHeight - 52)),
    });
  };
  const triggerProps: TriggerProps = {
    onContextMenu(event) {
      event.preventDefault();
      event.stopPropagation();
      open(event.currentTarget, event.clientX, event.clientY);
    },
    onKeyDown(event) {
      if (
        event.key !== "ContextMenu" &&
        !(event.shiftKey && event.key === "F10")
      )
        return;
      event.preventDefault();
      event.stopPropagation();
      const rect = event.currentTarget.getBoundingClientRect();
      open(event.currentTarget, rect.left, rect.bottom);
    },
  };
  const copy = async () => {
    close();
    try {
      await navigator.clipboard.writeText(path);
      setFeedback(t("messageContent.imageMenu.pathCopied"));
    } catch {
      setFeedback(t("messageContent.imageMenu.copyFailed"));
    }
  };

  return (
    <>
      {children(triggerProps)}
      {position &&
        createPortal(
          <div
            ref={menu}
            className="path-context-menu"
            role="menu"
            aria-label={t("messageContent.pathMenu.label")}
            style={{ left: position.x, top: position.y }}
          >
            <button type="button" role="menuitem" onClick={() => void copy()}>
              {t("messageContent.pathMenu.copyPath")}
            </button>
          </div>,
          document.body,
        )}
      {feedback &&
        createPortal(
          <div className="toast" role="status">
            {feedback}
          </div>,
          document.body,
        )}
    </>
  );
}

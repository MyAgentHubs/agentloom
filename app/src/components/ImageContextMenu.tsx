import {
  Fragment,
  type KeyboardEvent as ReactKeyboardEvent,
  type MouseEvent as ReactMouseEvent,
  type ReactNode,
  useEffect,
  useRef,
  useState,
} from "react";
import { createPortal } from "react-dom";
import { useI18n } from "../i18n";
import { isSvgDataUri } from "../lib/imageClipboard";
import {
  canCopyImageInEnv,
  copyImageToClipboard,
} from "../lib/imageClipboardTauri";

type ImageContextTriggerProps = {
  onContextMenu: (event: ReactMouseEvent<HTMLElement>) => void;
  onKeyDown: (event: ReactKeyboardEvent<HTMLElement>) => void;
};

const IMAGE_MENU_WIDTH = 180;
const IMAGE_MENU_HEIGHT = 96;
const IMAGE_MENU_GAP = 8;

export function computeImageMenuPosition(
  rect: Pick<DOMRect, "left" | "right" | "top" | "bottom">,
  cursor: { x: number; y: number } | undefined,
  viewport: { width: number; height: number },
): { left: number; top: number } {
  const clampLeft = (left: number) =>
    Math.max(
      IMAGE_MENU_GAP,
      Math.min(left, viewport.width - IMAGE_MENU_WIDTH - IMAGE_MENU_GAP),
    );
  const clampTop = (top: number) =>
    Math.max(
      IMAGE_MENU_GAP,
      Math.min(top, viewport.height - IMAGE_MENU_HEIGHT - IMAGE_MENU_GAP),
    );
  const right = rect.right + IMAGE_MENU_GAP;
  const left = rect.left - IMAGE_MENU_WIDTH - IMAGE_MENU_GAP;

  if (
    !cursor &&
    right + IMAGE_MENU_WIDTH > viewport.width - IMAGE_MENU_GAP &&
    left < IMAGE_MENU_GAP
  ) {
    const below = rect.bottom + IMAGE_MENU_GAP;
    const above = rect.top - IMAGE_MENU_HEIGHT - IMAGE_MENU_GAP;
    const verticalAnchor =
      below + IMAGE_MENU_HEIGHT <= viewport.height - IMAGE_MENU_GAP
        ? below
        : above >= IMAGE_MENU_GAP
          ? above
          : rect.top + 12;

    return {
      left: clampLeft(rect.left),
      top: clampTop(verticalAnchor),
    };
  }

  const horizontalAnchor =
    right + IMAGE_MENU_WIDTH <= viewport.width - IMAGE_MENU_GAP
      ? right
      : left >= IMAGE_MENU_GAP
        ? left
        : (cursor?.x ?? rect.left + 12);

  return {
    left: clampLeft(horizontalAnchor),
    top: clampTop(cursor?.y ?? rect.top + 12),
  };
}

function useImageContextMenu(path: string, dataUri: string) {
  const { t } = useI18n();
  const menuRef = useRef<HTMLDivElement>(null);
  const [menuPosition, setMenuPosition] = useState<{
    left: number;
    top: number;
  } | null>(null);
  const [feedback, setFeedback] = useState<string | null>(null);
  const clipboard =
    typeof navigator === "undefined" ? undefined : navigator.clipboard;
  const canCopyImage = canCopyImageInEnv(clipboard);

  useEffect(() => {
    if (!menuPosition) return;

    menuRef.current
      ?.querySelector<HTMLButtonElement>("button:not(:disabled)")
      ?.focus();
    const closeOnOutsidePress = (event: PointerEvent) => {
      if (!menuRef.current?.contains(event.target as Node)) {
        setMenuPosition(null);
      }
    };
    const closeOnOutsideContextMenu = (event: MouseEvent) => {
      if (!menuRef.current?.contains(event.target as Node)) {
        setMenuPosition(null);
      }
    };
    const closeOnEscape = (event: KeyboardEvent) => {
      if (event.key === "Escape") setMenuPosition(null);
    };
    document.addEventListener("pointerdown", closeOnOutsidePress);
    document.addEventListener("contextmenu", closeOnOutsideContextMenu, true);
    document.addEventListener("keydown", closeOnEscape);
    return () => {
      document.removeEventListener("pointerdown", closeOnOutsidePress);
      document.removeEventListener(
        "contextmenu",
        closeOnOutsideContextMenu,
        true,
      );
      document.removeEventListener("keydown", closeOnEscape);
    };
  }, [menuPosition]);

  useEffect(() => {
    if (!feedback) return;
    const timeout = window.setTimeout(() => setFeedback(null), 2000);
    return () => window.clearTimeout(timeout);
  }, [feedback]);

  const openMenu = (
    rect: Pick<DOMRect, "left" | "right" | "top" | "bottom">,
    cursor?: { x: number; y: number },
  ) => {
    setFeedback(null);
    setMenuPosition(
      computeImageMenuPosition(rect, cursor, {
        width: window.innerWidth,
        height: window.innerHeight,
      }),
    );
  };
  const triggerProps: ImageContextTriggerProps = {
    onContextMenu: (event) => {
      event.preventDefault();
      event.stopPropagation();
      openMenu(event.currentTarget.getBoundingClientRect(), {
        x: event.clientX,
        y: event.clientY,
      });
    },
    onKeyDown: (event) => {
      if (
        event.key !== "ContextMenu" &&
        !(event.shiftKey && event.key === "F10")
      )
        return;
      event.preventDefault();
      const rect = event.currentTarget.getBoundingClientRect();
      openMenu(rect);
    },
  };

  const copyPath = async () => {
    setMenuPosition(null);
    try {
      if (typeof clipboard?.writeText !== "function") {
        throw new Error("Clipboard text API unavailable");
      }
      await clipboard.writeText(path);
      setFeedback(t("messageContent.imageMenu.pathCopied"));
    } catch {
      setFeedback(t("messageContent.imageMenu.copyFailed"));
    }
  };

  const copyImage = async () => {
    setMenuPosition(null);
    try {
      await copyImageToClipboard(dataUri, clipboard, canCopyImage);
      setFeedback(t("messageContent.imageMenu.imageCopied"));
    } catch (e) {
      console.warn("copyImage failed:", e instanceof Error ? e.name : e);
      // Fall back to copying the path if SVG rasterization or clipboard writing
      // fails, so the user still has a usable reference.
      if (isSvgDataUri(dataUri) && typeof clipboard?.writeText === "function") {
        try {
          await clipboard.writeText(path);
          setFeedback(t("messageContent.imageMenu.svgCopyFallback"));
          return;
        } catch {
          // If the fallback fails too, show the generic failure message below.
        }
      }
      setFeedback(t("messageContent.imageMenu.copyFailed"));
    }
  };

  return {
    menuRef,
    menuPosition,
    feedback,
    triggerProps,
    canCopyImage,
    copyImage,
    copyPath,
    t,
  };
}

function ImageContextMenuPopup({
  children,
  menu,
}: {
  children: (props: ImageContextTriggerProps) => ReactNode;
  menu: ReturnType<typeof useImageContextMenu>;
}) {
  const {
    menuRef,
    menuPosition,
    feedback,
    triggerProps,
    canCopyImage,
    copyImage,
    copyPath,
    t,
  } = menu;

  const menuItemStyle = {
    width: "100%",
    padding: "7px 10px",
    border: 0,
    borderRadius: 5,
    background: "transparent",
    color: "var(--ink-2)",
    cursor: "pointer",
    font: "inherit",
    fontSize: 12,
    textAlign: "left" as const,
  };

  return (
    <Fragment>
      {children(triggerProps)}
      {menuPosition &&
        createPortal(
          <div
            ref={menuRef}
            role="menu"
            aria-label={t("messageContent.imageMenu.label")}
            style={{
              position: "fixed",
              left: menuPosition.left,
              top: menuPosition.top,
              zIndex: 1000,
              width: IMAGE_MENU_WIDTH,
              boxSizing: "border-box",
              padding: 4,
              border: "1px solid var(--line)",
              borderRadius: 7,
              background: "var(--panel)",
              boxShadow: "0 8px 24px rgba(72, 54, 35, 0.16)",
            }}
          >
            <button
              type="button"
              role="menuitem"
              disabled={!canCopyImage}
              title={
                canCopyImage
                  ? undefined
                  : t("messageContent.imageMenu.imageUnavailable")
              }
              onClick={() => void copyImage()}
              style={{
                ...menuItemStyle,
                cursor: canCopyImage ? "pointer" : "not-allowed",
                opacity: canCopyImage ? 1 : 0.5,
              }}
            >
              {t("messageContent.imageMenu.copyImage")}
            </button>
            <button
              type="button"
              role="menuitem"
              onClick={() => void copyPath()}
              style={menuItemStyle}
            >
              {t("messageContent.imageMenu.copyPath")}
            </button>
          </div>,
          document.body,
        )}
      {feedback &&
        createPortal(
          <span
            role="status"
            style={{
              position: "fixed",
              right: 18,
              bottom: 18,
              zIndex: 1000,
              padding: "6px 10px",
              border: "1px solid var(--line)",
              borderRadius: 7,
              background: "var(--panel)",
              color: "var(--ink-2)",
              boxShadow: "0 6px 18px rgba(72, 54, 35, 0.14)",
              fontSize: 12,
            }}
          >
            {feedback}
          </span>,
          document.body,
        )}
    </Fragment>
  );
}

export function ImageContextTarget({
  path,
  dataUri,
  children,
}: {
  path: string;
  dataUri: string;
  children: (props: ImageContextTriggerProps) => ReactNode;
}) {
  const menu = useImageContextMenu(path, dataUri);
  return <ImageContextMenuPopup children={children} menu={menu} />;
}

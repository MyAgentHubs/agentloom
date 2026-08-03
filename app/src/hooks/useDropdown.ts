import { useCallback, useEffect, useRef, useState } from "react";

export function useDropdown() {
  const [open, setOpen] = useState(false);
  const containerRef = useRef<HTMLDivElement>(null);
  const triggerRef = useRef<HTMLButtonElement>(null);

  const close = useCallback(() => {
    setOpen(false);
    triggerRef.current?.focus();
  }, []);

  useEffect(() => {
    if (!open) return;

    function onDocMouseDown(e: MouseEvent) {
      if (
        containerRef.current &&
        !containerRef.current.contains(e.target as Node)
      ) {
        setOpen(false);
      }
    }

    function onKey(e: KeyboardEvent) {
      if (e.key === "Escape") close();
    }

    // capture 阶段：保证外部点击一定触发（不被任何 bubble 阶段 stopPropagation 拦掉 ·
    // WKWebView 下 bubble 阶段 document 监听偶发收不到 = 「点别处菜单不收起」）。
    document.addEventListener("mousedown", onDocMouseDown, true);
    document.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("mousedown", onDocMouseDown, true);
      document.removeEventListener("keydown", onKey);
    };
  }, [open, close]);

  return {
    open,
    setOpen,
    close,
    toggle: () => setOpen((v) => !v),
    containerRef,
    triggerProps: {
      ref: triggerRef,
      "aria-haspopup": true as const,
      "aria-expanded": open,
    },
  };
}

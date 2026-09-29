// useStickToBottom.ts — T6f2 · 滚动行为：新内容自动贴底、用户上滑时不抢滚（任务书 §3）。
//
// 机制：滚动容器上挂一个 `scroll` 监听，持续记录"当前是否贴着底部"（`stickingRef`，用 ref 不用
// state——这个值只用来决定下一次 effect 要不要滚，本身不需要触发重渲染）；每当 `trigger`（消息数/
// live 内容变化）变了，只有仍然"贴底"时才把 `scrollTop` 拉到底——用户主动上滑查看历史时
// `stickingRef` 变 false，新消息到达也不会把视口抢走。

import { useEffect, useRef } from "react";

/** 距底部多近算"贴底"——留一点容差，避免因为子像素误差在边界抖动。 */
const STICK_THRESHOLD_PX = 48;

export function useStickToBottom<T extends HTMLElement>(trigger: unknown) {
  const ref = useRef<T | null>(null);
  const stickingRef = useRef(true);

  useEffect(() => {
    const el = ref.current;
    if (!el) return;
    const onScroll = () => {
      const distanceFromBottom = el.scrollHeight - el.scrollTop - el.clientHeight;
      stickingRef.current = distanceFromBottom <= STICK_THRESHOLD_PX;
    };
    el.addEventListener("scroll", onScroll, { passive: true });
    return () => el.removeEventListener("scroll", onScroll);
  }, []);

  useEffect(() => {
    const el = ref.current;
    if (!el || !stickingRef.current) return;
    el.scrollTop = el.scrollHeight;
    // trigger 只是"何时重新评估"的信号，不是要读的值本身——依赖数组只放它是有意的。
  }, [trigger]);

  return ref;
}

import { useEffect, useState, type ReactNode } from "react";

type Props = {
  startedAt?: number | null;
  children?: (workingSeconds: number) => ReactNode;
};

export function WorkingClock({ startedAt, children }: Props) {
  const [, setNow] = useState(() => Date.now());

  useEffect(() => {
    if (startedAt == null) return;
    const timer = window.setInterval(() => setNow(Date.now()), 1000);
    return () => window.clearInterval(timer);
  }, [startedAt]);

  if (startedAt == null) return null;

  const workingSeconds = Math.max(
    0,
    Math.floor((Date.now() - startedAt) / 1000),
  );
  return <>{children?.(workingSeconds) ?? `working · ${workingSeconds}s`}</>;
}

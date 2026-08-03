export type RunMeta = {
  cost_usd: number | null;
  output_tokens: number | null;
  elapsed_sec?: number | null;
};

function fmtElapsed(sec: number): string {
  if (sec < 60) return `${sec}s`;
  const m = Math.floor(sec / 60);
  const s = sec % 60;
  return `${m}m ${String(s).padStart(2, "0")}s`;
}

function fmtTokens(n: number): string {
  return n >= 1000 ? `${(n / 1000).toFixed(1)}k tok` : `${n} tok`;
}

/** 朴素运行耗费串（spec §5.6·原型 `28s · 12.4k tok`）。无数据 → 空串。 */
export function formatRunMeta(done: RunMeta | null): string {
  if (!done) return "";
  const parts: string[] = [];
  if (done.elapsed_sec != null) parts.push(fmtElapsed(done.elapsed_sec));
  if (done.output_tokens != null) parts.push(fmtTokens(done.output_tokens));
  return parts.join(" · ");
}

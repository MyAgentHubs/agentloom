import type { Session } from "../types/agent";

export type SessionUsage = {
  input: number;
  output: number;
};

function validTokenCount(value: number | null | undefined): number {
  return value != null && Number.isFinite(value) && value >= 0 ? value : 0;
}

export function accumulateWorkingTokens(
  prev: number,
  input?: number | null,
  output?: number | null,
): number {
  return prev + validTokenCount(input) + validTokenCount(output);
}

function formatOneDecimal(value: number, keepTrailingZero = false): string {
  const rounded = Math.round(value * 10) / 10;
  return Number.isInteger(rounded) && !keepTrailingZero
    ? String(rounded)
    : rounded.toFixed(1);
}

export function sessionUsageFromSession(
  session: Pick<Session, "total_input_tokens" | "total_output_tokens">,
): SessionUsage {
  return {
    input: validTokenCount(session.total_input_tokens),
    output: validTokenCount(session.total_output_tokens),
  };
}

export function accumulateSessionUsage(
  prev: SessionUsage,
  input?: number | null,
  output?: number | null,
): SessionUsage {
  return {
    input: prev.input + validTokenCount(input),
    output: prev.output + validTokenCount(output),
  };
}

export function formatTokenCount(value: number): string {
  if (value >= 1_000_000) {
    return `${formatOneDecimal(value / 1_000_000, true)}M`;
  }
  if (value >= 100_000) {
    const roundedThousands = Math.round(value / 1_000);
    return roundedThousands >= 1_000
      ? `${formatOneDecimal(value / 1_000_000, true)}M`
      : `${roundedThousands}k`;
  }
  if (value >= 1_000) {
    return `${formatOneDecimal(value / 1_000)}k`;
  }
  return String(value);
}

export function sessionUsageTotal(usage: SessionUsage): number {
  return usage.input + usage.output;
}

export function sessionUsageDetail(usage: SessionUsage): string {
  return `↑ ${formatTokenCount(usage.input)} · ↓ ${formatTokenCount(usage.output)}`;
}

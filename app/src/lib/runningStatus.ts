import type { ChatMessage } from "../types/agent";

export const STREAM_SILENCE_THRESHOLD_SECONDS = 30;
export const STEP_SUMMARY_MAX_CHARS = 40;

type StreamActivityInput = {
  running: boolean;
  sessionId: string | null;
  workingSeconds: number | null;
  messages: ChatMessage[];
  workingTokens: number | null;
};

export type StreamActivityState = {
  running: boolean;
  sessionId: string | null;
  workingSeconds: number;
  messages: ChatMessage[];
  workingTokens: number | null;
  lastEventAtSecond: number;
  silenceSeconds: number | null;
};

/**
 * Reuses the run's existing one-second clock. Message or token identity changes
 * mark a received frontend stream event; no additional timer is needed.
 */
export function advanceStreamActivity(
  previous: StreamActivityState | null,
  input: StreamActivityInput,
): StreamActivityState {
  const workingSeconds = Math.max(0, input.workingSeconds ?? 0);
  const runReset =
    previous === null ||
    !previous.running ||
    !input.running ||
    previous.sessionId !== input.sessionId ||
    workingSeconds < previous.workingSeconds;
  const receivedEvent =
    !runReset &&
    (previous.messages !== input.messages ||
      previous.workingTokens !== input.workingTokens);
  const lastEventAtSecond =
    runReset || receivedEvent ? workingSeconds : previous.lastEventAtSecond;
  const silenceDuration = workingSeconds - lastEventAtSecond;

  return {
    running: input.running,
    sessionId: input.sessionId,
    workingSeconds,
    messages: input.messages,
    workingTokens: input.workingTokens,
    lastEventAtSecond,
    silenceSeconds:
      input.running && silenceDuration > STREAM_SILENCE_THRESHOLD_SECONDS
        ? silenceDuration
        : null,
  };
}

export function truncateStepSummary(
  summary: string,
  maxChars = STEP_SUMMARY_MAX_CHARS,
): string {
  const normalized = summary.replace(/\s+/g, " ").trim();
  const chars = Array.from(normalized);
  if (chars.length <= maxChars) return normalized;
  if (maxChars <= 1) return "…".slice(0, maxChars);
  return `${chars.slice(0, maxChars - 1).join("")}…`;
}

/** Finds the latest visible stream step after the most recent user message. */
export function summarizeLastStep(
  messages: ChatMessage[],
  thinkingLabel: string,
): string | null {
  for (
    let messageIndex = messages.length - 1;
    messageIndex >= 0;
    messageIndex--
  ) {
    const message = messages[messageIndex];
    if (message.role === "user") break;

    for (
      let blockIndex = message.content.length - 1;
      blockIndex >= 0;
      blockIndex--
    ) {
      const block = message.content[blockIndex];
      if (block.type === "tool") {
        const summary = truncateStepSummary(block.summary);
        return summary || null;
      }
      if (block.type === "thinking") return thinkingLabel;
    }
  }
  return null;
}

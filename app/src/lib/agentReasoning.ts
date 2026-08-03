import type { AgentProfile, ReasoningTier } from "../types/agent";

export const REASONING_TIERS: ReasoningTier[] = [
  "auto",
  "none",
  "minimal",
  "low",
  "medium",
  "high",
  "xhigh",
  "max",
];

const COMMON_REASONING_TIERS: ReasoningTier[] = ["low", "medium", "high"];
const CLAUDE_REASONING_TIERS: ReasoningTier[] = [
  "low",
  "medium",
  "high",
  "xhigh",
  "max",
];
const CODEX_REASONING_TIERS: ReasoningTier[] = [
  "minimal",
  "low",
  "medium",
  "high",
  "xhigh",
];
const GENERIC_CAPS = new Set(["true", "1", "on", "native", "reasoning"]);
const DISABLED_CAPS = new Set(["false", "0", "off", "disabled", "no"]);
export const AUTO_REASONING_DEFAULT: ReasoningTier = "medium";

export function asReasoningTier(value: unknown): ReasoningTier | null {
  const normalized =
    typeof value === "string" ? value.trim().toLowerCase() : "";
  return REASONING_TIERS.includes(normalized as ReasoningTier)
    ? (normalized as ReasoningTier)
    : null;
}

function orderedUnique(tiers: ReasoningTier[]): ReasoningTier[] {
  const allowed = new Set(tiers);
  return REASONING_TIERS.filter((tier) => allowed.has(tier));
}

export function defaultReasoningCapabilityForProvider(
  provider: string | null | undefined,
): string | null {
  const normalized = provider?.trim().toLowerCase();
  if (!normalized) return COMMON_REASONING_TIERS.join(",");
  if (normalized === "claude" || normalized === "anthropic") {
    return CLAUDE_REASONING_TIERS.join(",");
  }
  if (normalized === "codex" || normalized === "openai") {
    return CODEX_REASONING_TIERS.join(",");
  }
  return COMMON_REASONING_TIERS.join(",");
}

export function reasoningOptionsForCapability(
  capReasoning: string | null | undefined,
  provider?: string | null,
): ReasoningTier[] {
  const cap = capReasoning?.trim().toLowerCase();
  if (!cap || DISABLED_CAPS.has(cap)) return [];
  if (GENERIC_CAPS.has(cap)) {
    return reasoningOptionsForCapability(
      defaultReasoningCapabilityForProvider(provider),
    );
  }

  const explicit = cap
    .split(/[^a-z0-9]+/)
    .map(asReasoningTier)
    .filter((tier): tier is ReasoningTier => tier !== null);
  if (explicit.length > 0) {
    return orderedUnique(explicit);
  }

  return COMMON_REASONING_TIERS;
}

export function reasoningOptionsForAgent(
  agent: AgentProfile | null | undefined,
): ReasoningTier[] {
  return reasoningOptionsForCapability(agent?.cap_reasoning, agent?.provider);
}

export function defaultReasoningTierForAgent(
  agent: AgentProfile | null | undefined,
): ReasoningTier | null {
  const options = reasoningOptionsForAgent(agent);
  if (options.length === 0) return null;

  const profileDefault = asReasoningTier(agent?.reasoning_default);
  const requestedDefault =
    profileDefault === "auto" ? AUTO_REASONING_DEFAULT : profileDefault;
  if (requestedDefault && options.includes(requestedDefault)) {
    return requestedDefault;
  }
  if (options.includes(AUTO_REASONING_DEFAULT)) return AUTO_REASONING_DEFAULT;
  return options[0];
}

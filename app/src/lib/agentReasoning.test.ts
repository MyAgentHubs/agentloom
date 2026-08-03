import { describe, expect, it } from "vitest";
import type { AgentProfile } from "../types/agent";
import {
  defaultReasoningCapabilityForProvider,
  defaultReasoningTierForAgent,
  reasoningOptionsForCapability,
  reasoningOptionsForAgent,
} from "./agentReasoning";

function agent(overrides: Partial<AgentProfile>): AgentProfile {
  return {
    id: "agent",
    name: "Agent",
    access: "borrow",
    provider: "deepseek",
    primary_model: null,
    endpoint: null,
    auth_mode: null,
    model_opus: null,
    model_sonnet: null,
    model_haiku: null,
    model_subagent: null,
    reasoning_default: "auto",
    max_output_tokens: null,
    api_timeout_ms: null,
    compat_disable_betas: false,
    compat_disable_nonessential: false,
    compat_disable_thinking: false,
    compat_proxy: null,
    custom_headers: null,
    extra_body: null,
    cap_reasoning: null,
    cap_computer_use: null,
    cap_lead: null,
    has_key: true,
    is_builtin: false,
    enabled: true,
    sort_order: 0,
    created_at: 0,
    updated_at: 0,
    ...overrides,
  };
}

describe("agentReasoning", () => {
  it("returns no runtime options when agent does not advertise reasoning capability", () => {
    expect(reasoningOptionsForAgent(agent({ cap_reasoning: null }))).toEqual(
      [],
    );
  });

  it("parses explicit supported tier subsets from cap_reasoning", () => {
    expect(
      reasoningOptionsForAgent(
        agent({
          cap_reasoning: "minimal, high, max",
          reasoning_default: "high",
        }),
      ),
    ).toEqual(["minimal", "high", "max"]);
  });

  it("treats generic reasoning capability as provider-specific tiers", () => {
    expect(
      reasoningOptionsForAgent(agent({ cap_reasoning: "native" })),
    ).toEqual(["low", "medium", "high"]);
    expect(
      reasoningOptionsForAgent(
        agent({ provider: "claude", cap_reasoning: "native" }),
      ),
    ).toEqual(["low", "medium", "high", "xhigh", "max"]);
    expect(
      reasoningOptionsForAgent(
        agent({ provider: "codex", cap_reasoning: "native" }),
      ),
    ).toEqual(["minimal", "low", "medium", "high", "xhigh"]);
  });

  it("maps known provider default capabilities to their CLI-supported tiers", () => {
    expect(defaultReasoningCapabilityForProvider("claude")).toBe(
      "low,medium,high,xhigh,max",
    );
    expect(defaultReasoningCapabilityForProvider("codex")).toBe(
      "minimal,low,medium,high,xhigh",
    );
    expect(reasoningOptionsForCapability("low,medium,high,xhigh,max")).toEqual([
      "low",
      "medium",
      "high",
      "xhigh",
      "max",
    ]);
  });

  it("uses profile default when supported and falls back to the first supported tier", () => {
    expect(
      defaultReasoningTierForAgent(
        agent({ cap_reasoning: "low, high", reasoning_default: "high" }),
      ),
    ).toBe("high");
    expect(
      defaultReasoningTierForAgent(
        agent({ cap_reasoning: "low, high", reasoning_default: "medium" }),
      ),
    ).toBe("low");
  });

  it("treats auto default as medium when that tier exists", () => {
    expect(
      defaultReasoningTierForAgent(
        agent({
          cap_reasoning: "minimal,low,medium,high,xhigh",
          reasoning_default: "auto",
        }),
      ),
    ).toBe("medium");
  });
});

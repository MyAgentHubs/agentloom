import { describe, it, expect } from "vitest";
import { isAgentAvailable, type RuntimeDetect } from "./agentAvailability";
import type { AgentProfile } from "../types/agent";

function agent(p: Partial<AgentProfile>): AgentProfile {
  return {
    id: "a",
    name: "A",
    access: "native",
    provider: "claude",
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
    has_key: false,
    is_builtin: true,
    enabled: true,
    sort_order: 0,
    created_at: 0,
    updated_at: 0,
    ...p,
  };
}
const LOADED: RuntimeDetect = { claude: true, codex: false };

describe("isAgentAvailable", () => {
  it("native + CLI 已装 → true", () => {
    expect(
      isAgentAvailable(agent({ access: "native", provider: "claude" }), LOADED),
    ).toBe(true);
  });
  it("native + CLI 未装 → false", () => {
    expect(
      isAgentAvailable(agent({ access: "native", provider: "codex" }), LOADED),
    ).toBe(false);
  });
  it("native + runtime 未加载(undefined) → true(乐观)", () => {
    expect(
      isAgentAvailable(
        agent({ access: "native", provider: "codex" }),
        undefined,
      ),
    ).toBe(true);
  });
  it("native + runtime 已加载但无此 provider → false(保守)", () => {
    expect(
      isAgentAvailable(agent({ access: "native", provider: "gemini" }), LOADED),
    ).toBe(false);
  });
  it("borrow + has_key=true → true", () => {
    expect(
      isAgentAvailable(agent({ access: "borrow", has_key: true }), LOADED),
    ).toBe(true);
  });
  it("borrow + 无 key → false", () => {
    expect(
      isAgentAvailable(agent({ access: "borrow", has_key: false }), LOADED),
    ).toBe(false);
  });
  it("borrow + 无 key + runtime undefined → false（borrow 不受乐观影响）", () => {
    expect(
      isAgentAvailable(agent({ access: "borrow", has_key: false }), undefined),
    ).toBe(false);
  });
  it("harness + 无 key → true（CLI sidecar·不绑 has_key）", () => {
    expect(
      isAgentAvailable(agent({ access: "harness", has_key: false }), LOADED),
    ).toBe(true);
  });
  it("harness + runtime undefined → true（CLI·乐观）", () => {
    expect(
      isAgentAvailable(agent({ access: "harness", has_key: false }), undefined),
    ).toBe(true);
  });
  it("harness + enabled=false → false", () => {
    expect(
      isAgentAvailable(
        agent({ enabled: false, access: "harness", has_key: false }),
        LOADED,
      ),
    ).toBe(false);
  });
  it("enabled=false → false（任何 access）", () => {
    expect(
      isAgentAvailable(
        agent({ enabled: false, access: "native", provider: "claude" }),
        LOADED,
      ),
    ).toBe(false);
    expect(
      isAgentAvailable(
        agent({ enabled: false, access: "borrow", has_key: true }),
        LOADED,
      ),
    ).toBe(false);
  });
});

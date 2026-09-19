import { fireEvent, screen, within } from "@testing-library/react";
import { expect } from "vitest";
import type { AgentProfile } from "../../types/agent";
import { writeModelCache } from "./agentFormHelpers";

export function agent(overrides: Partial<AgentProfile>): AgentProfile {
  return {
    id: "deepseek",
    name: "DeepSeek",
    access: "borrow",
    provider: "deepseek",
    primary_model: "deepseek-v4-pro",
    endpoint: "https://api.deepseek.com/anthropic",
    auth_mode: "bearer",
    model_opus: null,
    model_sonnet: null,
    model_haiku: null,
    model_subagent: null,
    reasoning_default: "auto",
    max_output_tokens: null,
    api_timeout_ms: null,
    compat_disable_betas: false,
    compat_disable_nonessential: true,
    compat_disable_thinking: false,
    compat_proxy: "thinking_passback",
    custom_headers: null,
    extra_body: null,
    cap_reasoning: null,
    cap_computer_use: null,
    cap_lead: null,
    has_key: false,
    is_builtin: false,
    enabled: true,
    sort_order: 0,
    created_at: 0,
    updated_at: 0,
    ...overrides,
  };
}

export function deferred<T>() {
  let resolve!: (value: T | PromiseLike<T>) => void;
  let reject!: (reason?: unknown) => void;
  const promise = new Promise<T>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

export function detectReady() {
  return {
    claude: { available: true, version: null, path: null, creds_hint: true },
    codex: { available: true, version: null, path: null, creds_hint: true },
  };
}

export function invokeWithConnectionOk(cmd: string) {
  if (cmd === "detect_runtime") return Promise.resolve(detectReady());
  if (cmd === "test_agent_connection") return Promise.resolve({ ok: true });
  if (cmd === "fetch_agent_models") return Promise.resolve([]);
  return Promise.resolve();
}

export function escapeRegExp(value: string) {
  return value.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

export function engineRegion() {
  return screen.getByLabelText("引擎");
}

export function clickEngine(name: string) {
  fireEvent.click(within(engineRegion()).getByRole("button", { name }));
}

export function providerRegion() {
  return screen.getByLabelText("LLM Provider");
}

export function providerChip(name: string) {
  return within(providerRegion()).getByRole("button", {
    name: new RegExp(`^${escapeRegExp(name)}`),
  });
}

export function borrowPreset(name: string) {
  return providerChip(name);
}

export function clickBorrowPreset(name: string) {
  fireEvent.click(borrowPreset(name));
}

export function harnessPreset(name: string) {
  clickEngine("myagent");
  return providerChip(name);
}

export function clickHarnessPreset(name: string) {
  fireEvent.click(harnessPreset(name));
}

export async function passConnectionTest() {
  fireEvent.click(screen.getByTestId("test-conn-btn"));
  await screen.findByText(/连接成功/);
}

export function openMoreOptions() {
  fireEvent.click(screen.getByRole("button", { name: /更多选项/ }));
}

export const HARNESS_DEEPSEEK_ENDPOINT = "https://api.deepseek.com/v1";

export function writeHarnessDeepSeekModelCache(
  models = ["model-a", "model-b"],
) {
  writeModelCache("harness-deepseek", HARNESS_DEEPSEEK_ENDPOINT, models);
}

export function expectAutoMark(label: string) {
  const field = screen.getByLabelText(label).closest("div");
  expect(field).not.toBeNull();
  expect(within(field!).getByText(/自动/)).toBeInTheDocument();
}

export function expectNoAutoMark(label: string) {
  const field = screen.getByLabelText(label).closest("div");
  expect(field).not.toBeNull();
  expect(within(field!).queryByText(/自动/)).toBeNull();
}

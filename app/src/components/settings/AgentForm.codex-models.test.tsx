import { act, render } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { invoke } from "@tauri-apps/api/core";
import { AgentForm } from "./AgentForm";
import { agent, detectReady } from "./AgentForm.test-helpers";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/plugin-dialog", () => ({ open: vi.fn() }));
vi.mock("@tauri-apps/plugin-opener", () => ({ openUrl: vi.fn() }));

const invokeMock = vi.mocked(invoke);
const settle = () =>
  act(async () => {
    for (let i = 0; i < 5; i++) await Promise.resolve();
  });
const listCalls = () =>
  invokeMock.mock.calls.filter(([cmd]) => cmd === "list_codex_models");
const mount = (overrides: Parameters<typeof agent>[0]) =>
  render(
    <AgentForm agent={agent(overrides)} onCancel={vi.fn()} onSaved={vi.fn()} />,
  );

describe("AgentForm codex live model gate", () => {
  beforeEach(() => {
    invokeMock.mockReset();
    invokeMock.mockImplementation((cmd: string) =>
      Promise.resolve(
        cmd === "detect_runtime"
          ? detectReady()
          : cmd === "list_codex_models"
            ? [{ slug: "gpt-9-test" }]
            : undefined,
      ),
    );
    localStorage.clear();
  });

  it("codex 原生 agent 编辑态恰好调用一次 list_codex_models", async () => {
    mount({
      id: "codex",
      access: "native",
      provider: "codex",
      primary_model: "gpt-6-sol",
      endpoint: null,
    });
    await settle();
    expect(listCalls()).toHaveLength(1);
  });

  it.each([
    [
      "claude 原生",
      { id: "claude", access: "native", provider: "claude", endpoint: null },
    ],
    [
      "codex 借壳",
      {
        id: "cb",
        access: "borrow",
        provider: "codex",
        primary_model: "gpt-5.5",
        endpoint: "https://api.openai.com/v1",
      },
    ],
    [
      "openai 借壳",
      {
        id: "cb2",
        access: "borrow",
        provider: "openai",
        primary_model: "gpt-5.5",
        endpoint: null,
      },
    ],
    [
      "codex harness",
      {
        id: "ch",
        access: "harness",
        provider: "codex",
        primary_model: "gpt-5.5",
        endpoint: null,
      },
    ],
    ["deepseek 借壳", { id: "ds", access: "borrow", provider: "deepseek" }],
  ])("%s 不调用 list_codex_models", async (_name, overrides) => {
    mount(overrides);
    await settle();
    expect(listCalls()).toHaveLength(0);
  });

  it("invoke 返回非 Promise（未配置的 mock）时表单不崩", async () => {
    invokeMock.mockReset();
    invokeMock.mockReturnValue(undefined as never);
    expect(() =>
      mount({
        id: "codex",
        access: "native",
        provider: "codex",
        primary_model: "gpt-6-sol",
        endpoint: null,
      }),
    ).not.toThrow();
    await settle();
  });
});

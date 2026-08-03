import {
  fireEvent,
  render,
  screen,
  waitFor,
  within,
} from "@testing-library/react";
import { describe, it, expect, vi, beforeEach } from "vitest";
import { invoke } from "@tauri-apps/api/core";
import { I18nProvider } from "../../i18n";
import type { AgentProfile } from "../../types/agent";
import { SettingsAgents } from "./SettingsAgents";

declare const process: { env: { VITEST_DEFER_INVOKE?: string } };

vi.mock("@tauri-apps/api/core", () => {
  const base = vi.fn();
  // VITEST_DEFER_INVOKE=1 makes every invoke settle one macrotask later, which
  // deterministically exposes assertions that read state landing from a *different*
  // async source than the one they awaited. CI runners are ~12x slower than a dev
  // machine and lose those races for real; this switch reproduces it on purpose.
  return {
    invoke: process.env.VITEST_DEFER_INVOKE
      ? new Proxy(base, {
          apply: (t, self, args) =>
            new Promise((r) => setTimeout(r, 0)).then(() =>
              Reflect.apply(t, self, args),
            ),
        })
      : base,
  };
});

function agent(overrides: Partial<AgentProfile>): AgentProfile {
  return {
    id: "claude",
    name: "Claude Code",
    access: "native",
    provider: "anthropic",
    primary_model: "claude-opus-4.7",
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
    is_builtin: true,
    enabled: true,
    sort_order: 0,
    created_at: 0,
    updated_at: 0,
    ...overrides,
  };
}

describe("SettingsAgents", () => {
  const invokeMock = vi.mocked(invoke);

  beforeEach(() => {
    invokeMock.mockReset();
  });

  it("settings_agents_renders_list", async () => {
    invokeMock.mockResolvedValueOnce([
      agent({
        id: "claude",
        name: "Claude Opus",
        access: "native",
        provider: "claude",
      }),
      agent({
        id: "deepseek",
        name: "DeepSeek",
        access: "borrow",
        provider: "deepseek",
        primary_model: "deepseek-v4",
        is_builtin: false,
        sort_order: 1,
      }),
      agent({
        id: "codex",
        name: "Codex CLI",
        access: "native",
        provider: "codex",
        primary_model: "gpt-5.2",
        sort_order: 2,
      }),
    ]);

    const { container } = render(<SettingsAgents />);

    // 内容化后 SettingsAgents 不再自渲染 nav（nav 由 SettingsSheet 统一提供）
    expect(container.querySelector(".st-nav")).toBeNull();
    expect(
      screen.getByRole("button", { name: "＋ 添加 agent" }),
    ).toBeInTheDocument();

    expect(await screen.findByText("Claude Opus")).toBeInTheDocument();
    expect(screen.getByText("DeepSeek")).toBeInTheDocument();
    expect(screen.getByText("Codex CLI")).toBeInTheDocument();
    expect(
      within(screen.getByTestId("agent-row-claude")).getByText("原生 CLI"),
    ).toBeInTheDocument();
    expect(
      within(screen.getByTestId("agent-row-deepseek")).getByText(
        "经 Claude Code",
      ),
    ).toBeInTheDocument();
    expect(
      within(screen.getByTestId("agent-row-codex")).getByText("原生 CLI"),
    ).toBeInTheDocument();
    expect(screen.queryByText("借壳")).not.toBeInTheDocument();
    expect(screen.queryByText(/借壳/)).not.toBeInTheDocument();
  });

  it("sorts agents naturally by name without using sort_order", async () => {
    invokeMock.mockResolvedValueOnce([
      agent({ id: "codex-10", name: "Codex 10", sort_order: 0 }),
      agent({ id: "alpha-lower", name: "alpha", sort_order: 1 }),
      agent({ id: "codex-2", name: "Codex 2", sort_order: 2 }),
      agent({ id: "alpha-upper", name: "Alpha", sort_order: 3 }),
      agent({ id: "echo", name: "Écho", sort_order: 4 }),
      agent({ id: "echo-accent", name: "Echo", sort_order: 5 }),
    ]);

    render(<SettingsAgents />);

    await screen.findByText("Codex 10");
    const rows = screen.getAllByTestId(/^agent-row-/);
    expect(rows.map((row) => row.dataset.testid)).toEqual([
      "agent-row-alpha-lower",
      "agent-row-alpha-upper",
      "agent-row-codex-2",
      "agent-row-codex-10",
      "agent-row-echo",
      "agent-row-echo-accent",
    ]);
  });

  it("native runtime detected renders Detected without Missing", async () => {
    invokeMock.mockImplementation((command) =>
      Promise.resolve(
        command === "list_agents"
          ? [
              agent({
                id: "claude",
                name: "Claude CLI",
                access: "native",
                provider: "claude",
                has_key: false,
              }),
            ]
          : undefined,
      ),
    );

    render(
      <I18nProvider initialLocale="en">
        <SettingsAgents runtimeDetect={{ claude: true }} />
      </I18nProvider>,
    );

    const row = await screen.findByTestId("agent-row-claude");
    expect(within(row).getByText("Detected")).toBeInTheDocument();
    expect(within(row).queryByText("Missing")).not.toBeInTheDocument();
  });

  it("native runtime unavailable renders Not installed", async () => {
    invokeMock.mockImplementation((command) =>
      Promise.resolve(
        command === "list_agents"
          ? [
              agent({
                id: "codex",
                name: "Codex CLI",
                access: "native",
                provider: "codex",
                has_key: false,
              }),
            ]
          : undefined,
      ),
    );

    render(
      <I18nProvider initialLocale="en">
        <SettingsAgents runtimeDetect={{ codex: false }} />
      </I18nProvider>,
    );

    const row = await screen.findByTestId("agent-row-codex");
    expect(within(row).getByText("Not installed")).toBeInTheDocument();
    expect(within(row).queryByText("Missing")).not.toBeInTheDocument();
  });

  it("native runtime not ready omits the status state", async () => {
    invokeMock.mockResolvedValueOnce([
      agent({
        id: "claude",
        name: "Claude CLI",
        access: "native",
        provider: "claude",
        has_key: false,
      }),
    ]);

    render(<SettingsAgents />);

    const row = await screen.findByTestId("agent-row-claude");
    expect(within(row).getByText("自动检测")).toBeInTheDocument();
    expect(within(row).queryByText("已检测到")).not.toBeInTheDocument();
    expect(within(row).queryByText("未安装")).not.toBeInTheDocument();
    expect(within(row).queryByText("待配")).not.toBeInTheDocument();
  });

  it("keyed API agents keep has_key status behavior", async () => {
    invokeMock.mockResolvedValueOnce([
      agent({
        id: "ready",
        name: "Ready Agent",
        access: "harness",
        provider: "zhipu",
        has_key: true,
      }),
      agent({
        id: "missing",
        name: "Missing Agent",
        access: "harness",
        provider: "deepseek",
        has_key: false,
        sort_order: 1,
      }),
    ]);

    render(<SettingsAgents />);

    const readyRow = await screen.findByTestId("agent-row-ready");
    const missingRow = screen.getByTestId("agent-row-missing");

    expect(within(readyRow).getByText("已配 ✓")).toBeInTheDocument();
    expect(within(missingRow).getByText("待配")).toBeInTheDocument();
  });

  it("native_delete_hidden_and_borrow_delete_still_works", async () => {
    invokeMock
      .mockResolvedValueOnce([
        agent({ id: "claude", name: "Claude Opus", is_builtin: true }),
        agent({
          id: "deepseek",
          name: "DeepSeek",
          access: "borrow",
          provider: "deepseek",
          is_builtin: false,
          sort_order: 1,
        }),
      ])
      .mockResolvedValueOnce([
        agent({ id: "claude", name: "Claude Opus", is_builtin: true }),
      ]);

    render(<SettingsAgents />);

    const builtinRow = await screen.findByTestId("agent-row-claude");
    const customRow = screen.getByTestId("agent-row-deepseek");
    expect(
      within(builtinRow).queryByRole("button", { name: "删除 Claude Opus" }),
    ).not.toBeInTheDocument();

    const customDelete = within(customRow).getByRole("button", {
      name: "删除 DeepSeek",
    });
    expect(customDelete).not.toBeDisabled();
    fireEvent.click(customDelete);

    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("delete_agent", {
        id: "deepseek",
      }),
    );
    await waitFor(() =>
      expect(
        screen.queryByTestId("agent-row-deepseek"),
      ).not.toBeInTheDocument(),
    );
  });

  it("native_agent_shows_auto_detect_badge_and_edit_but_keeps_delete_hidden", async () => {
    invokeMock.mockResolvedValueOnce([
      agent({
        id: "claude",
        name: "Claude CLI",
        access: "native",
        provider: "claude",
        is_builtin: false,
      }),
      agent({
        id: "deepseek",
        name: "DeepSeek",
        access: "borrow",
        is_builtin: false,
        sort_order: 1,
      }),
    ]);

    render(<SettingsAgents />);

    const nativeRow = await screen.findByTestId("agent-row-claude");
    const borrowRow = screen.getByTestId("agent-row-deepseek");
    const autoDetect = within(nativeRow).getByText("自动检测");

    expect(
      within(nativeRow).getByRole("button", { name: "编辑" }),
    ).toBeInTheDocument();
    expect(
      within(nativeRow).queryByRole("button", { name: "删除 Claude CLI" }),
    ).not.toBeInTheDocument();
    expect(autoDetect).toBeInTheDocument();
    expect(autoDetect).toHaveAttribute("title", "随本机 CLI 自动接入·无需配置");
    expect(
      within(borrowRow).getByRole("button", { name: "编辑" }),
    ).toBeInTheDocument();
    expect(
      within(borrowRow).getByRole("button", { name: "删除 DeepSeek" }),
    ).toBeInTheDocument();
  });

  it("native_agent_edit_opens_form_without_api_key", async () => {
    invokeMock.mockResolvedValueOnce([
      agent({
        id: "claude",
        name: "Claude CLI",
        access: "native",
        provider: "claude",
        is_builtin: false,
      }),
    ]);

    render(<SettingsAgents />);

    const nativeRow = await screen.findByTestId("agent-row-claude");
    fireEvent.click(within(nativeRow).getByRole("button", { name: "编辑" }));

    expect(screen.getByText("编辑 agent")).toBeInTheDocument();
    expect(screen.queryByLabelText("Provider 预设")).not.toBeInTheDocument();
    expect(screen.queryByLabelText("API Key")).not.toBeInTheDocument();
    expect(screen.getByLabelText("模型")).toBeInTheDocument();
    expect(screen.getByLabelText("reasoning 默认档")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /进阶设置/ })).toBeNull();
    await waitFor(() => {
      expect(invokeMock).toHaveBeenCalledWith("detect_runtime");
    });
  });

  it("borrow_agent_actions_enabled", async () => {
    invokeMock.mockResolvedValueOnce([
      agent({
        id: "deepseek",
        name: "DeepSeek",
        access: "borrow",
        is_builtin: false,
      }),
    ]);

    render(<SettingsAgents />);

    const borrowRow = await screen.findByTestId("agent-row-deepseek");

    expect(
      within(borrowRow).getByRole("button", { name: "编辑" }),
    ).not.toBeDisabled();
    expect(
      within(borrowRow).getByRole("button", { name: "删除 DeepSeek" }),
    ).not.toBeDisabled();
  });

  it("harness agent 显「内置引擎」chip（harness 类）+ 可编辑可删", async () => {
    invokeMock.mockResolvedValueOnce([
      agent({
        id: "glm",
        name: "GLM-4.7",
        access: "harness",
        provider: "zhipu",
        is_builtin: false,
        sort_order: 1,
      }),
    ]);

    render(<SettingsAgents />);

    const row = await screen.findByTestId("agent-row-glm");

    expect(within(row).getByText("内置引擎")).toBeInTheDocument();
    expect(within(row).queryByText("原生 CLI")).not.toBeInTheDocument();
    expect(within(row).getByText("内置引擎").className).toContain("harness");
    expect(
      within(row).getByRole("button", { name: "编辑" }),
    ).toBeInTheDocument();
    expect(
      within(row).getByRole("button", { name: /删除/ }),
    ).toBeInTheDocument();
  });
});

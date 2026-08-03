import { describe, it, expect, vi } from "vitest";
import { render, screen, fireEvent } from "@testing-library/react";
import type { AgentProfile } from "../types/agent";
import { AgentDropdown } from "./AgentDropdown";

function agent(overrides: Partial<AgentProfile>): AgentProfile {
  return {
    id: "agent",
    name: "Agent",
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
    is_builtin: false,
    enabled: true,
    sort_order: 0,
    created_at: 0,
    updated_at: 0,
    ...overrides,
  };
}

describe("AgentDropdown", () => {
  it("dropdown_does_not_render_builtin_options_without_agent_data", () => {
    render(<AgentDropdown agentId="missing-agent" onAgentChange={vi.fn()} />);
    const trigger = screen.getByRole("button", { name: /选择 agent/ });
    expect(trigger).toHaveTextContent("Agent");
    fireEvent.click(trigger);
    expect(screen.queryByRole("menuitemradio", { name: /Claude/ })).toBeNull();
    expect(screen.queryByRole("menuitemradio", { name: /Codex/ })).toBeNull();
    expect(
      screen.queryByRole("menuitemradio", { name: /DeepSeek/ }),
    ).toBeNull();
  });

  it("选 agent → onAgentChange(agent id) 后关闭", () => {
    const onAgentChange = vi.fn();
    const agents = [
      agent({ id: "claude-main", name: "Claude Main", sort_order: 0 }),
      agent({
        id: "codex-main",
        name: "Codex Main",
        provider: "codex",
        sort_order: 1,
      }),
    ];

    render(
      <AgentDropdown
        agents={agents}
        agentId="claude-main"
        onAgentChange={onAgentChange}
      />,
    );
    fireEvent.click(screen.getByRole("button", { name: /选择 agent/ }));
    fireEvent.click(screen.getByRole("menuitemradio", { name: /Codex Main/ }));
    expect(onAgentChange).toHaveBeenCalledWith("codex-main");
    expect(screen.queryByRole("menu")).toBeNull();
  });

  it("disabledIds 中的 agent 灰禁且不可选", () => {
    const onAgentChange = vi.fn();
    const agents = [
      agent({ id: "claude-main", name: "Claude Main", sort_order: 0 }),
      agent({
        id: "codex-main",
        name: "Codex Main",
        provider: "codex",
        sort_order: 1,
      }),
    ];

    render(
      <AgentDropdown
        agents={agents}
        agentId="claude-main"
        onAgentChange={onAgentChange}
        disabledIds={["codex-main"]}
      />,
    );
    fireEvent.click(screen.getByRole("button", { name: /选择 agent/ }));
    const disabledItem = screen.getByRole("menuitemradio", {
      name: /Codex Main/,
    });
    expect(disabledItem).toBeDisabled();
    fireEvent.click(disabledItem);
    expect(onAgentChange).not.toHaveBeenCalled();
  });

  it("dropdown_renders_dynamic_agents", () => {
    const onAgentChange = vi.fn();
    const agents = [
      agent({
        id: "kimi-z",
        name: "Kimi Agent",
        provider: "kimi",
        sort_order: 10,
      }),
      agent({
        id: "disabled-agent",
        name: "Disabled Agent",
        provider: "deepseek",
        enabled: false,
        sort_order: 0,
      }),
      agent({
        id: "glm-a",
        name: "GLM Agent",
        provider: "glm",
        sort_order: 10,
      }),
    ];

    render(
      <AgentDropdown
        agents={agents}
        agentId="kimi-z"
        onAgentChange={onAgentChange}
      />,
    );

    expect(
      screen.getByRole("button", { name: /选择 agent/ }),
    ).toHaveTextContent("Kimi Agent");

    fireEvent.click(screen.getByRole("button", { name: /选择 agent/ }));

    const items = screen.getAllByRole("menuitemradio");
    expect(items).toHaveLength(2);
    expect(
      items.map((item) => item.querySelector(".dd__item-name")?.textContent),
    ).toEqual(["GLM Agent", "Kimi Agent"]);
    expect(screen.queryByText("Disabled Agent")).toBeNull();
    const selected = screen.getByRole("menuitemradio", { name: /Kimi Agent/ });
    expect(selected).toHaveAttribute("aria-checked", "true");
    expect(selected).toHaveClass("dd__item--on");

    fireEvent.click(screen.getByRole("menuitemradio", { name: /GLM Agent/ }));
    expect(onAgentChange).toHaveBeenCalledWith("glm-a");
    expect(screen.queryByRole("menu")).toBeNull();
  });

  it("常驻「管理 agent →」页脚·点击触发 onMenuAgents", () => {
    const onMenuAgents = vi.fn();
    render(
      <AgentDropdown
        agents={[agent({ id: "claude", name: "Claude", provider: "claude" })]}
        agentId="claude"
        onAgentChange={() => {}}
        onMenuAgents={onMenuAgents}
      />,
    );
    fireEvent.click(screen.getByLabelText("选择 agent"));
    fireEvent.click(screen.getByRole("button", { name: /管理 agent/ }));
    expect(onMenuAgents).toHaveBeenCalledTimes(1);
  });

  it("零可用 → 空态引导 + 页脚仍在", () => {
    const onMenuAgents = vi.fn();
    render(
      <AgentDropdown
        agents={[]}
        agentId=""
        onAgentChange={() => {}}
        onMenuAgents={onMenuAgents}
      />,
    );
    fireEvent.click(screen.getByLabelText("选择 agent"));
    expect(screen.getByText(/暂无可用 agent/)).toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: /管理 agent/ }),
    ).toBeInTheDocument();
    expect(screen.queryAllByRole("menuitemradio")).toHaveLength(0);
  });

  it("当前 agentId 不在可用集·trigger 仍回退显示首个可用", () => {
    render(
      <AgentDropdown
        agents={[agent({ id: "claude", name: "Claude", provider: "claude" })]}
        agentId="gone"
        onAgentChange={() => {}}
      />,
    );
    expect(screen.getByLabelText("选择 agent")).toHaveTextContent("Claude");
  });

  it("disabled 时 trigger 禁用", () => {
    render(
      <AgentDropdown
        agents={[agent({ id: "claude-main", name: "Claude Main" })]}
        agentId="claude-main"
        onAgentChange={vi.fn()}
        disabled
      />,
    );
    const trigger = screen.getByLabelText("选择 agent");
    expect(trigger).toBeDisabled();
    expect(trigger).toHaveTextContent("Claude Main");
  });

  it("loading=true → trigger 显「…」且禁用", () => {
    render(
      <AgentDropdown
        agents={[agent({ id: "claude", name: "Claude", provider: "claude" })]}
        agentId="claude"
        onAgentChange={() => {}}
        loading
      />,
    );
    const trigger = screen.getByLabelText("选择 agent");
    expect(trigger).toBeDisabled();
    expect(trigger).toHaveTextContent("…");
  });
});

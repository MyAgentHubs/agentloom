import { describe, expect, it, vi } from "vitest";
import {
  fireEvent,
  render,
  screen,
  waitFor,
  within,
} from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import type { ComponentProps } from "react";
import type { AgentProfile } from "../types/agent";
import { ComposerAgentSelector } from "./ComposerAgentSelector";

function agent(overrides: Partial<AgentProfile>): AgentProfile {
  return {
    id: "agent",
    name: "Agent",
    access: "native",
    provider: "generic",
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

const agents = [
  agent({
    id: "alpha-lead",
    name: "Alpha Lead",
    provider: "claude",
    access: "native",
    cap_lead: "planner",
    sort_order: 0,
  }),
  agent({
    id: "beta-worker",
    name: "Beta Worker",
    provider: "beta",
    cap_lead: null,
    sort_order: 1,
  }),
  agent({
    id: "gamma-lead",
    name: "Gamma Lead",
    provider: "claude",
    access: "native",
    cap_lead: "planner",
    sort_order: 2,
  }),
  agent({
    id: "disabled-agent",
    name: "Disabled Agent",
    enabled: false,
    sort_order: 3,
  }),
];

function renderSelector(
  overrides: Partial<ComponentProps<typeof ComposerAgentSelector>> = {},
) {
  const props: ComponentProps<typeof ComposerAgentSelector> = {
    agents,
    agentId: "alpha-lead",
    leadId: null,
    memberIds: [],
    onAgentChange: vi.fn(),
    onSetLead: vi.fn(),
    onToggleMember: vi.fn(),
    ...overrides,
  };

  const utils = render(<ComposerAgentSelector {...props} />);
  return { ...props, ...utils };
}

describe("ComposerAgentSelector", () => {
  it("Solo trigger 显当前 agent，点普通 row 调 onAgentChange", () => {
    const props = renderSelector({ agentId: "beta-worker" });

    const trigger = screen.getByRole("button", {
      name: "选择 agent：Beta Worker",
    });
    expect(trigger).toHaveTextContent("Beta Worker");
    expect(trigger).not.toHaveTextContent("成员");

    fireEvent.click(trigger);
    expect(screen.getByText("选择 agent")).toBeVisible();
    fireEvent.click(screen.getByRole("menuitemradio", { name: /Gamma Lead/ }));

    expect(props.onAgentChange).toHaveBeenCalledWith("gamma-lead");
    expect(props.onSetLead).not.toHaveBeenCalled();
    expect(screen.queryByRole("menu")).toBeNull();
  });

  it("Solo agent 主选区是真 button，可用键盘 Enter 选择", async () => {
    const user = userEvent.setup();
    const props = renderSelector();

    await user.click(
      screen.getByRole("button", { name: "选择 agent：Alpha Lead" }),
    );
    const option = screen.getByRole("menuitemradio", { name: /Gamma Lead/ });

    expect(option.tagName).toBe("BUTTON");

    option.focus();
    await user.keyboard("{Enter}");

    expect(props.onAgentChange).toHaveBeenCalledWith("gamma-lead");
    expect(screen.queryByRole("menu")).toBeNull();
  });

  it("菜单行只做布局，避免 menuitemradio 内嵌交互按钮", () => {
    renderSelector();

    fireEvent.click(
      screen.getByRole("button", { name: "选择 agent：Alpha Lead" }),
    );

    for (const row of document.querySelectorAll(".cas-row")) {
      expect(row).toHaveAttribute("role", "none");
    }
  });

  it("Solo 菜单同时保留普通 agent 选择和明确的队长入口", () => {
    const props = renderSelector();

    fireEvent.click(
      screen.getByRole("button", { name: "选择 agent：Alpha Lead" }),
    );

    const leadButton = screen.getByRole("button", {
      name: "设为队长 Alpha Lead",
    });
    expect(leadButton).toBeInTheDocument();
    expect(leadButton).toHaveClass("cas-lead-star");
    expect(leadButton).toHaveTextContent("");
    expect(
      screen.getByRole("button", { name: "设为队长 Gamma Lead" }),
    ).toBeInTheDocument();
    expect(
      screen.queryByRole("button", { name: "设为队长 Beta Worker" }),
    ).toBeNull();
    expect(screen.queryByRole("button", { name: /成员 / })).toBeNull();
    expect(document.querySelector(".cas-ctl")).not.toBeNull();

    fireEvent.click(screen.getByRole("menuitemradio", { name: /Gamma Lead/ }));

    expect(props.onAgentChange).toHaveBeenCalledWith("gamma-lead");
    expect(props.onSetLead).not.toHaveBeenCalled();
  });

  it("Solo 菜单点皇冠直接调 onSetLead(id)，不切普通 agent", () => {
    const props = renderSelector();

    fireEvent.click(
      screen.getByRole("button", { name: "选择 agent：Alpha Lead" }),
    );
    fireEvent.click(
      screen.getByRole("button", { name: "设为队长 Gamma Lead" }),
    );

    expect(props.onSetLead).toHaveBeenCalledWith("gamma-lead", [
      "alpha-lead",
      "beta-worker",
    ]);
    expect(props.onAgentChange).not.toHaveBeenCalled();
    expect(screen.getByRole("menu")).toBeVisible();
  });

  it("显式 Team 配置态才渲染队长控制，点可带队皇冠调 onSetLead(id)", () => {
    const props = renderSelector({ teamMode: true });

    fireEvent.click(
      screen.getByRole("button", { name: "选择 agent：Alpha Lead" }),
    );
    expect(screen.getByText("这个会话用谁")).toBeVisible();
    fireEvent.click(
      screen.getByRole("button", { name: "设为队长 Alpha Lead" }),
    );

    expect(props.onSetLead).toHaveBeenCalledWith("alpha-lead", [
      "beta-worker",
      "gamma-lead",
    ]);
    expect(props.onAgentChange).not.toHaveBeenCalled();
  });

  it("点皇冠后配置保存 pending 时保持 Team 菜单，不闪回 Solo", async () => {
    const props = renderSelector({ teamMode: true });

    fireEvent.click(
      screen.getByRole("button", { name: "选择 agent：Alpha Lead" }),
    );
    fireEvent.click(
      screen.getByRole("button", { name: "设为队长 Alpha Lead" }),
    );

    expect(props.onSetLead).toHaveBeenCalledWith("alpha-lead", [
      "beta-worker",
      "gamma-lead",
    ]);
    expect(
      screen.getByRole("button", {
        name: "选择 agent：队长 Alpha Lead，成员 2",
      }),
    ).toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: "取消队长 Alpha Lead" }),
    ).toBeInTheDocument();

    props.rerender(
      <ComposerAgentSelector
        agents={agents}
        agentId="alpha-lead"
        leadId={null}
        memberIds={[]}
        teamMode
        saving
        onAgentChange={props.onAgentChange}
        onSetLead={props.onSetLead}
        onToggleMember={props.onToggleMember}
      />,
    );

    expect(
      screen.getByRole("button", {
        name: "选择 agent：队长 Alpha Lead，成员 2",
      }),
    ).toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: "取消队长 Alpha Lead" }),
    ).toBeInTheDocument();

    props.rerender(
      <ComposerAgentSelector
        agents={agents}
        agentId="alpha-lead"
        leadId={null}
        memberIds={[]}
        teamMode
        saving={false}
        onAgentChange={props.onAgentChange}
        onSetLead={props.onSetLead}
        onToggleMember={props.onToggleMember}
      />,
    );

    await waitFor(() =>
      expect(
        screen.getByRole("button", { name: "选择 agent：Alpha Lead" }),
      ).toBeInTheDocument(),
    );
  });

  it("Team 配置态不可带队 agent 不显示误导性的队长按钮", () => {
    renderSelector({ teamMode: true });

    fireEvent.click(
      screen.getByRole("button", { name: "选择 agent：Alpha Lead" }),
    );

    expect(
      screen.queryByRole("button", { name: "设为队长 Beta Worker" }),
    ).toBeNull();
  });

  it("Team 配置态非队长 agent 仍可作为成员切换", () => {
    const props = renderSelector({ leadId: "alpha-lead", memberIds: [] });

    fireEvent.click(
      screen.getByRole("button", {
        name: "选择 agent：队长 Alpha Lead，成员 0",
      }),
    );
    fireEvent.click(screen.getByRole("button", { name: "成员 Beta Worker" }));

    expect(props.onSetLead).not.toHaveBeenCalled();
    expect(props.onAgentChange).not.toHaveBeenCalledWith("beta-worker");
    expect(props.onToggleMember).toHaveBeenCalledWith("beta-worker");
  });

  it("Team trigger 显队长和成员数；lead 行点 on crown 调 onSetLead(null)", () => {
    const props = renderSelector({
      leadId: "alpha-lead",
      memberIds: ["alpha-lead", "beta-worker", "disabled-agent"],
    });

    const trigger = screen.getByRole("button", {
      name: "选择 agent：队长 Alpha Lead，成员 1",
    });
    expect(trigger).toHaveTextContent("队长");
    expect(trigger).toHaveTextContent("Alpha Lead");
    expect(trigger).toHaveTextContent("成员 1");

    fireEvent.click(trigger);
    fireEvent.click(
      screen.getByRole("button", { name: "取消队长 Alpha Lead" }),
    );

    expect(props.onSetLead).toHaveBeenCalledWith(null);
  });

  it("保存的 current lead 可点皇冠取消回 Solo", () => {
    const props = renderSelector({
      leadId: "beta-worker",
      memberIds: ["alpha-lead"],
    });

    fireEvent.click(
      screen.getByRole("button", {
        name: "选择 agent：队长 Beta Worker，成员 1",
      }),
    );
    const cancelLead = screen.getByRole("button", {
      name: "取消队长 Beta Worker",
    });
    expect(cancelLead).not.toBeDisabled();

    fireEvent.click(cancelLead);

    expect(props.onSetLead).toHaveBeenCalledWith(null);
  });

  it("取消队长后配置保存 pending 时保持 Solo 触发器", () => {
    const props = renderSelector({
      leadId: "alpha-lead",
      memberIds: ["beta-worker"],
    });

    fireEvent.click(
      screen.getByRole("button", {
        name: "选择 agent：队长 Alpha Lead，成员 1",
      }),
    );
    fireEvent.click(
      screen.getByRole("button", { name: "取消队长 Alpha Lead" }),
    );

    expect(props.onSetLead).toHaveBeenCalledWith(null);
    expect(
      screen.getByRole("button", { name: "选择 agent：Alpha Lead" }),
    ).toBeInTheDocument();

    props.rerender(
      <ComposerAgentSelector
        agents={agents}
        agentId="alpha-lead"
        leadId="alpha-lead"
        memberIds={["beta-worker"]}
        saving
        onAgentChange={props.onAgentChange}
        onSetLead={props.onSetLead}
        onToggleMember={props.onToggleMember}
      />,
    );

    expect(
      screen.getByRole("button", { name: "选择 agent：Alpha Lead" }),
    ).toBeInTheDocument();
  });

  it("Team member toggle 只在非 lead 行出现，点击调 onToggleMember(id)", () => {
    const props = renderSelector({
      leadId: "alpha-lead",
      memberIds: ["beta-worker"],
    });

    fireEvent.click(
      screen.getByRole("button", {
        name: "选择 agent：队长 Alpha Lead，成员 1",
      }),
    );

    const leadRow = screen
      .getByRole("menuitemradio", { name: /Alpha Lead/ })
      .closest(".cas-row");
    expect(leadRow).not.toBeNull();
    expect(
      within(leadRow as HTMLElement).queryByRole("button", {
        name: "成员 Alpha Lead",
      }),
    ).toBeNull();

    const memberToggle = screen.getByRole("button", {
      name: "成员 Beta Worker",
    });
    expect(memberToggle).toHaveAttribute("aria-pressed", "true");

    fireEvent.click(memberToggle);

    expect(props.onToggleMember).toHaveBeenCalledWith("beta-worker");
    expect(props.onAgentChange).not.toHaveBeenCalled();
    expect(screen.getByRole("menu")).toBeVisible();
  });

  it("memberIds=[] Team 显成员 0，不当全员", () => {
    renderSelector({ leadId: "alpha-lead", memberIds: [] });

    const trigger = screen.getByRole("button", {
      name: "选择 agent：队长 Alpha Lead，成员 0",
    });
    expect(trigger).toHaveTextContent("成员 0");
    expect(trigger.querySelectorAll(".agent-avatar")).toHaveLength(1);
    expect(trigger.querySelector(".cas-btn__lead")).toBeNull();
  });

  it("Team 菜单每行左侧只放 agent avatar，非 native Claude 行右侧给禁用皇冠和成员开关", () => {
    renderSelector({ leadId: "alpha-lead", memberIds: ["beta-worker"] });

    fireEvent.click(
      screen.getByRole("button", {
        name: "选择 agent：队长 Alpha Lead，成员 1",
      }),
    );

    const row = screen
      .getByRole("menuitemradio", { name: /Beta Worker/ })
      .closest(".cas-row");
    expect(row).not.toBeNull();
    expect(row!.firstElementChild).toHaveClass("cas-main");
    expect(row!.querySelectorAll(".cas-main .agent-avatar")).toHaveLength(1);
    expect(row).toHaveTextContent("beta · 仅可当队员");
    const leadCrown = within(row as HTMLElement).getByRole("button", {
      name: "该引擎暂不支持当队长（codex 开发中）",
    });
    expect(leadCrown).toHaveClass("cas-lead-star");
    expect(leadCrown).toBeDisabled();
    expect(leadCrown).toHaveAttribute(
      "title",
      "该引擎暂不支持当队长（codex 开发中）",
    );
    expect(
      within(row as HTMLElement).getByRole("button", {
        name: "成员 Beta Worker",
      }),
    ).toBeInTheDocument();
    expect(row!.querySelector(":scope > .cas-lead-star")).toBeNull();

    const claudeRow = screen
      .getByRole("menuitemradio", { name: /Alpha Lead/ })
      .closest(".cas-row");
    expect(claudeRow).toHaveTextContent("claude · 可带队 + 调度");
  });

  it("Team 菜单顺序为队长、已选成员、未选 agent 默认顺序", () => {
    renderSelector({ leadId: "gamma-lead", memberIds: ["beta-worker"] });

    fireEvent.click(
      screen.getByRole("button", {
        name: "选择 agent：队长 Gamma Lead，成员 1",
      }),
    );

    expect(screen.getByText("Auto")).toBeInTheDocument();
    const names = Array.from(
      document.querySelectorAll(".cas-list .cas-main__name"),
    ).map((node) => node.textContent);
    expect(names).toEqual(["Gamma Lead", "Beta Worker", "Alpha Lead"]);
  });

  it("disabled/loading 时已打开菜单也不触发选择、皇冠、成员或管理动作", () => {
    const onMenuAgents = vi.fn();
    const props = renderSelector({
      leadId: "alpha-lead",
      memberIds: ["beta-worker"],
      onMenuAgents,
    });

    fireEvent.click(
      screen.getByRole("button", {
        name: "选择 agent：队长 Alpha Lead，成员 1",
      }),
    );
    props.rerender(
      <ComposerAgentSelector
        agents={agents}
        agentId="alpha-lead"
        leadId="alpha-lead"
        memberIds={["beta-worker"]}
        onAgentChange={props.onAgentChange}
        onSetLead={props.onSetLead}
        onToggleMember={props.onToggleMember}
        onMenuAgents={onMenuAgents}
        disabled
        loading
      />,
    );

    fireEvent.click(screen.getByRole("menuitemradio", { name: /Gamma Lead/ }));
    fireEvent.click(
      screen.getByRole("button", { name: "取消队长 Alpha Lead" }),
    );
    fireEvent.click(screen.getByRole("button", { name: "成员 Beta Worker" }));
    fireEvent.click(screen.getByRole("button", { name: /管理 agent/ }));

    expect(props.onAgentChange).not.toHaveBeenCalled();
    expect(props.onSetLead).not.toHaveBeenCalled();
    expect(props.onToggleMember).not.toHaveBeenCalled();
    expect(onMenuAgents).not.toHaveBeenCalled();
  });

  it("Escape 和外部点击关闭菜单", () => {
    renderSelector();

    fireEvent.click(
      screen.getByRole("button", { name: "选择 agent：Alpha Lead" }),
    );
    expect(screen.getByRole("menu")).toBeVisible();
    fireEvent.keyDown(document, { key: "Escape" });
    expect(screen.queryByRole("menu")).toBeNull();

    fireEvent.click(
      screen.getByRole("button", { name: "选择 agent：Alpha Lead" }),
    );
    expect(screen.getByRole("menu")).toBeVisible();
    fireEvent.mouseDown(document.body);
    expect(screen.queryByRole("menu")).toBeNull();
  });

  it("Empty agents + manage action", () => {
    const onMenuAgents = vi.fn();
    renderSelector({
      agents: [],
      agentId: "",
      onMenuAgents,
    });

    fireEvent.click(screen.getByRole("button", { name: "选择 agent：Agent" }));

    expect(screen.getByText("暂无可用 agent · 去 Settings 配置")).toBeVisible();
    fireEvent.click(screen.getByRole("button", { name: /管理 agent/ }));
    expect(onMenuAgents).toHaveBeenCalledTimes(1);
  });
});

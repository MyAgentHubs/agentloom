import { fireEvent, render, screen } from "@testing-library/react";
import { describe, it, expect, vi } from "vitest";
import { ProjectIntroPage } from "./ProjectIntroPage";
import type { AgentProfile } from "../types/agent";

vi.mock("./RepoDocumentPanel", () => ({
  RepoDocumentPanel: ({
    repoId,
    agentId,
    kind,
  }: {
    repoId: string | null;
    agentId: string;
    kind: "intro" | "daily";
  }) => (
    <div
      data-testid="repo-document-panel"
      data-repo-id={repoId ?? ""}
      data-agent-id={agentId}
      data-kind={kind}
    />
  ),
}));

function agentProfile(
  id: string,
  name: string,
  provider: string,
  sortOrder: number,
): AgentProfile {
  return {
    id,
    name,
    access: "api",
    provider,
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
    is_builtin: true,
    enabled: true,
    sort_order: sortOrder,
    created_at: 0,
    updated_at: 0,
  };
}

const agents = [
  agentProfile("claude", "Claude", "anthropic", 0),
  agentProfile("deepseek", "DeepSeek", "deepseek", 1),
];

const repo = {
  id: "r1",
  source: "local",
  owner: null,
  name: "ai-cat-pet",
  path: "/Users/me/code/ai-cat-pet",
  status: "active",
  added_at: 1,
  last_used_at: null,
  namespace_id: "local",
};

describe("ProjectIntroPage", () => {
  it("关联项目时显项目名、path 和两个功能 tab，默认项目简报", () => {
    render(<ProjectIntroPage activeRepo={repo} agentId="deepseek" />);
    expect(screen.getByText("ai-cat-pet")).toBeInTheDocument();
    expect(screen.getByText(repo.path)).toBeInTheDocument();
    const tabs = screen.getAllByRole("tab");
    expect(tabs).toHaveLength(2);
    expect(tabs[0]).toHaveTextContent("项目简报");
    expect(tabs[0]).toHaveAttribute("aria-selected", "true");
    expect(tabs[1]).toHaveTextContent("Daily");
    expect(tabs[1]).toHaveAttribute("aria-selected", "false");
    const panel = screen.getByTestId("repo-document-panel");
    expect(panel).toHaveAttribute("data-kind", "intro");
    expect(panel).toHaveAttribute("data-repo-id", "r1");
    expect(panel).toHaveAttribute("data-agent-id", "deepseek");
  });

  it("点击 Daily tab 后切换选中态和面板 kind", () => {
    render(<ProjectIntroPage activeRepo={repo} />);
    const introTab = screen.getByRole("tab", { name: "项目简报" });
    const dailyTab = screen.getByRole("tab", { name: "Daily" });
    fireEvent.click(dailyTab);
    expect(introTab).toHaveAttribute("aria-selected", "false");
    expect(dailyTab).toHaveAttribute("aria-selected", "true");
    expect(screen.getByTestId("repo-document-panel")).toHaveAttribute(
      "data-kind",
      "daily",
    );
  });

  it("默认 session 仍显示页头、两个 tab，并向面板传 null repoId", () => {
    render(<ProjectIntroPage activeRepo={null} />);
    expect(
      screen.getByRole("heading", { name: "默认会话" }),
    ).toBeInTheDocument();
    expect(
      screen.getByText("无关联项目 · 工作目录由 AgentLoom 自动管理"),
    ).toBeInTheDocument();
    expect(screen.getAllByRole("tab")).toHaveLength(2);
    expect(screen.getByTestId("repo-document-panel")).toHaveAttribute(
      "data-repo-id",
      "",
    );
  });

  it("底部 InputArea composer 仍在", () => {
    render(<ProjectIntroPage activeRepo={repo} />);
    expect(screen.getByPlaceholderText("输入消息…")).toBeInTheDocument();
  });

  it("composer 透传 agents / agentId / onAgentChange", () => {
    const onAgentChange = vi.fn();
    render(
      <ProjectIntroPage
        activeRepo={repo}
        agents={agents}
        agentId="claude"
        onAgentChange={onAgentChange}
        onSend={() => {}}
        composerBusy={false}
      />,
    );
    fireEvent.click(screen.getByRole("button", { name: /选择 agent/ }));
    fireEvent.click(screen.getByRole("menuitemradio", { name: /DeepSeek/ }));
    expect(onAgentChange).toHaveBeenCalledWith("deepseek");
  });

  it("composer 透传 team lead / roster handlers 给 ComposerAgentSelector", () => {
    const onToggleRoster = vi.fn();
    render(
      <ProjectIntroPage
        activeRepo={repo}
        agents={[
          { ...agents[0], cap_lead: "planner" },
          { ...agents[1], cap_lead: null },
        ]}
        agentId="claude"
        onSend={() => {}}
        composerBusy={false}
        mode="team"
        teamLeadId="claude"
        rosterIds={["deepseek"]}
        onSetLead={vi.fn()}
        onToggleRoster={onToggleRoster}
      />,
    );

    fireEvent.click(
      screen.getByRole("button", {
        name: /选择 agent：队长 Claude，成员 1/,
      }),
    );
    fireEvent.click(screen.getByRole("button", { name: "成员 DeepSeek" }));

    expect(onToggleRoster).toHaveBeenCalledWith("deepseek", [
      "claude",
      "deepseek",
    ]);
  });
});

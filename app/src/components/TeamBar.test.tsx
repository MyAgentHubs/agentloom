import { describe, it, expect, vi } from "vitest";
import { render, screen, fireEvent } from "@testing-library/react";
import { TeamBar } from "./TeamBar";
import type { AgentProfile } from "../types/agent";

const AGENTS = [
  {
    id: "claude",
    name: "Claude Opus",
    provider: "claude",
    access: "native",
    cap_lead: "native_cli",
    enabled: true,
  },
  {
    id: "codex",
    name: "codex",
    provider: "codex",
    access: "native",
    cap_lead: null,
    enabled: true,
  },
  {
    id: "deepseek",
    name: "DeepSeek",
    provider: "deepseek",
    access: "borrow",
    cap_lead: null,
    enabled: true,
  },
] as unknown as AgentProfile[];

function setup(over: Partial<React.ComponentProps<typeof TeamBar>> = {}) {
  const props = {
    agents: AGENTS,
    leadId: "claude",
    rosterIds: null as string[] | null,
    onSetLead: vi.fn(),
    onToggleRoster: vi.fn(),
    runningCount: null as number | null,
    autonomy: "cautious" as string,
    onSetAutonomy: vi.fn(),
    ...over,
  };
  render(<TeamBar {...props} />);
  return props;
}

describe("TeamBar", () => {
  it("折叠态显 Lead·成员数（rosterIds=null → 全 enabled 计数）", () => {
    setup();
    expect(screen.getByText(/Lead/)).toBeInTheDocument();
    expect(screen.getByText("Claude Opus")).toBeInTheDocument();
    expect(screen.getByText("成员 3 名")).toBeInTheDocument();
  });

  it("运行中显「M 名成员在跑 · 共 N」", () => {
    setup({ runningCount: 2 });
    expect(screen.getByText("2 名成员在跑 · 共 3")).toBeInTheDocument();
  });

  it("点折叠条 → 展开面板（Lead 选择 + 名单 + 自动派 toggle）", () => {
    setup();
    fireEvent.click(screen.getByRole("button", { name: /展开/ }));
    expect(screen.getByText(/Lead（带队/)).toBeInTheDocument();
    expect(screen.getByText(/成员名单/)).toBeInTheDocument();
  });

  it("展开面板后渲染会话级组队配置标题", () => {
    setup();
    fireEvent.click(screen.getByRole("button", { name: /展开/ }));
    expect(
      screen.getByText("组队配置 · 会话级（改动粘滞本会话）"),
    ).toBeInTheDocument();
  });

  it("Lead 选择：codex（native 非 claude）不可当 Lead，borrow（deepseek）可以（L1b 矩阵）", () => {
    setup();
    fireEvent.click(screen.getByRole("button", { name: /展开/ }));
    const codexOpt = screen.getByRole("button", { name: /codex/ });
    expect(codexOpt).toBeDisabled();
    expect(screen.getAllByText(/暂不能当 Lead/).length).toBeGreaterThan(0);
    const deepseekOpt = screen.getByRole("button", { name: /DeepSeek/ });
    expect(deepseekOpt).not.toBeDisabled();
  });

  it("点可当 Lead 的项 → onSetLead", () => {
    const p = setup({ leadId: "codex" });
    fireEvent.click(screen.getByRole("button", { name: /展开/ }));
    fireEvent.click(screen.getByRole("button", { name: /Claude Opus/ }));
    expect(p.onSetLead).toHaveBeenCalledWith("claude");
  });

  it("勾成员 → onToggleRoster(id, 全集)（带全集上下文·治反向收窄）", () => {
    const p = setup();
    fireEvent.click(screen.getByRole("button", { name: /展开/ }));
    fireEvent.click(screen.getByLabelText(/成员 codex/));
    expect(p.onToggleRoster).toHaveBeenCalledWith("codex", [
      "claude",
      "codex",
      "deepseek",
    ]);
  });

  it("成员行显能力 hint（§8.6 公开知识·推荐≠强制）", () => {
    setup();
    fireEvent.click(screen.getByRole("button", { name: /展开/ }));
    expect(screen.getByText("实现 / 测试")).toBeInTheDocument();
    expect(screen.getByText("快搜 / 低成本")).toBeInTheDocument();
  });

  it("不再渲 autonomy 旋钮", () => {
    setup({});
    fireEvent.click(screen.getByRole("button", { name: /展开/ }));
    expect(screen.queryByRole("radiogroup")).toBeNull();
    expect(screen.queryByText("自动程度")).toBeNull();
  });

  it("展开后点 TeamBar 外部 → 面板折叠（panelTitle 消失）", () => {
    setup();
    fireEvent.click(screen.getByRole("button", { name: /展开/ }));
    expect(
      screen.getByText("组队配置 · 会话级（改动粘滞本会话）"),
    ).toBeInTheDocument();
    fireEvent.mouseDown(document.body);
    expect(
      screen.queryByText("组队配置 · 会话级（改动粘滞本会话）"),
    ).not.toBeInTheDocument();
  });
});

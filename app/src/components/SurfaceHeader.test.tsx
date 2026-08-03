import { describe, it, expect, vi } from "vitest";
import { render, screen, fireEvent } from "@testing-library/react";
import { I18nProvider } from "../i18n";
import { SurfaceHeader } from "./SurfaceHeader";

function rptProps() {
  return {
    rightPanelOpen: true,
    rightPanelTab: null,
    rightPanelExpanded: false,
    reviewBadge: 0,
    onTab: vi.fn(),
    onExpand: vi.fn(),
    onUserCollapse: vi.fn(),
    onExpandPanel: vi.fn(),
    onRestorePanel: vi.fn(),
  };
}

function goalProps() {
  return {
    goal: {
      goal: "目标条跑完打勾变绿",
      status: "frozen" as const,
      criteria: [
        {
          id: "c1",
          claim: "a",
          status: "passed" as const,
          scope: "task" as const,
        },
        {
          id: "c2",
          claim: "b",
          status: "pending" as const,
          scope: "task" as const,
        },
      ],
    },
    goalExpanded: false,
    onToggleGoal: vi.fn(),
    goalPanel: <div data-testid="goal-panel" />,
    goalRunComplete: false,
    goalRunHasMemberFailure: false,
    goalRunning: true,
  };
}

describe("SurfaceHeader（阶段1 Task1.4）", () => {
  it("③已启用：session 视图传 goal → topbar 渲出 GoalBar（能查到 goal 文本）", () => {
    const { container } = render(
      <SurfaceHeader
        view="session"
        sidebarCollapsed={false}
        sessionTitle="今天几号"
        status="idle"
        {...rptProps()}
        {...goalProps()}
      />,
    );
    const main = container.querySelector(".sf-head__main")!;
    expect(main.querySelector(".goal-wrap")).not.toBeNull();
    expect(screen.getByText("目标条跑完打勾变绿")).toBeInTheDocument();
  });

  it("Task4：session 视图无 goal → __main 不渲目标条也不渲 SessionContextBar", () => {
    const { container } = render(
      <SurfaceHeader
        view="session"
        sidebarCollapsed={false}
        sessionTitle="今天几号"
        status="idle"
        {...rptProps()}
      />,
    );
    const main = container.querySelector(".sf-head__main")!;
    expect(main.querySelector(".goal-wrap")).toBeNull();
    expect(main.querySelector(".sf-ctx")).toBeNull();
  });

  it("session 视图 scratch goal（criteria=[]）→ topbar 仍渲 GoalBar（goal 文本显示）", () => {
    const { container } = render(
      <SurfaceHeader
        view="session"
        sidebarCollapsed={false}
        {...rptProps()}
        goal={{
          goal: "写 10 个冷笑话",
          status: "frozen",
          criteria: [],
        }}
        goalExpanded={false}
        onToggleGoal={vi.fn()}
        goalPanel={<div />}
      />,
    );
    const main = container.querySelector(".sf-head__main")!;
    expect(main.querySelector(".goal-wrap")).not.toBeNull();
    expect(screen.getByText("写 10 个冷笑话")).toBeInTheDocument();
  });

  it("session 视图 goal 为空目标文本 → 不渲目标条", () => {
    const { container } = render(
      <SurfaceHeader
        view="session"
        sidebarCollapsed={false}
        {...rptProps()}
        goal={{
          goal: "",
          status: "frozen",
          criteria: [],
        }}
        goalExpanded={false}
        onToggleGoal={vi.fn()}
        goalPanel={<div />}
      />,
    );
    expect(container.querySelector(".goal-wrap")).toBeNull();
  });

  it("RightPanelTabs 控件回调透传（收起右面板 → onUserCollapse）", () => {
    const p = rptProps();
    render(
      <SurfaceHeader
        view="session"
        sidebarCollapsed={false}
        sessionTitle="今天几号"
        repoName="ai-digest"
        status="idle"
        {...p}
      />,
    );
    // RightPanelTabs open 态有「收起右面板 ⌘J」按钮（aria-label）
    fireEvent.click(screen.getByLabelText("收起右面板"));
    expect(p.onUserCollapse).toHaveBeenCalledOnce();
  });

  it("previewPath 非空时 preview tab 在切到 Files 后仍保留，可点击切回", () => {
    const p = rptProps();
    render(
      <SurfaceHeader
        view="session"
        sidebarCollapsed={false}
        {...p}
        rightPanelTab="files"
        previewPath="docs/guide/T4.md"
      />,
    );

    fireEvent.click(screen.getByRole("tab", { name: "Preview" }));

    expect(p.onTab).toHaveBeenCalledWith("preview");
  });

  it("停在 Preview 时保留进入前的 Files tab，可直接来回切换", () => {
    const p = rptProps();
    render(
      <SurfaceHeader
        view="session"
        sidebarCollapsed={false}
        {...p}
        rightPanelTab="preview"
        previewPath="docs/guide/T4.md"
        tabBeforePreview="files"
      />,
    );

    expect(screen.getByRole("tab", { name: "Preview" })).toHaveAttribute(
      "aria-selected",
      "true",
    );
    fireEvent.click(screen.getByRole("tab", { name: "Files" }));
    expect(p.onTab).toHaveBeenCalledWith("files");
  });

  it("previewPath 为空时 openTabs 维持只显示当前 tab", () => {
    render(
      <SurfaceHeader
        view="session"
        sidebarCollapsed={false}
        {...rptProps()}
        rightPanelTab="files"
        previewPath={null}
      />,
    );

    expect(screen.getByRole("tab", { name: "Files" })).toBeInTheDocument();
    expect(
      screen.queryByRole("tab", { name: "Preview" }),
    ).not.toBeInTheDocument();
  });

  it("右面板最大化：左段塌缩不渲目标条·tools tabs 接管", () => {
    const p = rptProps();
    const { container } = render(
      <SurfaceHeader
        view="session"
        sidebarCollapsed={false}
        sessionTitle="今天几号"
        repoName="ai-digest"
        status="idle"
        {...p}
        rightPanelExpanded={true}
      />,
    );
    expect(container.querySelector(".sf-head__main .goal-wrap")).toBeNull();
    expect(container.querySelector(".sf-tabs.expanded")).not.toBeNull();
  });

  it("overview 视图：右面板 tabs/控件常驻（去 view gate）", () => {
    const { container } = render(
      <SurfaceHeader
        view="overview"
        sidebarCollapsed={false}
        contextLabel="总览 · acme"
        {...rptProps()}
      />,
    );
    expect(screen.getByText("总览 · acme")).not.toBeNull();
    expect(container.querySelector(".topbar__panel")).not.toBeNull();
    // 非 session 不显 branch 状态条
    expect(container.querySelector(".sf-ctx .br")).toBeNull();
  });

  it("左栏收起：sf-head 加 .inset + 左侧 .sf-collapsed-ctrls（含折叠按钮触发 onToggleSidebar）", () => {
    const onToggle = vi.fn();
    const { container } = render(
      <SurfaceHeader
        view="session"
        sidebarCollapsed={true}
        sessionTitle="今天几号"
        repoName="ai-digest"
        status="idle"
        onToggleSidebar={onToggle}
        {...rptProps()}
      />,
    );
    expect(container.querySelector(".sf-head.inset")).not.toBeNull();
    const ctrls = container.querySelector(".sf-collapsed-ctrls");
    expect(ctrls).not.toBeNull();
    fireEvent.click(screen.getByLabelText("展开会话栏"));
    expect(onToggle).toHaveBeenCalledOnce();
  });

  it("左栏收起态不渲染仓库切换器，只保留展开/后退/前进/总览控件", () => {
    const legacyRepoAnchor = {
      ["collapsed" + "RepoAnchor"]: (
        <div>
          <button type="button" aria-label="仓库切换器">
            web
          </button>
          <div className="repo-switcher" />
        </div>
      ),
    };
    const { container } = render(
      <SurfaceHeader
        {...rptProps()}
        {...legacyRepoAnchor}
        view="session"
        sidebarCollapsed={true}
        onToggleSidebar={() => {}}
        onHome={() => {}}
      />,
    );
    expect(container.querySelector(".sf-collapsed-ctrls")).not.toBeNull();
    expect(screen.queryByLabelText("仓库切换器")).toBeNull();
    expect(container.querySelector(".repo-switcher")).toBeNull();
  });

  it("左栏收起态后退/前进按钮可执行导航回调", () => {
    const onBack = vi.fn();
    const onForward = vi.fn();
    render(
      <SurfaceHeader
        {...rptProps()}
        view="session"
        sidebarCollapsed={true}
        onToggleSidebar={() => {}}
        onHome={() => {}}
        canGoBack={true}
        canGoForward={true}
        onBack={onBack}
        onForward={onForward}
      />,
    );

    fireEvent.click(screen.getByLabelText("后退"));
    fireEvent.click(screen.getByLabelText("前进"));

    expect(onBack).toHaveBeenCalledOnce();
    expect(onForward).toHaveBeenCalledOnce();
  });

  it("左栏收起态没有历史时禁用后退/前进", () => {
    render(
      <SurfaceHeader
        {...rptProps()}
        view="session"
        sidebarCollapsed={true}
        onToggleSidebar={() => {}}
        onHome={() => {}}
        canGoBack={false}
        canGoForward={false}
      />,
    );

    expect(screen.getByLabelText("后退")).toBeDisabled();
    expect(screen.getByLabelText("前进")).toBeDisabled();
  });

  it("Task1：topbar 切两段——左段 .sf-head__main 存在·右段 .sf-head__ctl 含 .sf-tabs", () => {
    const { container } = render(
      <SurfaceHeader
        view="session"
        sidebarCollapsed={false}
        sessionTitle="今天几号"
        repoName="ai-digest"
        status="idle"
        {...rptProps()}
      />,
    );
    const main = container.querySelector(".sf-head__main");
    const ctl = container.querySelector(".sf-head__ctl");
    expect(main).not.toBeNull();
    expect(ctl).not.toBeNull();
    expect(ctl!.querySelector(".sf-tabs")).not.toBeNull();
    expect(
      main!.compareDocumentPosition(ctl!) & Node.DOCUMENT_POSITION_FOLLOWING,
    ).toBeTruthy();
  });

  it("Task1：右面板开（非最大化）→ 右段加 .sf-head__ctl--open（固定列宽 + 竖线）", () => {
    const { container } = render(
      <SurfaceHeader
        view="session"
        sidebarCollapsed={false}
        sessionTitle="x"
        status="idle"
        {...rptProps()}
        rightPanelOpen={true}
        rightPanelExpanded={false}
      />,
    );
    expect(container.querySelector(".sf-head__ctl--open")).not.toBeNull();
    expect(container.querySelector(".sf-head__ctl--max")).toBeNull();
  });

  it("Task1：右面板最大化 → 右段加 .sf-head__ctl--max（接管满宽）", () => {
    const { container } = render(
      <SurfaceHeader
        view="session"
        sidebarCollapsed={false}
        sessionTitle="x"
        status="idle"
        {...rptProps()}
        rightPanelExpanded={true}
      />,
    );
    expect(container.querySelector(".sf-head__ctl--max")).not.toBeNull();
  });

  it("低频语言/通知不进 topbar，统一留在 Settings", () => {
    const { container } = render(
      <I18nProvider initialLocale="zh">
        <SurfaceHeader
          view="overview"
          sidebarCollapsed={false}
          contextLabel="总览 · acme"
          {...rptProps()}
        />
      </I18nProvider>,
    );

    expect(screen.queryByLabelText("切换语言")).toBeNull();
    expect(screen.queryByLabelText("通知")).toBeNull();
    expect(screen.queryByLabelText("更多")).toBeNull();
    expect(container.querySelector(".sf-head__global")).toBeNull();
    expect(container.querySelector(".sf-lang")).toBeNull();
    expect(container.querySelector(".sf-more")).toBeNull();
  });

  it("主区与右面板 tabs 分离，不插入全局低频控件", () => {
    const { container } = render(
      <I18nProvider initialLocale="zh">
        <SurfaceHeader
          view="overview"
          sidebarCollapsed={false}
          contextLabel="总览 · acme"
          {...rptProps()}
        />
      </I18nProvider>,
    );

    const main = container.querySelector(".sf-head__main");
    const tabs = container.querySelector(".sf-tabs");
    expect(main).not.toBeNull();
    expect(tabs).not.toBeNull();
    expect(container.querySelector(".sf-more")).toBeNull();
  });

  it("右面板收起时 header 只保留右面板自己的展开按钮", () => {
    const { container } = render(
      <I18nProvider initialLocale="zh">
        <SurfaceHeader
          view="overview"
          sidebarCollapsed={false}
          contextLabel="总览 · acme"
          {...rptProps()}
          rightPanelOpen={false}
          rightPanelTab={null}
        />
      </I18nProvider>,
    );

    expect(screen.queryByLabelText("更多")).toBeNull();
    expect(screen.getByLabelText("展开右面板")).toBeInTheDocument();
    expect(container.querySelector(".sf-head__ctl--closed")).toBeNull();
  });

  it("右面板最大化时不渲染全局低频控件", () => {
    const { container } = render(
      <I18nProvider initialLocale="zh">
        <SurfaceHeader
          view="overview"
          sidebarCollapsed={false}
          contextLabel="总览 · acme"
          {...rptProps()}
          rightPanelExpanded={true}
        />
      </I18nProvider>,
    );

    expect(screen.queryByLabelText("更多")).toBeNull();
    expect(container.querySelector(".sf-tabs.expanded")).not.toBeNull();
  });

  it("③ goal_title 优先：session + goal 有 goal_title → topbar 显短标题", () => {
    render(
      <SurfaceHeader
        view="session"
        sidebarCollapsed={false}
        {...rptProps()}
        goal={{
          goal: "这是很长很长的完整目标描述文本",
          goal_title: "创建10个冷笑话",
          status: "frozen",
          criteria: [],
        }}
        goalExpanded={false}
        onToggleGoal={vi.fn()}
        goalPanel={<div />}
      />,
    );
    expect(screen.getByText("创建10个冷笑话")).toBeInTheDocument();
    expect(
      screen.queryByText("这是很长很长的完整目标描述文本"),
    ).not.toBeInTheDocument();
  });

  describe("Solo 会话 topbar 标题兜底（无 goal 时显会话标题 + 运行 spinner）", () => {
    it("无 goal + 有标题 → topbar 渲出会话标题文本", () => {
      const { container } = render(
        <SurfaceHeader
          view="session"
          sidebarCollapsed={false}
          sessionTitle="今天几号"
          status="idle"
          {...rptProps()}
        />,
      );
      const main = container.querySelector(".sf-head__main")!;
      expect(main.querySelector(".sf-session-title")).not.toBeNull();
      expect(screen.getByText("今天几号")).toBeInTheDocument();
      expect(main.querySelector(".sf-session-title__spin")).toBeNull();
    });

    it("无 goal + status=working（会话运行中）→ 标题旁渲出 spinner", () => {
      const { container } = render(
        <SurfaceHeader
          view="session"
          sidebarCollapsed={false}
          sessionTitle="今天几号"
          status="working"
          {...rptProps()}
        />,
      );
      expect(container.querySelector(".sf-session-title__spin")).not.toBeNull();
      expect(screen.getByText("今天几号")).toBeInTheDocument();
    });

    it("有 goal → 仍渲 GoalBar，不渲兜底标题（即便 sessionTitle 也传了）", () => {
      const { container } = render(
        <SurfaceHeader
          view="session"
          sidebarCollapsed={false}
          sessionTitle="今天几号"
          status="working"
          {...rptProps()}
          {...goalProps()}
        />,
      );
      expect(container.querySelector(".goal-wrap")).not.toBeNull();
      expect(container.querySelector(".sf-session-title")).toBeNull();
    });

    it("rightPanelExpanded → 兜底标题不渲染", () => {
      const { container } = render(
        <SurfaceHeader
          view="session"
          sidebarCollapsed={false}
          sessionTitle="今天几号"
          status="working"
          {...rptProps()}
          rightPanelExpanded={true}
        />,
      );
      expect(container.querySelector(".sf-session-title")).toBeNull();
    });
  });

  describe("taskbtn — orchestrated task count button", () => {
    it("orchestratedTaskCount=2 + anyRunning → 渲出 .taskbtn、计数、脉冲点、点击触发回调", () => {
      const onOpenTaskList = vi.fn();
      const { container } = render(
        <I18nProvider initialLocale="zh">
          <SurfaceHeader
            view="session"
            sidebarCollapsed={false}
            {...rptProps()}
            orchestratedTaskCount={2}
            orchestratedAnyRunning={true}
            onOpenTaskList={onOpenTaskList}
          />
        </I18nProvider>,
      );
      const btn = container.querySelector(".taskbtn");
      expect(btn).not.toBeNull();
      const ct = container.querySelector(".taskbtn__ct");
      expect(ct?.textContent).toBe("2");
      const dot = container.querySelector(".taskbtn__dot");
      expect(dot).not.toBeNull();
      fireEvent.click(btn!);
      expect(onOpenTaskList).toHaveBeenCalledOnce();
    });

    it("orchestratedTaskCount=0（或不传）→ 无 .taskbtn", () => {
      const { container } = render(
        <I18nProvider initialLocale="zh">
          <SurfaceHeader
            view="session"
            sidebarCollapsed={false}
            {...rptProps()}
            orchestratedTaskCount={0}
          />
        </I18nProvider>,
      );
      expect(container.querySelector(".taskbtn")).toBeNull();
    });

    it("不传 orchestratedTaskCount → 无 .taskbtn", () => {
      const { container } = render(
        <I18nProvider initialLocale="zh">
          <SurfaceHeader
            view="session"
            sidebarCollapsed={false}
            {...rptProps()}
          />
        </I18nProvider>,
      );
      expect(container.querySelector(".taskbtn")).toBeNull();
    });

    it("orchestratedAnyRunning=false → 无 .taskbtn__dot", () => {
      const { container } = render(
        <I18nProvider initialLocale="zh">
          <SurfaceHeader
            view="session"
            sidebarCollapsed={false}
            {...rptProps()}
            orchestratedTaskCount={3}
            orchestratedAnyRunning={false}
          />
        </I18nProvider>,
      );
      expect(container.querySelector(".taskbtn")).not.toBeNull();
      expect(container.querySelector(".taskbtn__dot")).toBeNull();
    });

    it("orchestratedTaskCount>0 → no .goal-bar__view (onView prop removed)", () => {
      const onOpenTaskList = vi.fn();
      const { container } = render(
        <I18nProvider initialLocale="zh">
          <SurfaceHeader
            view="session"
            sidebarCollapsed={false}
            {...rptProps()}
            goal={{
              goal: "测试目标",
              status: "frozen" as const,
              criteria: [],
            }}
            goalExpanded={false}
            onToggleGoal={vi.fn()}
            goalPanel={<div />}
            orchestratedTaskCount={1}
            onOpenTaskList={onOpenTaskList}
          />
        </I18nProvider>,
      );
      expect(container.querySelector(".goal-bar__view")).toBeNull();
    });

    it("orchestratedTaskCount=0 → no .goal-bar__view", () => {
      const { container } = render(
        <I18nProvider initialLocale="zh">
          <SurfaceHeader
            view="session"
            sidebarCollapsed={false}
            {...rptProps()}
            goal={{
              goal: "测试目标",
              status: "frozen" as const,
              criteria: [],
            }}
            goalExpanded={false}
            onToggleGoal={vi.fn()}
            goalPanel={<div />}
            orchestratedTaskCount={0}
          />
        </I18nProvider>,
      );
      expect(container.querySelector(".goal-bar__view")).toBeNull();
    });
  });
});

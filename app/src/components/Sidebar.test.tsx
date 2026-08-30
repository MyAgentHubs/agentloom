import {
  act,
  render,
  screen,
  fireEvent,
  waitFor,
} from "@testing-library/react";
import { useState } from "react";
import { beforeEach, describe, it, expect, vi } from "vitest";
import { I18nProvider, type Locale } from "../i18n";
import { Sidebar } from "./Sidebar";
import type {
  Session,
  RepoMeta,
  GroupMeta,
  NamespaceMeta,
} from "../types/agent";
import { makeSession } from "../test/factories";
import type { UpdaterSnapshot } from "../types/updater";

const { invokeMock, listenMock } = vi.hoisted(() => ({
  invokeMock: vi.fn(),
  listenMock: vi.fn(),
}));

vi.mock("@tauri-apps/api/core", () => ({ invoke: invokeMock }));
vi.mock("@tauri-apps/api/event", () => ({ listen: listenMock }));

import {
  __resetForTests as __resetUpdaterStoreForTests,
  getUpdaterSnapshot,
} from "../lib/updaterStore";

const localNamespace: NamespaceMeta = {
  id: "local",
  kind: "local",
  name: "Local",
  is_builtin: 1,
  last_active_repo_id: "local-default",
  added_at: 0,
  last_used_at: null,
};
const ghNamespace: NamespaceMeta = {
  id: "gh:acme",
  kind: "github_org",
  name: "acme",
  is_builtin: 0,
  last_active_repo_id: "r-a",
  added_at: 0,
  last_used_at: null,
};
const localRepo: RepoMeta = {
  id: "local-default",
  source: "local",
  owner: null,
  name: "Local 默认",
  path: "/tmp",
  status: "active",
  added_at: 0,
  last_used_at: null,
  namespace_id: "local",
};
const repoA: RepoMeta = {
  ...localRepo,
  id: "r-a",
  source: "github",
  owner: "acme",
  name: "web",
  namespace_id: "gh:acme",
};
const repoB: RepoMeta = {
  ...localRepo,
  id: "r-b",
  source: "github",
  owner: "acme",
  name: "api",
  namespace_id: "gh:acme",
};

const sessions: Session[] = [
  makeSession({ id: "s1", title: "会话一" }),
  makeSession({ id: "s2", title: "会话二" }),
];

type SidebarProps = Parameters<typeof Sidebar>[0];

const base: SidebarProps = {
  sessions,
  currentId: "s1",
  busy: false,
  activeMenu: "session",
  activeRepoId: "local-default" as string | null,
  activeNamespace: localNamespace,
  activeRepo: localRepo,
  namespaces: [localNamespace, ghNamespace],
  allRepos: [localRepo, repoA, repoB],
  reposInActiveNs: [localRepo] as RepoMeta[],
  repoGroupExpanded: {} as Record<string, boolean>,
  onToggleRepoGroup: () => {},
  onSelectRepoInNamespace: () => {},
  onManageRepos: () => {},
  newDisabled: false,
  onSelect: () => {},
  onNew: () => {},
  onRequestDelete: () => {},
  onMenuIntro: () => {},
};

function makeSidebarProps(overrides: Partial<SidebarProps> = {}): SidebarProps {
  return {
    ...base,
    onToggleSidebar: vi.fn(),
    onHome: vi.fn(),
    ...overrides,
  };
}

function renderSidebar(overrides: Partial<SidebarProps> = {}) {
  return render(<Sidebar {...makeSidebarProps(overrides)} />);
}

describe("Sidebar · v4 严格保真", () => {
  it("父组件无关 state 更新时隔离重渲，runningSessionIds 引用变化时正常更新", () => {
    const renderProbe = vi.fn();
    const stableProps = makeSidebarProps();
    class ProbedSet extends Set<string> {
      has(value: string) {
        renderProbe();
        return super.has(value);
      }
    }

    function Harness() {
      const [, setUnrelated] = useState(0);
      const [runningSessionIds, setRunningSessionIds] = useState<
        ReadonlySet<string>
      >(() => new ProbedSet(["s1"]));

      return (
        <>
          <button onClick={() => setUnrelated((value) => value + 1)}>
            unrelated
          </button>
          <button
            onClick={() => setRunningSessionIds(new ProbedSet(["s1", "s2"]))}
          >
            running
          </button>
          <Sidebar {...stableProps} runningSessionIds={runningSessionIds} />
        </>
      );
    }

    const { container } = render(<Harness />);
    const initialProbeCount = renderProbe.mock.calls.length;
    expect(initialProbeCount).toBeGreaterThan(0);
    expect(container.querySelectorAll(".sess__dot.run")).toHaveLength(1);

    fireEvent.click(screen.getByRole("button", { name: "unrelated" }));
    expect(renderProbe).toHaveBeenCalledTimes(initialProbeCount);

    fireEvent.click(screen.getByRole("button", { name: "running" }));
    expect(renderProbe.mock.calls.length).toBeGreaterThan(initialProbeCount);
    expect(container.querySelectorAll(".sess__dot.run")).toHaveLength(2);
  });

  it("每分钟刷新会话相对时间，并在卸载时清理 interval", () => {
    vi.useFakeTimers();
    try {
      const nowMs = new Date(2026, 6, 18, 12, 0, 0).getTime();
      vi.setSystemTime(nowMs);
      const { container, unmount } = renderSidebar({
        sessions: [
          makeSession({
            id: "s1",
            title: "会话一",
            created_at: nowMs / 1000 - 59,
          }),
        ],
      });

      expect(container.querySelector(".sess__time")?.textContent).toBe("刚刚");
      act(() => vi.advanceTimersByTime(60_000));
      expect(container.querySelector(".sess__time")?.textContent).toBe(
        "1 分钟",
      );
      unmount();
      expect(vi.getTimerCount()).toBe(0);
    } finally {
      vi.useRealTimers();
    }
  });

  it("1 repo 时平铺 sessions · 不渲染 .sb-repo-grp · 用 v4 .sess + .sess__nm + .sess__dot", () => {
    const { container } = renderSidebar();
    expect(container.querySelector(".sb-repo-grp")).toBeNull();
    expect(container.querySelectorAll(".sess").length).toBe(2);
    expect(screen.getByText("会话一")).toBeInTheDocument();
    expect(container.querySelector(".sess__nm")).not.toBeNull();
    expect(container.querySelector(".sess__dot")).not.toBeNull();
  });

  it("点 .sess 触发 onSelect", () => {
    const onSelect = vi.fn();
    const { container } = renderSidebar({ onSelect });
    fireEvent.click(container.querySelectorAll(".sess")[1]);
    expect(onSelect).toHaveBeenCalledWith("s2");
  });

  it("session 并发 Task 3 · busy 时仍可切换 session", () => {
    const onSelect = vi.fn();
    const { container } = renderSidebar({ busy: true, onSelect });
    fireEvent.click(container.querySelectorAll(".sess")[1]);
    expect(onSelect).toHaveBeenCalledWith("s2");
  });

  it("session 并发 Task 2 · runningSessionIds 同时点亮多个运行会话", () => {
    const { container } = renderSidebar({
      runningSessionIds: new Set(["s1", "s2"]),
    });
    expect(container.querySelectorAll(".sess__dot.run")).toHaveLength(2);
  });

  it("左栏行状态点三态 · sessionStatusById 驱动非当前会话行的 done/attention", () => {
    // currentId="s1"（base）→ s1 是当前打开行·即便 map 里有 done/attention 也不显（切进去即清的兜底）；
    // s2 非当前 → 照 map 值显。
    const { container } = renderSidebar({
      sessionStatusById: new Map([
        ["s1", "done"],
        ["s2", "attention"],
      ]),
    });
    const rows = container.querySelectorAll(".sess");
    const s1Dot = rows[0].querySelector(".sess__dot")!;
    const s2Dot = rows[1].querySelector(".sess__dot")!;
    expect(s1Dot.className).not.toContain("done");
    expect(s1Dot.className).toContain("idle");
    expect(s2Dot.className).toContain("attention");
  });

  it("左栏行状态点三态 · running 优先于 sessionStatusById（即便 map 标 done 也显 running）", () => {
    const { container } = renderSidebar({
      currentId: null,
      runningSessionIds: new Set(["s2"]),
      sessionStatusById: new Map([["s2", "done"]]),
    });
    const rows = container.querySelectorAll(".sess");
    const s2Dot = rows[1].querySelector(".sess__dot")!;
    expect(s2Dot.className).toContain("run");
    expect(s2Dot.className).not.toContain("done");
  });

  it(".sb-grp 含「会话」+ .sb-grp__add「＋ 新会话」 · busy 时仍可新建", () => {
    const { container, rerender } = renderSidebar();
    expect(screen.getByText("会话")).toBeInTheDocument();
    const add = container.querySelector(".sb-grp__add") as HTMLButtonElement;
    expect(add).not.toBeNull();
    expect(add.disabled).toBe(false);
    rerender(<Sidebar {...makeSidebarProps({ busy: true })} />);
    expect(
      (container.querySelector(".sb-grp__add") as HTMLButtonElement).disabled,
    ).toBe(false);
  });

  it("B3 newDisabled=true 时 .sb-grp__add disabled + title 含「请先添加 repo」", () => {
    const { container } = renderSidebar({ newDisabled: true });
    const add = container.querySelector(".sb-grp__add") as HTMLButtonElement;
    expect(add.disabled).toBe(true);
    expect(add.title).toMatch(/请先添加 repo/);
  });

  it(".menu-item「项目简介」+ SVG · activeMenu='intro' 加 .active", () => {
    const { container, rerender } = renderSidebar();
    const menu = container.querySelector(".menu-item");
    expect(menu).not.toBeNull();
    expect(menu!.querySelector("svg")).not.toBeNull();
    expect(menu!.classList.contains("active")).toBe(false);
    rerender(<Sidebar {...makeSidebarProps({ activeMenu: "intro" })} />);
    expect(
      container.querySelector(".menu-item")!.classList.contains("active"),
    ).toBe(true);
  });

  it("点 .menu-item 触发 onMenuIntro", () => {
    const onMenuIntro = vi.fn();
    const { container } = renderSidebar({ onMenuIntro });
    fireEvent.click(container.querySelector(".menu-item")!);
    expect(onMenuIntro).toHaveBeenCalledTimes(1);
  });

  it("N repos 时左侧仍只平铺 activeRepo 的 session · 不渲染 .sb-repo-grp", () => {
    const mixed: Session[] = [
      makeSession({
        id: "sa1",
        title: "active-a1",
        repo_id: "r-a",
        namespace_id: "ns-a",
      }),
      makeSession({
        id: "sa2",
        title: "active-a2",
        repo_id: "r-a",
        namespace_id: "ns-a",
      }),
      makeSession({
        id: "sb1",
        title: "other-b1",
        repo_id: "r-b",
        namespace_id: "ns-a",
      }),
    ];
    const { container } = renderSidebar({
      sessions: mixed,
      activeRepoId: "r-a",
      reposInActiveNs: [repoA, repoB],
      repoGroupExpanded: { "r-a": true },
    });
    expect(container.querySelector(".sb-repo-grp")).toBeNull();
    expect(container.querySelectorAll(".sess")).toHaveLength(2);
    expect(screen.getByText("active-a1")).toBeInTheDocument();
    expect(screen.getByText("active-a2")).toBeInTheDocument();
    expect(screen.queryByText("other-b1")).not.toBeInTheDocument();
  });

  it("N repos 时不渲染 repo header，也不暴露 repo group toggle 入口", () => {
    const onToggleRepoGroup = vi.fn();
    const { container } = renderSidebar({
      activeRepoId: "r-a",
      reposInActiveNs: [repoA, repoB],
      repoGroupExpanded: { "r-a": true },
      onToggleRepoGroup,
    });
    expect(container.querySelector(".sb-repo-head")).toBeNull();
    expect(onToggleRepoGroup).not.toHaveBeenCalled();
  });

  it(".sb-foot 渲染项目切换器 + 设置齿轮", () => {
    const { container } = renderSidebar();
    expect(container.querySelector(".sb-foot")).not.toBeNull();
    expect(screen.getByLabelText("项目切换器")).toHaveTextContent("Local 默认");
    expect(screen.getByRole("button", { name: "设置" })).not.toBeNull();
    expect(container.querySelector(".foot-sys")).toBeNull();
  });

  it("footer 齿轮触发 onMenuAgents", () => {
    const onMenuAgents = vi.fn();
    renderSidebar({ onMenuAgents });

    expect(screen.queryByText("Agent 池")).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "设置" }));

    expect(onMenuAgents).toHaveBeenCalledTimes(1);
  });

  it("footer 项目切换器选择 repo 时透传 onSelectRepoInNamespace", () => {
    const onSelectRepoInNamespace = vi.fn();
    renderSidebar({ onSelectRepoInNamespace });
    fireEvent.click(screen.getByLabelText("项目切换器"));
    fireEvent.click(screen.getByText("web"));
    expect(onSelectRepoInNamespace).toHaveBeenCalledWith("gh:acme", "r-a");
  });

  it("活动列表排除 archived · 已归档区渲染 archived 会话", () => {
    const mixed = [
      makeSession({ id: "a1", title: "活动1", repo_id: "local-default" }),
      makeSession({
        id: "z1",
        title: "归档1",
        repo_id: "local-default",
        archived: true,
        archived_at: 100,
      }),
    ];
    const { container } = renderSidebar({
      sessions: mixed,
      activeNamespaceId: "local",
    });
    // 活动区只 1 个（a1）；归档区 1 个（z1）
    const arch = container.querySelector(".sb-arch")!;
    expect(arch).not.toBeNull();
    expect(arch.textContent).toMatch(/已归档/);
    // 顶层活动区（非 .sb-arch 内）不含 z1
    expect(
      container.querySelector('.sessions__scroll > [data-session-id="z1"]'),
    ).toBeNull();
    // 归档区含 z1
    expect(arch.querySelector('[data-session-id="z1"]')).not.toBeNull();
  });

  it("已归档区默认折叠（body 隐）· 点 head 展开", () => {
    const mixed = [
      makeSession({ id: "z1", title: "归档1", archived: true, archived_at: 1 }),
    ];
    const { container } = renderSidebar({
      sessions: mixed,
      activeNamespaceId: "local",
    });
    const arch = container.querySelector(".sb-arch")!;
    expect(arch.classList.contains("collapsed")).toBe(true);
    fireEvent.click(arch.querySelector(".sb-arch__head")!);
    expect(
      container.querySelector(".sb-arch")!.classList.contains("collapsed"),
    ).toBe(false);
  });

  it("无 archived 会话时不渲染已归档区", () => {
    const { container } = renderSidebar({ activeNamespaceId: "local" });
    expect(container.querySelector(".sb-arch")).toBeNull();
  });

  it("置顶组与非置顶组之间插分隔线 .sb-pin-div", () => {
    const mixed = [
      makeSession({ id: "p1", title: "置顶1", pinned: true }),
      makeSession({ id: "n1", title: "普通1" }),
      makeSession({ id: "n2", title: "普通2" }),
    ];
    const { container } = renderSidebar({
      sessions: mixed,
      activeNamespaceId: "local",
    });
    expect(container.querySelector(".sb-pin-div")).not.toBeNull();
  });

  it("全部非置顶 / 全部置顶 时不插分隔线", () => {
    const allPlain = [
      makeSession({ id: "n1", title: "普通1" }),
      makeSession({ id: "n2", title: "普通2" }),
    ];
    const { container, rerender } = renderSidebar({
      sessions: allPlain,
      activeNamespaceId: "local",
    });
    expect(container.querySelector(".sb-pin-div")).toBeNull();
    const allPinned = [
      makeSession({ id: "p1", title: "置顶1", pinned: true }),
      makeSession({ id: "p2", title: "置顶2", pinned: true }),
    ];
    rerender(
      <Sidebar
        {...makeSidebarProps({
          sessions: allPinned,
          activeNamespaceId: "local",
        })}
      />,
    );
    expect(container.querySelector(".sb-pin-div")).toBeNull();
  });

  it("多 repo namespace 下 sidebar 只渲染 activeRepoId 那一个 repo 的会话（无 .sb-repo-grp）", () => {
    const sessions2 = [
      makeSession({ id: "a", title: "在 web", repo_id: repoA.id }),
      makeSession({ id: "b", title: "在 api", repo_id: repoB.id }),
    ];
    const { container } = renderSidebar({
      sessions: sessions2,
      activeRepoId: repoA.id,
    });
    expect(container.querySelector(".sb-repo-grp")).toBeNull();
    expect(container.querySelector('[data-session-id="a"]')).not.toBeNull();
    expect(container.querySelector('[data-session-id="b"]')).toBeNull();
  });

  it("分组：未分组在上·分组折叠带在下·常驻＋新建分组·已归档最底", () => {
    const mixedSessions = [
      makeSession({ id: "u1", title: "未分组1", group_id: null }),
      makeSession({ id: "g1s", title: "组内", group_id: "gA" }),
    ];
    const groups: GroupMeta[] = [
      {
        id: "gA",
        repo_id: "local-default",
        name: "前端",
        position: 0,
        created_at: 0,
      },
    ];
    const { container } = renderSidebar({
      sessions: mixedSessions,
      groups,
    });
    expect(container.querySelector('[data-session-id="u1"]')).not.toBeNull();
    expect(container.textContent).toContain("前端");
    expect(container.querySelector('[data-action="new-group"]')).not.toBeNull();
  });

  it("renders continuation children directly under root inside the same group", () => {
    const mixedSessions = [
      makeSession({
        id: "tip",
        title: "Tip",
        group_id: "gA",
        parent_session_id: "root",
      }),
      makeSession({ id: "other", title: "Other", group_id: "gA" }),
      makeSession({
        id: "root",
        title: "Root",
        group_id: "gA",
        continued_to_session_id: "tip",
      }),
    ];
    const groups: GroupMeta[] = [
      {
        id: "gA",
        repo_id: "local-default",
        name: "Thread",
        position: 0,
        created_at: 0,
      },
    ];

    const { container } = renderSidebar({ sessions: mixedSessions, groups });
    const ids = Array.from(container.querySelectorAll("[data-session-id]")).map(
      (el) => el.getAttribute("data-session-id"),
    );
    expect(ids.indexOf("tip")).toBe(ids.indexOf("root") + 1);
    expect(container.querySelector('[data-session-id="tip"]')).toHaveClass(
      "sess--child",
    );
  });

  it("renders orphan continuation child under root when parent pointer is missing", () => {
    const mixedSessions = [
      makeSession({ id: "root", title: "Root", group_id: "gA" }),
      makeSession({ id: "other", title: "Other", group_id: "gA" }),
      makeSession({
        id: "tip",
        title: "Tip",
        group_id: "gA",
        parent_session_id: "root",
      }),
    ];
    const groups: GroupMeta[] = [
      {
        id: "gA",
        repo_id: "local-default",
        name: "Thread",
        position: 0,
        created_at: 0,
      },
    ];

    const { container } = renderSidebar({ sessions: mixedSessions, groups });
    const ids = Array.from(container.querySelectorAll("[data-session-id]")).map(
      (el) => el.getAttribute("data-session-id"),
    );
    expect(ids.indexOf("tip")).toBe(ids.indexOf("root") + 1);
  });

  it("labels orphan continuation parent actions as continuation session group operations", () => {
    const mixedSessions = [
      makeSession({ id: "root", title: "Root", group_id: "gA" }),
      makeSession({
        id: "tip",
        title: "Tip",
        group_id: "gA",
        parent_session_id: "root",
      }),
    ];
    const groups: GroupMeta[] = [
      {
        id: "gA",
        repo_id: "local-default",
        name: "Thread",
        position: 0,
        created_at: 0,
      },
    ];

    const { container } = renderSidebar({ sessions: mixedSessions, groups });
    fireEvent.contextMenu(container.querySelector('[data-session-id="root"]')!);
    expect(
      container.querySelector('[data-action="move-to-group"]')!.textContent,
    ).toContain("移动接续会话组");
    expect(
      container.querySelector('[data-action="archive"]')!.textContent,
    ).toContain("归档接续会话组");
  });

  it("空组也渲染（计数 0）", () => {
    const groups: GroupMeta[] = [
      {
        id: "gE",
        repo_id: "local-default",
        name: "空组",
        position: 0,
        created_at: 0,
      },
    ];
    const { container } = renderSidebar({ sessions: [], groups });
    expect(container.textContent).toContain("空组");
  });

  it("点＋新建分组进 inline 输入·提交调 onCreateGroup", () => {
    const onCreateGroup = vi.fn();
    const { container } = renderSidebar({ onCreateGroup });
    fireEvent.click(container.querySelector('[data-action="new-group"]')!);
    const input = container.querySelector(
      ".sb-new-group-input input",
    ) as HTMLInputElement;
    expect(input).not.toBeNull();
    fireEvent.change(input, { target: { value: "调研" } });
    fireEvent.keyDown(input, {
      key: "Enter",
      nativeEvent: { isComposing: false },
    });
    expect(onCreateGroup).toHaveBeenCalledWith("调研");
  });

  it("会话栏右边缘手柄可拖拽调宽并持久化夹在 200 到 360px", () => {
    localStorage.removeItem("agentloom.sidebarWidth");
    const { container } = renderSidebar();
    const handle = screen.getByLabelText("拖拽调整会话栏宽度");
    const sidebar = container.querySelector(".sidebar") as HTMLElement;

    fireEvent.mouseDown(handle, { clientX: 230 });
    fireEvent.mouseMove(window, { clientX: 320 });
    expect(sidebar.style.width).toBe("320px");
    expect(sidebar.style.flexBasis).toBe("320px");

    fireEvent.mouseMove(window, { clientX: 1000 });
    expect(sidebar.style.width).toBe("360px");

    fireEvent.mouseUp(window, { clientX: 320 });
    const stored = localStorage.getItem("agentloom.sidebarWidth");
    expect(stored).not.toBeNull();
    expect(Number(stored)).toBeGreaterThanOrEqual(200);
    expect(Number(stored)).toBeLessThanOrEqual(360);
  });
});

describe("Sidebar · 全高列 sb-top + footer selector（阶段1 Task1.3）", () => {
  it("aside 挂 .sidebar 类（新骨架 CSS 接线）+ 顶部 .sb-top 含折叠/总览/搜索·不渲染 fake 红绿灯", () => {
    const { container } = renderSidebar();
    expect(container.querySelector("aside.sidebar")).not.toBeNull();
    expect(container.querySelector(".sb-top")).not.toBeNull();
    expect(container.querySelector(".sb-top .traffic")).toBeNull();
    expect(screen.getByLabelText("折叠会话栏")).not.toBeNull();
    expect(screen.getByRole("button", { name: "总览" })).not.toBeNull();
  });

  it("点折叠触发 onToggleSidebar", () => {
    const onToggle = vi.fn();
    renderSidebar({ onToggleSidebar: onToggle });
    fireEvent.click(screen.getByLabelText("折叠会话栏"));
    expect(onToggle).toHaveBeenCalledOnce();
  });

  it("点总览触发 onHome（删 TopBar 后总览入口归位·不能丢·既有 App.test 断言 getByRole/Label「总览」）", () => {
    const onHome = vi.fn();
    renderSidebar({ onHome });
    fireEvent.click(screen.getByRole("button", { name: "总览" }));
    expect(onHome).toHaveBeenCalledOnce();
  });

  it("搜索功能未实现时按钮置灰且不可点击", () => {
    renderSidebar();
    expect(screen.getByRole("button", { name: "搜索" })).toBeDisabled();
  });

  it(".sb-foot 保留项目切换器和设置齿轮（设置齿轮仍触发 onMenuAgents）", () => {
    const onMenuAgents = vi.fn();
    const { container } = renderSidebar({ onMenuAgents });
    expect(container.querySelector(".sb-foot .foot-repo")).toBeNull();
    expect(
      container.querySelector(".sb-foot .project-switcher"),
    ).not.toBeNull();
    fireEvent.click(screen.getByLabelText("设置"));
    expect(onMenuAgents).toHaveBeenCalledOnce();
  });
});

describe("UpdateButton — 左侧栏 footer 常驻胶囊", () => {
  let eventHandler: ((event: { payload: unknown }) => void) | null;
  // 按命令名路由的响应表——不用 mockResolvedValueOnce 位置队列：
  // `I18nProvider` 自己也会在挂载时 `invoke("set_ui_locale", …)`，与
  // `UpdateButton` 的 `updater_get_state` 调用谁先谁后不是稳定顺序（前者是同步
  // 调用、后者要等 `listen()` 先 resolve 一次微任务），位置队列会被错配。
  const responses = new Map<string, unknown>();

  function snap(
    revision: number,
    kind: string,
    extra: object = {},
  ): UpdaterSnapshot {
    return { revision, state: { kind, ...extra } } as UpdaterSnapshot;
  }

  function emit(payload: unknown) {
    eventHandler?.({ payload });
  }

  function renderSidebarWithUpdater(locale: Locale = "zh") {
    return render(
      <I18nProvider initialLocale={locale}>
        <Sidebar {...makeSidebarProps()} />
      </I18nProvider>,
    );
  }

  beforeEach(() => {
    __resetUpdaterStoreForTests();
    eventHandler = null;
    responses.clear();
    responses.set("updater_get_state", snap(0, "idle"));
    invokeMock.mockReset();
    listenMock.mockReset();
    listenMock.mockImplementation(
      async (_channel: string, handler: typeof eventHandler) => {
        eventHandler = handler;
        return vi.fn();
      },
    );
    invokeMock.mockImplementation(async (cmd: string) => responses.get(cmd));
  });

  it("idle/checking/up_to_date 等非更新态 → 不渲染 footer 胶囊", async () => {
    for (const state of [
      snap(1, "idle"),
      snap(1, "checking"),
      snap(1, "up_to_date", { checked_at: Date.now() }),
      snap(1, "staging"),
      snap(1, "swapping"),
      snap(1, "disabled", { reason: "dev" }),
    ]) {
      // 每轮重置模块级单例——否则 start() 幂等短路，第二轮起不会再调 invoke。
      __resetUpdaterStoreForTests();
      responses.set("updater_get_state", state);
      const { container, unmount } = renderSidebarWithUpdater();
      // 等真正的迁移落地（revision 从 0 推进到 1），而不是只等 invoke 被调用过
      // ——否则哪怕 invoke 的返回值从没被应用，这条断言也会假阳性通过。
      await waitFor(() => expect(getUpdaterSnapshot().revision).toBe(1));
      expect(container.querySelector(".updbtn")).toBeNull();
      unmount();
    }
  });

  it("up_to_date（已是最新）→ footer 不渲染胶囊", async () => {
    responses.set(
      "updater_get_state",
      snap(1, "up_to_date", { checked_at: Date.now() }),
    );
    const { container } = renderSidebarWithUpdater();
    await waitFor(() => expect(getUpdaterSnapshot().revision).toBe(1));
    expect(container.querySelector(".sb-foot > .updbtn")).toBeNull();
  });

  it("自动检查失败落回 idle → footer 胶囊仍不渲染", async () => {
    responses.set("updater_get_state", snap(1, "checking"));
    const { container } = renderSidebarWithUpdater();
    await waitFor(() => expect(getUpdaterSnapshot().revision).toBe(1));
    expect(container.querySelector(".sb-foot > .updbtn")).toBeNull();

    act(() => emit(snap(2, "idle")));
    expect(getUpdaterSnapshot().revision).toBe(2);
    expect(container.querySelector(".sb-foot > .updbtn")).toBeNull();
  });

  it.each([
    [
      "zh",
      'AL_ERR:updater.swap_failed:{"detail":"extract archive failed: corrupt payload"}',
      "更新失败 · 重试",
      "版本切换失败：extract archive failed: corrupt payload",
    ],
    [
      "en",
      'AL_ERR:updater.swap_failed:{"detail":"signature verification failed: bad signature"}',
      "Update failed · Retry",
      "Version swap failed: signature verification failed: bad signature",
    ],
  ])(
    "%s Error 态 → 渲染失败胶囊，tooltip 保留完整错误",
    async (locale, msg, label, tooltip) => {
      responses.set(
        "updater_get_state",
        snap(1, "error", { msg, checked_at: Date.now() }),
      );
      const { container } = renderSidebarWithUpdater(locale as Locale);
      const pill = await screen.findByRole("button", { name: label });
      expect(pill).toHaveAttribute("title", tooltip);
      expect(pill).toHaveClass("updbtn--error");
      expect(container.querySelector(".sb-foot > .updbtn")).toBe(pill);
    },
  );

  it("Error 态点击 → 手动检查发现更新后立即继续下载，且命令顺序正确", async () => {
    responses.set(
      "updater_get_state",
      snap(1, "error", { msg: "extract failed", checked_at: Date.now() }),
    );
    responses.set(
      "updater_check",
      snap(2, "available", {
        version: "0.3.0",
        notes: null,
        pub_date: null,
      }),
    );
    responses.set(
      "updater_download_and_install",
      snap(3, "downloading", { downloaded: 0, total: null }),
    );
    renderSidebarWithUpdater();
    const retry = await screen.findByRole("button", {
      name: "更新失败 · 重试",
    });

    fireEvent.click(retry);

    await waitFor(() =>
      expect(screen.getByRole("button", { name: "下载中…" })).toBeDisabled(),
    );
    const updaterCommands = invokeMock.mock.calls
      .map(([command]) => command)
      .filter((command) =>
        ["updater_check", "updater_download_and_install"].includes(command),
      );
    expect(invokeMock).toHaveBeenCalledWith("updater_check", { manual: true });
    expect(updaterCommands).toEqual([
      "updater_check",
      "updater_download_and_install",
    ]);
  });

  it("Error(reopen) 点击 → 只调用 updater_reopen，不检查也不下载", async () => {
    responses.set(
      "updater_get_state",
      snap(1, "error", {
        msg: "AL_ERR:updater.relaunch_failed",
        checked_at: Date.now(),
        retry: "reopen",
      }),
    );
    responses.set("updater_reopen", undefined);
    renderSidebarWithUpdater();
    const retry = await screen.findByRole("button", {
      name: "更新失败 · 重试",
    });

    fireEvent.click(retry);

    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("updater_reopen"),
    );
    expect(invokeMock).not.toHaveBeenCalledWith("updater_check", {
      manual: true,
    });
    expect(invokeMock).not.toHaveBeenCalledWith("updater_download_and_install");
  });

  it("Error 缺省 retry 点击 → 仍调用 updater_check，不误走 reopen", async () => {
    responses.set(
      "updater_get_state",
      snap(1, "error", { msg: "extract failed", checked_at: Date.now() }),
    );
    responses.set(
      "updater_check",
      snap(2, "up_to_date", { checked_at: Date.now() }),
    );
    renderSidebarWithUpdater();
    const retry = await screen.findByRole("button", {
      name: "更新失败 · 重试",
    });

    fireEvent.click(retry);

    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("updater_check", {
        manual: true,
      }),
    );
    expect(invokeMock).not.toHaveBeenCalledWith("updater_reopen");
  });

  it("Error 态点击 → 手动检查已是最新时不下载，胶囊消失", async () => {
    responses.set(
      "updater_get_state",
      snap(1, "error", { msg: "extract failed", checked_at: Date.now() }),
    );
    responses.set(
      "updater_check",
      snap(2, "up_to_date", { checked_at: Date.now() }),
    );
    const { container } = renderSidebarWithUpdater();
    const retry = await screen.findByRole("button", {
      name: "更新失败 · 重试",
    });

    fireEvent.click(retry);

    await waitFor(() =>
      expect(container.querySelector(".sb-foot > .updbtn")).toBeNull(),
    );
    expect(invokeMock).toHaveBeenCalledWith("updater_check", { manual: true });
    expect(invokeMock).not.toHaveBeenCalledWith("updater_download_and_install");
  });

  it.each([
    [
      "zh",
      snap(1, "available", {
        version: "0.3.0",
        notes: null,
        pub_date: null,
      }),
      "更新到 v0.3.0",
    ],
    [
      "en",
      snap(1, "available", {
        version: "0.3.0",
        notes: null,
        pub_date: null,
      }),
      "Update to v0.3.0",
    ],
    ["zh", snap(1, "downloading", { downloaded: 19, total: 20 }), "下载中 95%"],
    [
      "en",
      snap(1, "downloading", { downloaded: 19, total: 20 }),
      "Downloading 95%",
    ],
    ["zh", snap(1, "downloading", { downloaded: 19, total: null }), "下载中…"],
    [
      "en",
      snap(1, "downloading", { downloaded: 19, total: null }),
      "Downloading…",
    ],
    [
      "zh",
      snap(1, "ready", { version: "0.3.0", staged_path: "/tmp/x.app" }),
      "重启以更新",
    ],
    [
      "en",
      snap(1, "ready", { version: "0.3.0", staged_path: "/tmp/x.app" }),
      "Restart to update",
    ],
    [
      "zh",
      snap(1, "ready", {
        version: "0.3.0",
        staged_path: "/tmp/x.app",
        last_error: "AL_ERR:updater.swap_failed",
      }),
      "更新失败 · 重试",
    ],
    [
      "en",
      snap(1, "ready", {
        version: "0.3.0",
        staged_path: "/tmp/x.app",
        last_error: "AL_ERR:updater.swap_failed",
      }),
      "Update failed · Retry",
    ],
  ])("%s 标签：%s → %s", async (locale, state, label) => {
    responses.set("updater_get_state", state);
    renderSidebarWithUpdater(locale as Locale);
    await waitFor(() =>
      expect(screen.getByRole("button", { name: label })).toBeInTheDocument(),
    );
  });

  it("available → 胶囊点击直接下载，不弹浮层；随后显示不可点击的下载进度", async () => {
    responses.set(
      "updater_get_state",
      snap(1, "available", {
        version: "0.3.0",
        notes: "修了一些 bug",
        pub_date: null,
      }),
    );
    const { container } = renderSidebarWithUpdater();
    await waitFor(() =>
      expect(container.querySelector(".updbtn")).not.toBeNull(),
    );
    expect(container.querySelector(".sb-foot > .updbtn")).toHaveTextContent(
      "更新到 v0.3.0",
    );
    expect(
      container.querySelector(".sb-foot > .updbtn + .project-switcher"),
    ).not.toBeNull();
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
    expect(screen.queryByText("修了一些 bug")).not.toBeInTheDocument();

    responses.set(
      "updater_download_and_install",
      snap(2, "downloading", { downloaded: 0, total: null }),
    );
    fireEvent.click(screen.getByRole("button", { name: "更新到 v0.3.0" }));
    expect(invokeMock).toHaveBeenCalledWith("updater_download_and_install");
    await waitFor(() => {
      expect(container.querySelector(".updbtn__ring")).not.toBeNull();
      expect(screen.getByRole("button", { name: "下载中…" })).toBeDisabled();
    });
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
  });

  it("available → 初次出现与版本事件更新都只更新胶囊，不自动弹层", async () => {
    responses.set(
      "updater_get_state",
      snap(1, "available", { version: "0.3.0", notes: null, pub_date: null }),
    );
    renderSidebarWithUpdater();
    await waitFor(() =>
      expect(
        screen.getByRole("button", { name: "更新到 v0.3.0" }),
      ).toBeInTheDocument(),
    );
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();

    act(() => {
      emit(
        snap(2, "available", {
          version: "0.4.0",
          notes: null,
          pub_date: null,
        }),
      );
    });
    expect(
      screen.getByRole("button", { name: "更新到 v0.4.0" }),
    ).toBeInTheDocument();
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
  });

  it("downloading → 胶囊不可点，进度百分比随 updater 事件更新", async () => {
    responses.set(
      "updater_get_state",
      snap(1, "downloading", { downloaded: 10, total: 100 }),
    );
    const { container } = renderSidebarWithUpdater();
    await waitFor(() =>
      expect(container.querySelector(".updbtn")).not.toBeNull(),
    );
    expect(container.querySelector(".updbtn__ring")).not.toBeNull();
    expect(screen.getByRole("button", { name: "下载中 10%" })).toBeDisabled();
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();

    act(() => {
      emit(snap(2, "downloading", { downloaded: 73, total: 100 }));
    });
    expect(screen.getByRole("button", { name: "下载中 73%" })).toBeDisabled();
  });

  it("downloading 无总量 → 显示不定进度且胶囊不可点", async () => {
    responses.set(
      "updater_get_state",
      snap(1, "downloading", { downloaded: 19, total: null }),
    );
    const { container } = renderSidebarWithUpdater();
    const pill = await screen.findByRole("button", { name: "下载中…" });
    expect(pill).toBeDisabled();
    expect(container.querySelector(".updbtn__ring--spin")).not.toBeNull();
  });

  it("ready → 点击胶囊直接调用 updater_relaunch，全程无浮层", async () => {
    responses.set(
      "updater_get_state",
      snap(1, "ready", { version: "0.3.0", staged_path: "/tmp/x.app" }),
    );
    const { container } = renderSidebarWithUpdater();
    await waitFor(() =>
      expect(container.querySelector(".updbtn")).not.toBeNull(),
    );
    expect(
      screen.getByRole("button", { name: "重启以更新" }),
    ).toBeInTheDocument();
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "重启以更新" }));
    expect(invokeMock).toHaveBeenCalledWith("updater_relaunch");
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
  });

  it("ready + last_error → 点击胶囊重跑交换，不调用下载命令", async () => {
    responses.set(
      "updater_get_state",
      snap(1, "ready", {
        version: "0.3.0",
        staged_path: "/tmp/x.app",
        last_error: "AL_ERR:updater.swap_failed",
      }),
    );
    renderSidebarWithUpdater();
    const retry = await screen.findByRole("button", {
      name: "更新失败 · 重试",
    });

    fireEvent.click(retry);

    expect(invokeMock).toHaveBeenCalledWith("updater_relaunch");
    expect(invokeMock).not.toHaveBeenCalledWith("updater_download_and_install");
  });

  it("ready 不带 last_error → 胶囊保持正常文案，不渲染错误或重试提示", async () => {
    responses.set(
      "updater_get_state",
      snap(1, "ready", { version: "0.3.0", staged_path: "/tmp/x.app" }),
    );
    renderSidebarWithUpdater();
    const pill = await screen.findByRole("button", { name: "重启以更新" });
    expect(pill).toHaveAttribute("title", "重启以更新");
    expect(
      screen.queryByRole("button", { name: "更新失败 · 重试" }),
    ).not.toBeInTheDocument();
    expect(screen.queryByText(/上次更新未完成/)).not.toBeInTheDocument();
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
  });

  it("ready 重启失败 → 胶囊就地变为可重试，tooltip 显示完整错误", async () => {
    responses.set(
      "updater_get_state",
      snap(1, "ready", { version: "0.3.0", staged_path: "/tmp/x.app" }),
    );
    const { container } = renderSidebarWithUpdater();
    await waitFor(() =>
      expect(container.querySelector(".updbtn")).not.toBeNull(),
    );
    invokeMock.mockRejectedValueOnce("AL_ERR:updater.relaunch_not_ready");
    fireEvent.click(screen.getByRole("button", { name: "重启以更新" }));
    const retry = await screen.findByRole("button", {
      name: "更新失败 · 重试",
    });
    expect(retry).toHaveAttribute("title", "重启安装尚未就绪，请稍后重试");
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();

    invokeMock.mockResolvedValueOnce(undefined);
    fireEvent.click(retry);
    expect(invokeMock).toHaveBeenLastCalledWith("updater_relaunch");
    expect(
      invokeMock.mock.calls.filter(
        ([command]) => command === "updater_relaunch",
      ),
    ).toHaveLength(2);
    expect(invokeMock).not.toHaveBeenCalledWith("updater_download_and_install");
  });

  it("ready 带 last_error → 失败胶囊 tooltip 显示完整错误，点击直接重跑交换", async () => {
    responses.set(
      "updater_get_state",
      snap(1, "ready", {
        version: "0.3.0",
        staged_path: "/tmp/x.app",
        last_error:
          'AL_ERR:updater.swap_failed:{"detail":"renameatx_np(RENAME_SWAP) failed: Operation not permitted (os error 1)"}',
      }),
    );
    renderSidebarWithUpdater();
    const retry = await screen.findByRole("button", {
      name: "更新失败 · 重试",
    });
    expect(retry).toHaveAttribute(
      "title",
      "版本切换失败：renameatx_np(RENAME_SWAP) failed: Operation not permitted (os error 1)",
    );
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();

    fireEvent.click(retry);
    expect(invokeMock).toHaveBeenCalledWith("updater_relaunch");
    expect(invokeMock).not.toHaveBeenCalledWith("updater_download_and_install");
  });

  it("recovery_offered 带 last_error → 胶囊 tooltip 与确认层均显示完整错误", async () => {
    responses.set(
      "updater_get_state",
      snap(1, "recovery_offered", {
        bundle_path: "/Applications/AgentLoom.app",
        staged_path: "/Applications/.agentloom-update-abc123/AgentLoom.app",
        target_version: "0.3.0",
        last_error:
          'AL_ERR:updater.swap_failed:{"detail":"renameatx_np(RENAME_SWAP) failed: Operation not permitted (os error 1)"}',
      }),
    );
    const { container } = renderSidebarWithUpdater();
    await waitFor(() =>
      expect(container.querySelector(".updbtn")).not.toBeNull(),
    );
    const pill = screen.getByRole("button", { name: "更新失败 · 重试" });
    expect(pill).toHaveAttribute(
      "title",
      "版本切换失败：renameatx_np(RENAME_SWAP) failed: Operation not permitted (os error 1)",
    );
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();

    fireEvent.click(pill);
    expect(
      screen.getByText(
        "版本切换失败：renameatx_np(RENAME_SWAP) failed: Operation not permitted (os error 1)",
      ),
    ).toBeInTheDocument();
    const swapBackBtn = screen.getByRole("button", {
      name: "恢复旧版本并重启",
    });
    expect(swapBackBtn).not.toBeDisabled();
  });

  it("recovery_offered → 点击后先确认，确认后才调用 updater_swap_back", async () => {
    responses.set(
      "updater_get_state",
      snap(1, "recovery_offered", {
        bundle_path: "/Applications/AgentLoom.app",
        staged_path: "/Applications/.agentloom-update-abc123/AgentLoom.app",
        target_version: "0.3.0",
      }),
    );
    const { container } = renderSidebarWithUpdater();
    await waitFor(() =>
      expect(container.querySelector(".updbtn")).not.toBeNull(),
    );
    expect(container.querySelector(".updbtn--warn")).not.toBeNull();
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
    expect(invokeMock).not.toHaveBeenCalledWith("updater_swap_back");

    fireEvent.click(container.querySelector(".updbtn")!);
    expect(screen.getByRole("dialog")).toBeInTheDocument();
    expect(screen.getByText(/正在运行备份中的旧版/)).toBeInTheDocument();
    expect(screen.getByText(/v0\.3\.0 安装后未能正常启动/)).toBeInTheDocument();
    const swapBackBtn = screen.getByRole("button", {
      name: "恢复旧版本并重启",
    });
    expect(swapBackBtn).not.toHaveTextContent("0.3.0");

    responses.set("updater_swap_back", undefined);
    fireEvent.click(swapBackBtn);
    expect(invokeMock).toHaveBeenCalledWith("updater_swap_back");
  });

  it("recovery_offered → 确认换回失败时在确认层显示完整错误", async () => {
    responses.set(
      "updater_get_state",
      snap(1, "recovery_offered", {
        bundle_path: "/Applications/AgentLoom.app",
        staged_path: "/Applications/.agentloom-update-abc123/AgentLoom.app",
        target_version: "0.3.0",
      }),
    );
    const { container } = renderSidebarWithUpdater();
    await waitFor(() =>
      expect(container.querySelector(".updbtn")).not.toBeNull(),
    );
    fireEvent.click(container.querySelector(".updbtn")!);

    invokeMock.mockRejectedValueOnce(
      'AL_ERR:updater.swap_failed:{"detail":"renameatx_np failed"}',
    );
    fireEvent.click(screen.getByRole("button", { name: "恢复旧版本并重启" }));
    expect(
      await screen.findByText("版本切换失败：renameatx_np failed"),
    ).toBeInTheDocument();
  });

  it("recovery_offered → Esc 关闭确认层（胶囊仍在）", async () => {
    responses.set(
      "updater_get_state",
      snap(1, "recovery_offered", {
        bundle_path: "/Applications/AgentLoom.app",
        staged_path: "/Applications/.agentloom-update-abc123/AgentLoom.app",
        target_version: "0.3.0",
      }),
    );
    const { container } = renderSidebarWithUpdater();
    await waitFor(() =>
      expect(container.querySelector(".updbtn")).not.toBeNull(),
    );
    fireEvent.click(container.querySelector(".updbtn")!);
    expect(screen.getByRole("dialog")).toBeInTheDocument();

    fireEvent.keyDown(document, { key: "Escape" });
    await waitFor(() =>
      expect(screen.queryByRole("dialog")).not.toBeInTheDocument(),
    );
    expect(container.querySelector(".updbtn")).not.toBeNull();
  });
});

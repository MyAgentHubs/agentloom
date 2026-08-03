import {
  render,
  screen,
  fireEvent,
  waitFor,
  within,
} from "@testing-library/react";
import { describe, it, expect, vi, beforeEach } from "vitest";
import { OverviewHome } from "./OverviewHome";
import type { RepoMeta } from "../types/agent";

declare const process: { env: { VITEST_DEFER_INVOKE?: string } };

const { invokeMock } = vi.hoisted(() => ({ invokeMock: vi.fn() }));

// VITEST_DEFER_INVOKE=1 makes every invoke settle one macrotask later, which
// deterministically exposes assertions that read state landing from a *different*
// async source than the one they awaited. CI runners are ~12x slower than a dev
// machine and lose those races for real; this switch reproduces it on purpose.
function __deferInvoke<T>(p: T): T | Promise<Awaited<T>> {
  return process.env.VITEST_DEFER_INVOKE
    ? new Promise((r) => setTimeout(r, 0)).then(() => p as Promise<Awaited<T>>)
    : p;
}

vi.mock("@tauri-apps/api/core", () => ({
  invoke: (cmd: string, args?: unknown) => __deferInvoke(invokeMock(cmd, args)),
}));

const repoA: RepoMeta = {
  id: "r1",
  source: "github",
  owner: "acme",
  name: "web",
  path: "/tmp/web",
  status: "active",
  added_at: 1,
  last_used_at: null,
  namespace_id: "local",
};

const repoB: RepoMeta = {
  id: "r2",
  source: "github",
  owner: "acme",
  name: "docs",
  path: "/tmp/docs",
  status: "active",
  added_at: 1,
  last_used_at: null,
  namespace_id: "local",
};

function localIsoDay(offsetDays = 0): string {
  const date = new Date();
  date.setDate(date.getDate() + offsetDays);
  return [
    date.getFullYear(),
    String(date.getMonth() + 1).padStart(2, "0"),
    String(date.getDate()).padStart(2, "0"),
  ].join("-");
}

describe("OverviewHome", () => {
  beforeEach(() => {
    invokeMock.mockReset();
    // 默认：最近活动查询返回空数组（多数用例不关心这块，走空态分支即可）。
    invokeMock.mockResolvedValue([]);
  });

  it("显示「总览」标题", async () => {
    render(<OverviewHome sessions={[]} onOpen={() => {}} />);
    expect(screen.getByRole("heading", { name: "总览" })).toBeInTheDocument();
    // 等最近活动的自取数落地（默认 mock 返回 []），避免卸载后异步 setState 溅出 act 警告。
    await screen.findByText("最近还没有改动记录。");
  });

  it("总览根节点包含直接内层容器，且标题位于内层容器中", async () => {
    render(<OverviewHome sessions={[]} onOpen={() => {}} />);

    const overview = document.querySelector(".overview");
    const inner = document.querySelector(".overview__inner");
    const title = screen.getByRole("heading", { name: "总览" });

    expect(overview).toBeInTheDocument();
    expect(inner).toBeInTheDocument();
    expect(inner?.parentElement).toBe(overview);
    expect(inner).toContainElement(title);
    await screen.findByText("最近还没有改动记录。");
  });

  it("无会话时显示空状态提示", async () => {
    render(<OverviewHome sessions={[]} onOpen={() => {}} />);
    expect(
      screen.getByText(
        "还没有会话。用左下项目切换器选一个项目，开始第一条会话。",
      ),
    ).toBeInTheDocument();
    await screen.findByText("最近还没有改动记录。");
  });

  it("有会话时按需注意 / 运行中 / 闲置分组", async () => {
    render(
      <OverviewHome
        sessions={[
          { id: "s1", title: "修 typecheck", unread: true, repo_id: "r1" },
          { id: "s2", title: "实现 returns", repo_id: "r1" },
          { id: "s3", title: "整理 README", repo_id: "r1" },
        ]}
        repos={[repoA]}
        runningSessionIds={new Set(["s2"])}
        onOpen={() => {}}
      />,
    );
    expect(screen.getByLabelText("需注意")).toBeInTheDocument();
    expect(screen.getByLabelText("运行中")).toBeInTheDocument();
    expect(screen.getByLabelText("闲置")).toBeInTheDocument();
    expect(screen.getByText("修 typecheck")).toBeInTheDocument();
    expect(screen.getByText("实现 returns")).toBeInTheDocument();
    expect(screen.getByText("整理 README")).toBeInTheDocument();
    expect(screen.getAllByText("acme/web").length).toBeGreaterThan(0);
    expect(
      screen.getByLabelText("1 个会话需要你 · 1 个在跑 · 跨 1 个仓库"),
    ).toBeInTheDocument();
    expect(screen.queryByText(/还没有会话/)).not.toBeInTheDocument();
    await screen.findByText("最近还没有改动记录。");
  });

  it("闲置超过 6 条时默认显示 6 条，展开后显示全部并可收起", async () => {
    const sessions = Array.from({ length: 8 }, (_, index) => ({
      id: `s${index + 1}`,
      title: `会话 ${index + 1}`,
      created_at: 8 - index,
    }));

    render(<OverviewHome sessions={sessions} onOpen={() => {}} />);

    const idleSection = screen.getByLabelText("闲置");
    expect(idleSection.querySelectorAll(".overview__sess")).toHaveLength(6);
    expect(within(idleSection).queryByText("会话 7")).not.toBeInTheDocument();

    fireEvent.click(
      within(idleSection).getByRole("button", { name: "展开其余 2 条" }),
    );
    expect(idleSection.querySelectorAll(".overview__sess")).toHaveLength(8);
    expect(within(idleSection).getByText("会话 7")).toBeInTheDocument();

    fireEvent.click(within(idleSection).getByRole("button", { name: "收起" }));
    expect(idleSection.querySelectorAll(".overview__sess")).toHaveLength(6);
    await screen.findByText("最近还没有改动记录。");
  });

  it("空分组不渲染，且一切正常只在需注意和运行中都为空时出现", async () => {
    const { rerender } = render(
      <OverviewHome
        sessions={[{ id: "s1", title: "闲置会话" }]}
        onOpen={() => {}}
      />,
    );

    expect(screen.queryByLabelText("需注意")).not.toBeInTheDocument();
    expect(screen.queryByLabelText("运行中")).not.toBeInTheDocument();
    expect(screen.getByLabelText("闲置")).toBeInTheDocument();
    expect(screen.getByText("一切正常，没有会话在等你。")).toBeInTheDocument();

    rerender(
      <OverviewHome
        sessions={[{ id: "s2", title: "待处理会话", unread: true }]}
        onOpen={() => {}}
      />,
    );

    expect(screen.getByLabelText("需注意")).toBeInTheDocument();
    expect(screen.queryByLabelText("运行中")).not.toBeInTheDocument();
    expect(screen.queryByLabelText("闲置")).not.toBeInTheDocument();
    expect(
      screen.queryByText("一切正常，没有会话在等你。"),
    ).not.toBeInTheDocument();
    await screen.findByText("最近还没有改动记录。");
  });

  it("摘要中的 0 使用中性色类，不带警示类", async () => {
    render(
      <OverviewHome
        sessions={[{ id: "s1", title: "闲置会话" }]}
        onOpen={() => {}}
      />,
    );

    const summary = screen.getByLabelText(
      "0 个会话需要你 · 0 个在跑 · 跨 1 个仓库",
    );
    const counts = summary.querySelectorAll(".overview__summarycount");
    expect(counts).toHaveLength(2);
    counts.forEach((count) => {
      expect(count).not.toHaveClass("overview__summarycount--active");
    });
    await screen.findByText("最近还没有改动记录。");
  });

  it("点会话触发 onOpen(id)", async () => {
    const onOpen = vi.fn();
    render(
      <OverviewHome
        sessions={[{ id: "s1", title: "修 typecheck" }]}
        onOpen={onOpen}
      />,
    );
    fireEvent.click(screen.getByText("修 typecheck"));
    expect(onOpen).toHaveBeenCalledWith("s1");
    await screen.findByText("最近还没有改动记录。");
  });

  it("不再展示已删除的 Daily AI News / 前一天工作总结 占位卡", async () => {
    render(<OverviewHome sessions={[]} onOpen={() => {}} />);
    expect(screen.queryByText(/Daily AI News/)).not.toBeInTheDocument();
    expect(screen.queryByText(/前一天工作总结/)).not.toBeInTheDocument();
    await screen.findByText("最近还没有改动记录。");
  });

  describe("最近活动", () => {
    it("加载成功且有数据时按天渲染改动次数 / 增删行数 / 失败数", async () => {
      invokeMock.mockImplementation((cmd: string) => {
        if (cmd === "recent_activity") {
          return Promise.resolve([
            {
              date: localIsoDay(-1),
              commits: 3,
              files_changed: 5,
              insertions: 42,
              deletions: 7,
              failed: 1,
            },
          ]);
        }
        return Promise.resolve(undefined);
      });

      render(<OverviewHome sessions={[]} onOpen={() => {}} />);

      await waitFor(() => {
        expect(screen.getByText("3 次改动")).toBeInTheDocument();
      });
      expect(screen.getByText("+42")).toBeInTheDocument();
      expect(screen.getByText("−7")).toBeInTheDocument();
      expect(screen.getByText("1 次失败")).toBeInTheDocument();
    });

    it("加载成功但无数据时显示人话空态", async () => {
      invokeMock.mockImplementation((cmd: string) => {
        if (cmd === "recent_activity") return Promise.resolve([]);
        return Promise.resolve(undefined);
      });

      render(<OverviewHome sessions={[]} onOpen={() => {}} />);

      await waitFor(() => {
        expect(screen.getByText("最近还没有改动记录。")).toBeInTheDocument();
      });
    });

    it("加载失败时兜底显示人话提示，不炸页面", async () => {
      invokeMock.mockImplementation((cmd: string) => {
        if (cmd === "recent_activity") return Promise.reject(new Error("boom"));
        return Promise.resolve(undefined);
      });

      render(<OverviewHome sessions={[]} onOpen={() => {}} />);

      await waitFor(() => {
        expect(
          screen.getByText("最近活动加载失败，请稍后再试。"),
        ).toBeInTheDocument();
      });
      // 页面其余部分仍然正常渲染，没有被这块失败拖垮
      expect(screen.getByRole("heading", { name: "总览" })).toBeInTheDocument();
    });

    it("有活动时补齐最近 7 天，无数据天渲染占位而不是柱子", async () => {
      invokeMock.mockResolvedValue([
        {
          date: localIsoDay(),
          commits: 3,
          files_changed: 5,
          insertions: 42,
          deletions: 7,
          failed: 0,
        },
      ]);

      render(<OverviewHome sessions={[]} onOpen={() => {}} />);

      const chart = await screen.findByRole("img", {
        name: "最近活动按天柱状图，柱高表示每天的增删行数。",
      });
      expect(chart.querySelectorAll(".overview__activitydaybar")).toHaveLength(
        7,
      );
      expect(chart.querySelectorAll(".overview__activitybar")).toHaveLength(1);
      expect(
        chart.querySelectorAll(".overview__activityplaceholder"),
      ).toHaveLength(6);
      expect(
        chart.querySelector(".overview__activitydaybar--peak"),
      ).toHaveTextContent("49");
      expect(
        chart.querySelector(`[data-date="${localIsoDay()}"]`),
      ).toHaveAttribute("title", "今天：3 次提交 / +42 −7 行");
    });

    it("失败红点与文字图例成对出现，无失败天时两者都不渲染", async () => {
      invokeMock.mockResolvedValue([
        {
          date: localIsoDay(),
          commits: 2,
          files_changed: 4,
          insertions: 20,
          deletions: 5,
          failed: 1,
        },
      ]);

      const { unmount } = render(
        <OverviewHome sessions={[]} onOpen={() => {}} />,
      );

      await screen.findByText("红点表示当天有失败提交");
      expect(
        document.querySelectorAll(".overview__activityfaildot"),
      ).toHaveLength(1);
      expect(
        document.querySelector(`[data-date="${localIsoDay()}"]`),
      ).toHaveAttribute("title", "今天：2 次提交 / +20 −5 行 / 失败 1 次");
      unmount();

      invokeMock.mockResolvedValue([
        {
          date: localIsoDay(),
          commits: 2,
          files_changed: 4,
          insertions: 20,
          deletions: 5,
          failed: 0,
        },
      ]);
      render(<OverviewHome sessions={[]} onOpen={() => {}} />);

      await screen.findByRole("img", {
        name: "最近活动按天柱状图，柱高表示每天的增删行数。",
      });
      expect(
        screen.queryByText("红点表示当天有失败提交"),
      ).not.toBeInTheDocument();
      expect(
        document.querySelector(".overview__activityfaildot"),
      ).not.toBeInTheDocument();
    });
  });

  describe("用量", () => {
    it("按项目聚合排行 + 渲染诚实标注文案", async () => {
      render(
        <OverviewHome
          sessions={[
            {
              id: "s1",
              title: "会话一",
              repo_id: "r1",
              total_input_tokens: 10_000,
              total_output_tokens: 2_000,
            },
            {
              id: "s2",
              title: "会话二",
              repo_id: "r1",
              total_input_tokens: 1_000,
              total_output_tokens: 500,
            },
            {
              id: "s3",
              title: "会话三",
              repo_id: "r2",
              total_input_tokens: 200,
              total_output_tokens: 100,
            },
          ]}
          repos={[repoA, repoB]}
          onOpen={() => {}}
        />,
      );

      expect(screen.getAllByText("acme/web").length).toBeGreaterThan(0);
      expect(screen.getAllByText("acme/docs").length).toBeGreaterThan(0);
      expect(
        screen.getByText("输入 + 输出总量；带缓存命中的输入 token 会低报。"),
      ).toBeInTheDocument();
      await screen.findByText("最近还没有改动记录。");
    });

    it("项目用量条按最大项目归一化，并在 tooltip 说明总量占比", async () => {
      render(
        <OverviewHome
          sessions={[
            {
              id: "s1",
              title: "会话一",
              repo_id: "r1",
              total_input_tokens: 60,
              total_output_tokens: 15,
            },
            {
              id: "s2",
              title: "会话二",
              repo_id: "r2",
              total_input_tokens: 20,
              total_output_tokens: 5,
            },
          ]}
          repos={[repoA, repoB]}
          onOpen={() => {}}
        />,
      );

      const usageSection = screen.getByLabelText("用量");
      const webRow = within(usageSection)
        .getByText("acme/web")
        .closest(".overview__usagerow");
      const docsRow = within(usageSection)
        .getByText("acme/docs")
        .closest(".overview__usagerow");
      expect(webRow?.querySelector(".overview__usagefill")).toHaveStyle(
        "width: 100%",
      );
      expect(docsRow?.querySelector(".overview__usagefill")).toHaveStyle(
        "width: 33.33%",
      );
      expect(docsRow).toHaveAttribute(
        "title",
        "acme/docs：25 tokens，占总量 25%",
      );
      await screen.findByText("最近还没有改动记录。");
    });

    it("没有用量数据时显示人话空态", async () => {
      render(
        <OverviewHome
          sessions={[{ id: "s1", title: "会话一", repo_id: "r1" }]}
          repos={[repoA]}
          onOpen={() => {}}
        />,
      );
      expect(screen.getByText("还没有用量数据。")).toBeInTheDocument();
      await screen.findByText("最近还没有改动记录。");
    });

    it("点 USAGE 项目排行行触发 onSelectRepo(namespaceId, repoId)", async () => {
      const onSelectRepo = vi.fn();
      render(
        <OverviewHome
          sessions={[
            {
              id: "s1",
              title: "会话一",
              repo_id: "r1",
              total_input_tokens: 10_000,
              total_output_tokens: 2_000,
            },
          ]}
          repos={[repoA]}
          onOpen={() => {}}
          onSelectRepo={onSelectRepo}
        />,
      );

      const usageSection = screen.getByLabelText("用量");
      const row = within(usageSection).getByText("acme/web").closest("button");
      expect(row).not.toBeNull();
      fireEvent.click(row!);
      expect(onSelectRepo).toHaveBeenCalledWith("local", "r1");
      await screen.findByText("最近还没有改动记录。");
    });

    it("点 HIGHEST-USAGE SESSIONS 行触发 onOpen(session.id)", async () => {
      const onOpen = vi.fn();
      render(
        <OverviewHome
          sessions={[
            {
              id: "s1",
              title: "会话一",
              repo_id: "r1",
              total_input_tokens: 10_000,
              total_output_tokens: 2_000,
            },
          ]}
          repos={[repoA]}
          onOpen={onOpen}
        />,
      );

      const usageSection = screen.getByLabelText("用量");
      const row = within(usageSection).getByText("会话一").closest("button");
      expect(row).not.toBeNull();
      fireEvent.click(row!);
      expect(onOpen).toHaveBeenCalledWith("s1");
      await screen.findByText("最近还没有改动记录。");
    });

    it("无 repo_id 会话聚出的 local-default 桶行不可点", async () => {
      const onSelectRepo = vi.fn();
      render(
        <OverviewHome
          sessions={[
            {
              id: "s1",
              title: "无归属会话",
              total_input_tokens: 500,
              total_output_tokens: 100,
            },
          ]}
          onOpen={() => {}}
          onSelectRepo={onSelectRepo}
        />,
      );

      const usageSection = screen.getByLabelText("用量");
      const label = within(usageSection).getByText("Local 默认");
      expect(label.closest("button")).toBeNull();
      fireEvent.click(label);
      expect(onSelectRepo).not.toHaveBeenCalled();
      await screen.findByText("最近还没有改动记录。");
    });

    it("RECENT ACTIVITY 行防回归：有数据也不渲染成 button", async () => {
      invokeMock.mockImplementation((cmd: string) => {
        if (cmd === "recent_activity") {
          return Promise.resolve([
            {
              date: localIsoDay(-1),
              commits: 3,
              files_changed: 5,
              insertions: 42,
              deletions: 7,
              failed: 0,
            },
          ]);
        }
        return Promise.resolve(undefined);
      });

      render(<OverviewHome sessions={[]} onOpen={() => {}} />);

      await waitFor(() => {
        expect(screen.getByText("3 次改动")).toBeInTheDocument();
      });
      const row = screen
        .getByText("3 次改动")
        .closest(".overview__activityrow");
      expect(row).not.toBeNull();
      expect(row?.tagName).not.toBe("BUTTON");
    });
  });
});

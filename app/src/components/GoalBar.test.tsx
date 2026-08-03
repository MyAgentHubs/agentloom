import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, test, vi } from "vitest";
import { GoalBar } from "./GoalBar";
import type { GoalContract } from "../types/agent";
import { I18nProvider } from "../i18n";

const goal = (over: Partial<GoalContract> = {}): GoalContract => ({
  goal: "实现 stage 2 心情记录（参考 schema.md）",
  status: "frozen",
  criteria: [
    { id: "1", claim: "a", status: "passed", scope: "task" },
    { id: "2", claim: "b", status: "passed", scope: "task" },
    { id: "3", claim: "c", status: "pending", scope: "task" },
    { id: "4", claim: "d", status: "failed", scope: "task" },
    { id: "5", claim: "e", status: "pending", scope: "run" },
    { id: "6", claim: "f", status: "waived", scope: "task" },
  ],
  ...over,
});

describe("GoalBar", () => {
  test("一行三信号：标签 + n/m 离散计数 + 状态点 + 目标摘要", () => {
    render(
      <GoalBar
        goal={goal()}
        expanded={false}
        onToggle={() => {}}
        expandedSlot={null}
      />,
    );
    expect(screen.getByText("本轮目标")).toBeInTheDocument();
    expect(screen.getByText("3/6 ✓")).toBeInTheDocument(); // n=已了结(2 passed + 1 waived)=3·m=6·opus P1-A·✓ 常驻
    expect(screen.getByText(/实现 stage 2 心情记录/)).toBeInTheDocument();
    // 6 个状态点（.atd-gdots > i）
    expect(document.querySelectorAll(".goal-bar__dots > i")).toHaveLength(6);
  });

  test("n=已了结(passed+waived)·m=total（opus P1-A·waived 计入分子也计入分母）", () => {
    // 5 条：1 passed + 1 waived + 3 pending → n=已了结=2（passed+waived）·m=5
    const g = goal({
      criteria: [
        { id: "1", claim: "a", status: "passed", scope: "task" },
        { id: "2", claim: "b", status: "waived", scope: "task" },
        { id: "3", claim: "c", status: "pending", scope: "task" },
        { id: "4", claim: "d", status: "pending", scope: "task" },
        { id: "5", claim: "e", status: "pending", scope: "task" },
      ],
    });
    render(
      <GoalBar
        goal={g}
        expanded={false}
        onToggle={() => {}}
        expandedSlot={null}
      />,
    );
    expect(screen.getByText("2/5 ✓")).toBeInTheDocument(); // passed+waived=2 / total=5
  });

  test("uncertain 不计入已了结计数，并渲染独立状态点", () => {
    const g = goal({
      criteria: [
        { id: "1", claim: "a", status: "passed", scope: "task" },
        { id: "2", claim: "b", status: "uncertain", scope: "task" },
      ],
    });
    render(
      <GoalBar
        goal={g}
        expanded={false}
        onToggle={() => {}}
        expandedSlot={null}
      />,
    );
    expect(screen.getByText("1/2 ✓")).toBeInTheDocument();
    expect(
      document.querySelector(".goal-bar__dots > i.uncertain"),
    ).not.toBeNull();
  });

  test("有 failed → 条带 has-fail 暖橙描边", () => {
    render(
      <GoalBar
        goal={goal()}
        expanded={false}
        onToggle={() => {}}
        expandedSlot={null}
      />,
    );
    expect(document.querySelector(".goal-bar")).toHaveClass("has-fail");
  });

  test("全了结（passed+waived===total·无 pending/failed）→ is-done", () => {
    // 1 passed + 1 waived → 全了结 → is-done（验 waived 计入达成·屏⑥ 6/6 语义）
    const allDone = goal({
      criteria: [
        { id: "1", claim: "a", status: "passed", scope: "task" },
        { id: "2", claim: "b", status: "waived", scope: "task" },
      ],
    });
    render(
      <GoalBar
        goal={allDone}
        expanded={false}
        onToggle={() => {}}
        expandedSlot={null}
      />,
    );
    expect(document.querySelector(".goal-bar")).toHaveClass("is-done");
    expect(screen.getByText("2/2 ✓")).toBeInTheDocument();
  });

  test("点折叠行上抛 onToggle + aria-expanded；展开时渲染 expandedSlot（原位·非浮层）", () => {
    const onToggle = vi.fn();
    const { rerender } = render(
      <GoalBar
        goal={goal()}
        expanded={false}
        onToggle={onToggle}
        expandedSlot={<div>展开体</div>}
      />,
    );
    const btn = screen.getByRole("button", { name: /查看验收|本轮目标/ });
    expect(btn).toHaveAttribute("aria-expanded", "false");
    expect(screen.queryByText("展开体")).not.toBeInTheDocument(); // 折叠态不渲染展开体
    fireEvent.click(btn);
    expect(onToggle).toHaveBeenCalledOnce();
    rerender(
      <GoalBar
        goal={goal()}
        expanded
        onToggle={onToggle}
        expandedSlot={<div>展开体</div>}
      />,
    );
    expect(screen.getByText("展开体")).toBeInTheDocument(); // 原位向下展开
  });

  test("展开态点 bar 外部 → 调 onToggle 收回（outside-click 关闭）", () => {
    const onToggle = vi.fn();
    render(
      <div>
        <GoalBar
          topbar
          goal={goal()}
          expanded
          onToggle={onToggle}
          expandedSlot={<div>验收槽</div>}
        />
        <button>外部按钮</button>
      </div>,
    );
    fireEvent.mouseDown(screen.getByText("外部按钮"));
    expect(onToggle).toHaveBeenCalledTimes(1);
  });

  test("折叠态点外部 → 不调 onToggle", () => {
    const onToggle = vi.fn();
    render(
      <div>
        <GoalBar
          topbar
          goal={goal()}
          expanded={false}
          onToggle={onToggle}
          expandedSlot={null}
        />
        <button>外部按钮2</button>
      </div>,
    );
    fireEvent.mouseDown(screen.getByText("外部按钮2"));
    expect(onToggle).not.toHaveBeenCalled();
  });

  test("Task2：running=true → 渲转圈 spinner，不显示执行中文字", () => {
    render(
      <GoalBar
        goal={goal()}
        expanded={false}
        onToggle={() => {}}
        expandedSlot={null}
        running={true}
      />,
    );
    expect(document.querySelector(".goal-bar__spin")).not.toBeNull();
    expect(screen.queryByText("执行中")).toBeNull();
    expect(screen.queryByText(/working/i)).not.toBeInTheDocument();
  });

  test("Task2：running 缺省（false）→ 无 spinner", () => {
    render(
      <GoalBar
        goal={goal()}
        expanded={false}
        onToggle={() => {}}
        expandedSlot={null}
      />,
    );
    expect(document.querySelector(".goal-bar__spin")).toBeNull();
    expect(screen.queryByText("执行中")).not.toBeInTheDocument();
  });

  test("run 未完成 + 全 pending → 维持运行中计数，不进待复核态", () => {
    const pendingGoal = goal({
      criteria: [
        { id: "1", claim: "a", status: "pending", scope: "task" },
        { id: "2", claim: "b", status: "pending", scope: "run" },
      ],
    });
    render(
      <GoalBar
        goal={pendingGoal}
        expanded={false}
        onToggle={() => {}}
        expandedSlot={null}
        runComplete={false}
      />,
    );
    expect(screen.getByText("0/2 ✓")).toBeInTheDocument();
    expect(
      screen.queryByText("运行已完成 · 2 条验收待复核"),
    ).not.toBeInTheDocument();
    expect(document.querySelector(".goal-bar")).not.toHaveClass(
      "is-settled-unverified",
    );
  });

  test("run 完成 + 全 pending → 显示验收待复核中性态", () => {
    const pendingGoal = goal({
      criteria: [
        { id: "1", claim: "a", status: "pending", scope: "task" },
        { id: "2", claim: "b", status: "pending", scope: "run" },
      ],
    });
    render(
      <GoalBar
        goal={pendingGoal}
        expanded={false}
        onToggle={() => {}}
        expandedSlot={null}
        runComplete
        runHasMemberFailure={false}
      />,
    );
    expect(screen.getByText("运行已完成 · 2 条验收待复核")).toBeInTheDocument();
    // 去冗余：左侧只报总数「目标 N 条」·待核计数由右句承载·不重复「待核」
    expect(screen.getByText("目标 2 条")).toBeInTheDocument();
    expect(screen.queryByText("0/2 ✓")).not.toBeInTheDocument();
    expect(screen.queryByText("0/2 待核")).not.toBeInTheDocument();
    expect(document.querySelector(".goal-bar")).toHaveClass(
      "is-settled-unverified",
    );
  });

  test("run 完成 + 有 passed/waived → 维持现状非待复核态", () => {
    const partiallyResolved = goal({
      criteria: [
        { id: "1", claim: "a", status: "passed", scope: "task" },
        { id: "2", claim: "b", status: "waived", scope: "run" },
        { id: "3", claim: "c", status: "pending", scope: "task" },
      ],
    });
    render(
      <GoalBar
        goal={partiallyResolved}
        expanded={false}
        onToggle={() => {}}
        expandedSlot={null}
        runComplete
      />,
    );
    expect(screen.getByText("2/3 ✓")).toBeInTheDocument();
    expect(screen.queryByText(/验收待复核/)).not.toBeInTheDocument();
    expect(document.querySelector(".goal-bar")).not.toHaveClass(
      "is-settled-unverified",
    );
  });

  test("run 完成 + 有 failed → 走失败态非待复核", () => {
    const failedGoal = goal({
      criteria: [
        { id: "1", claim: "a", status: "pending", scope: "task" },
        { id: "2", claim: "b", status: "failed", scope: "run" },
      ],
    });
    render(
      <GoalBar
        goal={failedGoal}
        expanded={false}
        onToggle={() => {}}
        expandedSlot={null}
        runComplete
      />,
    );
    expect(document.querySelector(".goal-bar")).toHaveClass("has-fail");
    expect(document.querySelector(".goal-bar")).not.toHaveClass(
      "is-settled-unverified",
    );
    expect(screen.queryByText(/验收待复核/)).not.toBeInTheDocument();
  });

  test("Task3：topbar=true → goal-wrap 加 goal-wrap--topbar", () => {
    render(
      <GoalBar
        goal={goal()}
        expanded={false}
        onToggle={() => {}}
        expandedSlot={null}
        topbar={true}
      />,
    );
    expect(document.querySelector(".goal-wrap--topbar")).not.toBeNull();
  });

  test("Task3：topbar=false（缺省）→ 无 topbar 变体 class·保留 本轮目标 标签", () => {
    render(
      <GoalBar
        goal={goal()}
        expanded={false}
        onToggle={() => {}}
        expandedSlot={null}
      />,
    );
    expect(document.querySelector(".goal-wrap--topbar")).toBeNull();
    expect(screen.getByText("本轮目标")).toBeInTheDocument();
  });

  test("Task3：topbar + expanded → 展开体（expandedSlot）仍渲染（验收入口不丢）", () => {
    render(
      <GoalBar
        goal={goal()}
        expanded={true}
        onToggle={() => {}}
        expandedSlot={<div data-testid="goal-panel" />}
        topbar={true}
      />,
    );
    expect(screen.getByTestId("goal-panel")).toBeInTheDocument();
  });

  // polish-b：Local scratch 任务无验收标准（criteria=[]）·目标仍要在条上显示
  test("无验收标准（criteria=[]）→ 渲目标文本·不渲计数/状态点/查看验收", () => {
    const scratch = goal({ goal: "写 10 个冷笑话", criteria: [] });
    render(
      <GoalBar
        topbar
        goal={scratch}
        expanded={false}
        onToggle={() => {}}
        expandedSlot={null}
      />,
    );
    // 目标文本仍在
    expect(screen.getByText("写 10 个冷笑话")).toBeInTheDocument();
    // 无验收标准 → 不显 N/M ✓ 计数·不显状态点·不显查看验收 CTA
    expect(screen.queryByText(/\d+\/\d+ ✓/)).toBeNull();
    expect(document.querySelectorAll(".goal-bar__dots > i")).toHaveLength(0);
    expect(screen.queryByText(/查看验收/)).toBeNull();
  });

  test("有验收标准 → 渲状态点 + 查看验收 CTA", () => {
    render(
      <GoalBar
        topbar
        goal={goal()}
        expanded={false}
        onToggle={() => {}}
        expandedSlot={null}
      />,
    );
    expect(document.querySelectorAll(".goal-bar__dots > i")).toHaveLength(6);
    expect(screen.getByText(/查看验收/)).toBeInTheDocument();
  });

  test("无验收标准 + running → 仍渲转圈 spinner（执行中态）", () => {
    const scratch = goal({ goal: "写 10 个冷笑话", criteria: [] });
    render(
      <GoalBar
        topbar
        goal={scratch}
        expanded={false}
        onToggle={() => {}}
        expandedSlot={null}
        running
      />,
    );
    expect(document.querySelector(".goal-bar__spin")).not.toBeNull();
  });

  test("topbar 变体·有 goal_title → 渲出 goal_title 短标题", () => {
    const g = goal({ goal_title: "创建10个冷笑话文件" });
    render(
      <GoalBar
        topbar
        goal={g}
        expanded={false}
        onToggle={() => {}}
        expandedSlot={null}
      />,
    );
    expect(screen.getByText("创建10个冷笑话文件")).toBeInTheDocument();
    expect(screen.queryByText(g.goal)).not.toBeInTheDocument();
  });

  test("topbar 变体·无 goal_title（只有 goal.goal）→ fallback 显 goal.goal", () => {
    const g = goal();
    render(
      <GoalBar
        topbar
        goal={g}
        expanded={false}
        onToggle={() => {}}
        expandedSlot={null}
      />,
    );
    expect(screen.getByText(g.goal)).toBeInTheDocument();
  });

  test("isDone: runComplete no running no criteria -> .goal-bar__done and .goal-bar__ic.is-done", () => {
    const scratch = goal({ goal: "冷笑话", criteria: [] });
    render(
      <I18nProvider initialLocale="zh">
        <GoalBar
          topbar
          goal={scratch}
          expanded={false}
          onToggle={() => {}}
          expandedSlot={null}
          running={false}
          runComplete={true}
        />
      </I18nProvider>,
    );
    expect(document.querySelector(".goal-bar__done")).not.toBeNull();
    expect(document.querySelector(".goal-bar__ic.is-done")).not.toBeNull();
  });
  test("isDone: running=true -> no .goal-bar__done", () => {
    const scratch = goal({ goal: "冷笑话", criteria: [] });
    render(
      <I18nProvider initialLocale="zh">
        <GoalBar
          topbar
          goal={scratch}
          expanded={false}
          onToggle={() => {}}
          expandedSlot={null}
          running={true}
          runComplete={true}
        />
      </I18nProvider>,
    );
    expect(document.querySelector(".goal-bar__done")).toBeNull();
  });
  test("isDone: has criteria -> no .goal-bar__done", () => {
    render(
      <I18nProvider initialLocale="zh">
        <GoalBar
          topbar
          goal={goal()}
          expanded={false}
          onToggle={() => {}}
          expandedSlot={null}
          running={false}
          runComplete={true}
        />
      </I18nProvider>,
    );
    expect(document.querySelector(".goal-bar__done")).toBeNull();
  });
  test("topbar mode does not render .goal-bar__view (onView prop removed)", () => {
    render(
      <I18nProvider initialLocale="zh">
        <GoalBar
          topbar
          goal={goal({ goal: "冷笑话", criteria: [] })}
          expanded={false}
          onToggle={() => {}}
          expandedSlot={null}
        />
      </I18nProvider>,
    );
    expect(document.querySelector(".goal-bar__view")).toBeNull();
  });
});

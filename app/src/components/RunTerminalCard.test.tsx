import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { I18nProvider } from "../i18n";
import { RunTerminalCard } from "./RunTerminalCard";
import type { Block } from "../types/agent";

type RunTerminalBlock = Extract<Block, { type: "run_terminal" }>;

const mk = (o: Partial<RunTerminalBlock>): RunTerminalBlock => ({
  type: "run_terminal",
  run_id: "r1",
  status: "completed",
  message: null,
  ...o,
});

function renderCard(block: RunTerminalBlock, locale: "zh" | "en" = "zh") {
  return render(
    <I18nProvider initialLocale={locale}>
      <RunTerminalCard block={block} />
    </I18nProvider>,
  );
}

describe("RunTerminalCard", () => {
  it("completed 且无 message → 不渲染（return null）", () => {
    const { container } = renderCard(
      mk({ status: "completed", message: null }),
    );
    expect(container.firstChild).toBeNull();
  });

  it("completed 带 message → 绿点 +「已完成」+ message", () => {
    const { container } = renderCard(
      mk({ status: "completed", message: "已推送到远端" }),
    );
    expect(screen.getByText("已完成")).toBeInTheDocument();
    expect(screen.getByText("已推送到远端")).toBeInTheDocument();
    expect(container.querySelector(".run-terminal__dot--ok")).not.toBeNull();
  });

  it("error → 红点 +「出错」+ message", () => {
    const { container } = renderCard(
      mk({ status: "error", message: "网络超时" }),
    );
    expect(screen.getByText("出错")).toBeInTheDocument();
    expect(screen.getByText("网络超时")).toBeInTheDocument();
    expect(container.querySelector(".run-terminal__dot--error")).not.toBeNull();
  });

  it("error · zh locale → AL_ERR 信封显示本地化文案", () => {
    renderCard(
      mk({
        status: "error",
        message: 'AL_ERR:run.spawnFailed:{"detail":"看门狗超时"}',
      }),
      "zh",
    );
    expect(screen.getByText("启动失败：看门狗超时")).toBeInTheDocument();
    expect(screen.queryByText(/AL_ERR:/)).not.toBeInTheDocument();
  });

  it("error · en locale → AL_ERR envelope displays localized message", () => {
    renderCard(
      mk({
        status: "error",
        message: 'AL_ERR:run.spawnFailed:{"detail":"watchdog timeout"}',
      }),
      "en",
    );
    expect(
      screen.getByText("Failed to start the run: watchdog timeout"),
    ).toBeInTheDocument();
    expect(screen.queryByText(/AL_ERR:/)).not.toBeInTheDocument();
  });

  it("error · 非 AL_ERR 普通文本原样显示", () => {
    renderCard(mk({ status: "error", message: "plain persisted error" }));
    expect(screen.getByText("plain persisted error")).toBeInTheDocument();
  });

  it("interrupted → 黄点 +「已中断」+ message（若有）", () => {
    const { container } = renderCard(
      mk({ status: "interrupted", message: "用户手动停止" }),
    );
    expect(screen.getByText("已中断")).toBeInTheDocument();
    expect(screen.getByText("用户手动停止")).toBeInTheDocument();
    expect(container.querySelector(".run-terminal__dot--warn")).not.toBeNull();
  });

  it("interrupted 无 message → 只显示状态文案", () => {
    renderCard(mk({ status: "interrupted", message: null }));
    expect(screen.getByText("已中断")).toBeInTheDocument();
  });

  it("blocked · 未知 reason → 黄点 +「已停下」+ 原样裸串（前向兼容·不吞）", () => {
    const { container } = renderCard(
      mk({ status: "blocked", message: "安全 preflight 未过" }),
    );
    expect(screen.getByText("已停下")).toBeInTheDocument();
    expect(screen.getByText("安全 preflight 未过")).toBeInTheDocument();
    expect(container.querySelector(".run-terminal__dot--warn")).not.toBeNull();
  });

  it("blocked · 已知 reason 码 → 人话文案，不显裸串", () => {
    renderCard(mk({ status: "blocked", message: "no_progress" }));
    expect(screen.getByText("已停下")).toBeInTheDocument();
    expect(
      screen.getByText("连续多轮没有实质进展，已自动停下"),
    ).toBeInTheDocument();
    expect(screen.queryByText("no_progress")).not.toBeInTheDocument();
  });

  it("needs_decision → 黄点 +「待决策」（不附带 message）", () => {
    const { container } = renderCard(
      mk({ status: "needs_decision", message: "范围调整详情" }),
    );
    expect(screen.getByText("待决策")).toBeInTheDocument();
    expect(screen.queryByText("范围调整详情")).not.toBeInTheDocument();
    expect(container.querySelector(".run-terminal__dot--warn")).not.toBeNull();
  });

  it("fallback → 灰点 +「会话收尾未完成…」+ message（若有）", () => {
    const { container } = renderCard(
      mk({ status: "fallback", message: "已从最后一条工具事件恢复" }),
    );
    expect(
      screen.getByText("会话收尾未完成 · 已兜底恢复现场"),
    ).toBeInTheDocument();
    expect(screen.getByText("已从最后一条工具事件恢复")).toBeInTheDocument();
    expect(container.querySelector(".run-terminal__dot--muted")).not.toBeNull();
  });

  it("未知 status → 不崩·灰点 + 原样显示 status 文本", () => {
    const { container } = renderCard(
      mk({ status: "some_future_status", message: "新状态说明" }),
    );
    expect(screen.getByText("some_future_status")).toBeInTheDocument();
    expect(screen.getByText("新状态说明")).toBeInTheDocument();
    expect(container.querySelector(".run-terminal__dot--muted")).not.toBeNull();
  });

  it("message 较长 → 容器 title 属性携带全文", () => {
    const { container } = renderCard(
      mk({ status: "error", message: "非常长的错误信息一大段用于验证 title" }),
    );
    const el = container.querySelector(".run-terminal") as HTMLElement;
    expect(el.getAttribute("title")).toBe(
      "出错 · 非常长的错误信息一大段用于验证 title",
    );
  });

  it("en locale → 英文文案", () => {
    renderCard(mk({ status: "error", message: "boom" }), "en");
    expect(screen.getByText("Error")).toBeInTheDocument();
    expect(screen.getByText("boom")).toBeInTheDocument();
  });

  it("en locale · completed → Completed", () => {
    renderCard(mk({ status: "completed", message: "pushed" }), "en");
    expect(screen.getByText("Completed")).toBeInTheDocument();
  });

  it("en locale · fallback → 英文兜底文案", () => {
    renderCard(mk({ status: "fallback", message: null }), "en");
    expect(
      screen.getByText("Wrap-up incomplete · state recovered via fallback"),
    ).toBeInTheDocument();
  });
});

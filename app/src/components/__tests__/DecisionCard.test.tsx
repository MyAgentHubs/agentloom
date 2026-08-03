import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { DecisionCard } from "../DecisionCard";
import type { ChatMessage } from "../../types/agent";

type DecisionCardBlock = Extract<
  ChatMessage["content"][number],
  { type: "decision_card" }
>;

const baseRationale = "补齐测试能降低回归风险。";

const base: DecisionCardBlock = {
  type: "decision_card",
  decision_id: "d1",
  kind: "ask",
  question: "下一步怎么处理？",
  options: ["继续实现", "先补测试", "暂停"],
  recommended: "先补测试",
  rationale: baseRationale,
  payload: null,
  source_run_id: "run-1",
  status: "pending",
  chosen_option: null,
  created_at: 1,
};

describe("DecisionCard", () => {
  it("渲 .decision-card·dc-head 含短提示 + 折叠态问题·每 option 一个 .decision-option 按钮", () => {
    const { container } = render(
      <DecisionCard block={base} onChoose={vi.fn()} />,
    );

    const card = container.querySelector(".decision-card");
    expect(card).not.toBeNull();
    // 卡头两层：一句短提示 + question 全文（折叠态·CSS clamp，非 JS 移除·DOM 里仍可查到）。
    expect(card?.querySelector(".dc-head-hint")?.textContent).toBe(
      "选择一项即回复",
    );
    expect(card?.querySelector(".dc-head-question")?.textContent).toBe(
      base.question,
    );
    expect(
      card
        ?.querySelector(".dc-head-question")
        ?.classList.contains("dc-head-question--open"),
    ).toBe(false);
    const options = card?.querySelectorAll(".decision-option");
    expect(options).toHaveLength(base.options.length);
    options?.forEach((option) => {
      expect(option.tagName).toBe("BUTTON");
    });
  });

  it("问题默认折叠·点击展开切换态与按钮文案", () => {
    const { container } = render(
      <DecisionCard block={base} onChoose={vi.fn()} />,
    );

    fireEvent.click(screen.getByRole("button", { name: "展开问题 ▾" }));

    expect(
      container
        .querySelector(".dc-head-question")
        ?.classList.contains("dc-head-question--open"),
    ).toBe(true);
    expect(screen.getByRole("button", { name: "收起 ▴" })).toBeInTheDocument();
  });

  it("recommended option 带 .rec·.rec-pill 嵌在 <b> 内", () => {
    const { container } = render(
      <DecisionCard block={base} onChoose={vi.fn()} />,
    );

    const rec = container.querySelector(".decision-option.rec");
    expect(rec).not.toBeNull();
    expect(rec?.textContent).toContain(base.recommended);
    const pill = rec?.querySelector(".rec-pill");
    expect(pill?.textContent).toBe("推荐");
    expect(pill?.parentElement?.tagName).toBe("B");
  });

  it("有 onChoose 时点 option 调 onChoose(decision_id, option)", () => {
    const onChoose = vi.fn();
    render(<DecisionCard block={base} onChoose={onChoose} />);

    fireEvent.click(screen.getByRole("button", { name: /继续实现/ }));

    expect(onChoose).toHaveBeenCalledWith("d1", "继续实现");
  });

  it("submitting 时按钮禁用", () => {
    const { container } = render(
      <DecisionCard
        block={{ ...base, status: "submitting" }}
        onChoose={vi.fn()}
      />,
    );

    const options = Array.from(container.querySelectorAll(".decision-option"));
    expect(options).toHaveLength(base.options.length);
    options.forEach((button) => {
      expect(button).toBeDisabled();
    });
  });

  it("chosen 时渲紧凑「已选」回执行·不再整条消失", () => {
    const { container } = render(
      <DecisionCard
        block={{ ...base, status: "chosen", chosen_option: "先补测试" }}
        onChoose={vi.fn()}
      />,
    );

    expect(container.querySelector(".decision-card")).toBeNull();
    expect(container.querySelectorAll(".decision-option")).toHaveLength(0);
    expect(screen.queryByRole("button")).toBeNull();
    expect(container.querySelector(".decision-chosen")?.textContent).toBe(
      "已选：先补测试",
    );
  });

  it("rationale 默认折叠·点击为什么先问后才显示", () => {
    render(<DecisionCard block={base} onChoose={vi.fn()} />);

    expect(screen.queryByText(baseRationale)).toBeNull();

    fireEvent.click(screen.getByRole("button", { name: "为什么先问 ▾" }));

    expect(screen.getByText(baseRationale)).toBeInTheDocument();
  });

  it("选项无分隔符时不渲二级说明（整句作标签）", () => {
    const { container } = render(
      <DecisionCard block={base} onChoose={vi.fn()} />,
    );

    expect(
      container.querySelectorAll(".decision-option .di-tx > span"),
    ).toHaveLength(0);
  });

  it("选项含分隔符时切两段式：<b> 标签 + <span> 说明", () => {
    const withDesc = {
      ...base,
      options: ["按推荐继续，风险最低", "先停下，等确认后再继续"],
      recommended: "按推荐继续，风险最低",
    };
    const { container } = render(
      <DecisionCard block={withDesc} onChoose={vi.fn()} />,
    );

    const options = container.querySelectorAll(".decision-option");
    expect(options).toHaveLength(2);
    expect(options[0].querySelector(".di-tx b")?.textContent).toContain(
      "按推荐继续",
    );
    expect(options[0].querySelector(".di-tx > span")?.textContent).toBe(
      "风险最低",
    );
    expect(options[1].querySelector(".di-tx b")?.textContent).toBe("先停下");
    expect(options[1].querySelector(".di-tx > span")?.textContent).toBe(
      "等确认后再继续",
    );
  });

  it("failed 时显重试按钮", () => {
    const onChoose = vi.fn();
    render(
      <DecisionCard
        block={{ ...base, status: "failed", chosen_option: "暂停" }}
        onChoose={onChoose}
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: "重试" }));

    expect(onChoose).toHaveBeenCalledWith("d1", "暂停");
  });

  it("无 onChoose 时按钮禁用（只读展示）", () => {
    const { container } = render(<DecisionCard block={base} />);

    Array.from(container.querySelectorAll(".decision-option")).forEach(
      (button) => {
        expect(button).toBeDisabled();
      },
    );
  });
});

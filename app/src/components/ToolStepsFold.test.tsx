import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { ToolStepsFold } from "./ToolStepsFold";
import type { ToolBlock } from "../lib/streamItems";
import { I18nProvider } from "../i18n";

function tool(over: Partial<ToolBlock>): ToolBlock {
  return {
    type: "tool",
    id: "t1",
    tool: "Bash",
    summary: "ls -la",
    card: "command",
    status: "ok",
    exit_code: 0,
    output: null,
    ...over,
  };
}

function renderZh(blocks: ToolBlock[], defaultOpen?: boolean) {
  return render(
    <I18nProvider initialLocale="zh">
      <ToolStepsFold blocks={blocks} defaultOpen={defaultOpen} />
    </I18nProvider>,
  );
}

describe("ToolStepsFold", () => {
  it("组头文案按 n 插值", () => {
    renderZh([
      tool({ id: "a", summary: "ls" }),
      tool({ id: "b", summary: "cat a" }),
      tool({ id: "c", summary: "cd b" }),
    ]);
    expect(screen.getByText("执行了 3 步")).toBeInTheDocument();
  });

  it("组头带 DONE 徽标", () => {
    const { container } = renderZh([
      tool({ id: "a" }),
      tool({ id: "b" }),
      tool({ id: "c" }),
    ]);
    const summary = container.querySelector("summary.toolfold__sum");
    expect(summary?.querySelector(".toolcard__badge--done")).not.toBeNull();
    expect(summary).toHaveTextContent("完成");
  });

  it("defaultOpen=false 时初始收起", () => {
    const { container } = renderZh(
      [tool({ id: "a" }), tool({ id: "b" }), tool({ id: "c" })],
      false,
    );
    const details = container.querySelector("details.toolfold");
    expect(details).not.toBeNull();
    expect(details?.hasAttribute("open")).toBe(false);
  });

  it("defaultOpen=true 时初始展开", () => {
    const { container } = renderZh(
      [tool({ id: "a" }), tool({ id: "b" }), tool({ id: "c" })],
      true,
    );
    const details = container.querySelector("details.toolfold");
    expect(details).not.toBeNull();
    expect(details?.hasAttribute("open")).toBe(true);
  });

  it("点开展开后渲出组内全部 ToolCard（compact）", () => {
    renderZh([
      tool({ id: "a", summary: "ls" }),
      tool({ id: "b", summary: "cat a" }),
      tool({ id: "c", summary: "cd b" }),
    ]);
    fireEvent.click(screen.getByText("执行了 3 步"));
    expect(screen.getByText("ls")).toBeInTheDocument();
    expect(screen.getByText("cat a")).toBeInTheDocument();
    expect(screen.getByText("cd b")).toBeInTheDocument();
  });

  it("点击 summary 后通过 open 属性切换 chevron 展开态", () => {
    const { container } = renderZh([
      tool({ id: "a", summary: "ls" }),
      tool({ id: "b", summary: "cat a" }),
    ]);
    const details = container.querySelector("details.toolfold");
    const chevron = details?.querySelector(".toolfold__chevron");

    expect(details?.hasAttribute("open")).toBe(false);
    expect(chevron).not.toBeNull();
    fireEvent.click(screen.getByText("执行了 2 步"));
    expect(details?.hasAttribute("open")).toBe(true);
    expect(details?.querySelector(".toolfold__chevron")).toBe(chevron);
  });

  it("重渲染（相同 key）不丢展开态", () => {
    const blocksV1 = [
      tool({ id: "a", summary: "ls" }),
      tool({ id: "b", summary: "cat a" }),
      tool({ id: "c", summary: "cd b" }),
    ];
    const { rerender, container } = renderZh(blocksV1);
    fireEvent.click(screen.getByText("执行了 3 步"));
    const details = container.querySelector("details.toolfold");
    expect(details?.hasAttribute("open")).toBe(true);

    // 流式追加新块（组身份延续·内部 state 应保留 open 态）
    const blocksV2 = [...blocksV1, tool({ id: "d", summary: "pwd" })];
    rerender(
      <I18nProvider initialLocale="zh">
        <ToolStepsFold blocks={blocksV2} defaultOpen={false} />
      </I18nProvider>,
    );
    const detailsAfter = container.querySelector("details.toolfold");
    expect(detailsAfter?.hasAttribute("open")).toBe(true);
    expect(screen.getByText("执行了 4 步")).toBeInTheDocument();
  });

  it("用户可收起 defaultOpen=true 的组，重渲染不会强制掰回展开", () => {
    const blocks = [
      tool({ id: "a", summary: "ls" }),
      tool({ id: "b", summary: "cat a" }),
    ];
    const { rerender, container } = renderZh(blocks, true);

    fireEvent.click(screen.getByText("执行了 2 步"));
    expect(
      container.querySelector("details.toolfold")?.hasAttribute("open"),
    ).toBe(false);

    rerender(
      <I18nProvider initialLocale="zh">
        <ToolStepsFold blocks={blocks} defaultOpen />
      </I18nProvider>,
    );
    expect(
      container.querySelector("details.toolfold")?.hasAttribute("open"),
    ).toBe(false);
  });
});

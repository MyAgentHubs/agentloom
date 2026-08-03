import { render, screen, fireEvent } from "@testing-library/react";
import { describe, it, expect, vi } from "vitest";
import { RightPanelTabs } from "./RightPanelTabs";

const base = {
  open: true,
  tab: null,
  openTabs: [],
  expanded: false,
  onTab: () => {},
  onExpand: () => {},
  onUserCollapse: () => {},
  onExpandPanel: () => {},
  onRestorePanel: () => {},
};

describe("RightPanelTabs v3", () => {
  it("picker 默认态（tab=null, openTabs=[]）tab 行只有末尾 +，不预渲任何 tab", () => {
    render(<RightPanelTabs {...base} />);
    expect(screen.queryByRole("tab")).not.toBeInTheDocument();
    expect(screen.getByLabelText("新 tab / 回选择器")).toBeInTheDocument();
  });

  it("打开某个 tab（tab=review, openTabs=[review]）渲染该 tab 并激活", () => {
    render(<RightPanelTabs {...base} tab="review" openTabs={["review"]} />);
    expect(screen.getByRole("tab", { name: "Review" })).toHaveAttribute(
      "aria-selected",
      "true",
    );
  });

  it("plan B3：reviewBadge>0 时 Review tab 显角标数字", () => {
    render(
      <RightPanelTabs
        {...base}
        tab="review"
        openTabs={["review"]}
        reviewBadge={3}
      />,
    );
    expect(screen.getByText("3")).toBeInTheDocument();
  });

  it("plan B3：reviewBadge=0 时不显角标", () => {
    render(
      <RightPanelTabs
        {...base}
        tab="review"
        openTabs={["review"]}
        reviewBadge={0}
      />,
    );
    expect(screen.queryByText("0")).not.toBeInTheDocument();
  });

  it("打开多个 tab，未激活的不带 aria-selected=true", () => {
    render(
      <RightPanelTabs
        {...base}
        tab="files"
        openTabs={["files", "review", "terminal"]}
      />,
    );
    expect(screen.getByRole("tab", { name: "Files" })).toHaveAttribute(
      "aria-selected",
      "true",
    );
    expect(screen.getByRole("tab", { name: "Review" })).toHaveAttribute(
      "aria-selected",
      "false",
    );
    expect(screen.getByRole("tab", { name: "Terminal" })).toHaveAttribute(
      "aria-selected",
      "false",
    );
  });

  it("点 tab 触发 onTab(tabId)", () => {
    const onTab = vi.fn();
    render(
      <RightPanelTabs
        {...base}
        tab="review"
        openTabs={["files", "review"]}
        onTab={onTab}
      />,
    );
    fireEvent.click(screen.getByRole("tab", { name: "Files" }));
    expect(onTab).toHaveBeenCalledWith("files");
  });

  it("点末尾 + 触发 onTab(null)（回 picker）", () => {
    const onTab = vi.fn();
    render(
      <RightPanelTabs
        {...base}
        tab="review"
        openTabs={["review"]}
        onTab={onTab}
      />,
    );
    fireEvent.click(screen.getByLabelText("新 tab / 回选择器"));
    expect(onTab).toHaveBeenCalledWith(null);
  });

  it("不渲染 Agent tab（v3 去 Agent 卡）", () => {
    render(
      <RightPanelTabs
        {...base}
        tab="review"
        openTabs={["files", "review", "side", "terminal", "browser"]}
      />,
    );
    expect(
      screen.queryByRole("tab", { name: /Agent/i }),
    ).not.toBeInTheDocument();
  });

  it("tab 行不渲染 ⌂ 前缀符号（v3 平铺无 ⌂）", () => {
    const { container } = render(
      <RightPanelTabs {...base} tab="terminal" openTabs={["terminal"]} />,
    );
    expect(container.textContent ?? "").not.toContain("⌂");
  });

  it("展开态有 ⤢ 展开（占用 main） + ▣ 收起两枚不同按钮", () => {
    render(<RightPanelTabs {...base} />);
    expect(screen.getByLabelText("展开（占用 main）")).toBeInTheDocument();
    expect(screen.getByLabelText("收起右面板")).toBeInTheDocument();
  });

  it("canMaximize=false 时隐藏展开/恢复控件但保留收起", () => {
    render(<RightPanelTabs {...base} canMaximize={false} />);
    expect(
      screen.queryByLabelText("展开（占用 main）"),
    ).not.toBeInTheDocument();
    expect(screen.queryByLabelText("恢复分栏")).not.toBeInTheDocument();
    expect(screen.queryByLabelText("通知")).not.toBeInTheDocument();
    expect(screen.getByLabelText("收起右面板")).toBeInTheDocument();
  });

  it("canMaximize=false 且 expanded=true 时同样隐藏恢复分栏", () => {
    render(<RightPanelTabs {...base} expanded={true} canMaximize={false} />);
    expect(screen.queryByLabelText("恢复分栏")).not.toBeInTheDocument();
    expect(
      screen.queryByLabelText("展开（占用 main）"),
    ).not.toBeInTheDocument();
    expect(screen.queryByLabelText("通知")).not.toBeInTheDocument();
    expect(screen.getByLabelText("收起右面板")).toBeInTheDocument();
  });

  it("点 ⤢ 触发 onExpandPanel（非展开态）", () => {
    const onExpandPanel = vi.fn();
    render(<RightPanelTabs {...base} onExpandPanel={onExpandPanel} />);
    fireEvent.click(screen.getByLabelText("展开（占用 main）"));
    expect(onExpandPanel).toHaveBeenCalledTimes(1);
  });

  it("展开态 ⤢ 变「恢复分栏」、点触发 onRestorePanel", () => {
    const onRestorePanel = vi.fn();
    render(
      <RightPanelTabs
        {...base}
        expanded={true}
        onRestorePanel={onRestorePanel}
      />,
    );
    expect(
      screen.queryByLabelText("展开（占用 main）"),
    ).not.toBeInTheDocument();
    expect(screen.getByLabelText("恢复分栏")).toBeInTheDocument();
    fireEvent.click(screen.getByLabelText("恢复分栏"));
    expect(onRestorePanel).toHaveBeenCalledTimes(1);
  });

  it("点 ▣ 触发 onUserCollapse（不触发 onExpandPanel / onExpand）", () => {
    const onUserCollapse = vi.fn();
    const onExpandPanel = vi.fn();
    const onExpand = vi.fn();
    render(
      <RightPanelTabs
        {...base}
        onUserCollapse={onUserCollapse}
        onExpandPanel={onExpandPanel}
        onExpand={onExpand}
      />,
    );
    fireEvent.click(screen.getByLabelText("收起右面板"));
    expect(onUserCollapse).toHaveBeenCalledTimes(1);
    expect(onExpandPanel).not.toHaveBeenCalled();
    expect(onExpand).not.toHaveBeenCalled();
  });

  it("收起态只渲染单个展开 toggle，不渲染 tab/+/⤢/▣", () => {
    render(<RightPanelTabs {...base} open={false} />);
    expect(screen.queryByRole("tab")).not.toBeInTheDocument();
    expect(
      screen.queryByLabelText("新 tab / 回选择器"),
    ).not.toBeInTheDocument();
    expect(
      screen.queryByLabelText("展开（占用 main）"),
    ).not.toBeInTheDocument();
    expect(screen.queryByLabelText("收起右面板")).not.toBeInTheDocument();
    expect(screen.getByLabelText("展开右面板")).toBeInTheDocument();
  });

  it("收起态点展开 toggle 触发 onExpand（不触发 onUserCollapse）", () => {
    const onExpand = vi.fn();
    const onUserCollapse = vi.fn();
    render(
      <RightPanelTabs
        {...base}
        open={false}
        onExpand={onExpand}
        onUserCollapse={onUserCollapse}
      />,
    );
    fireEvent.click(screen.getByLabelText("展开右面板"));
    expect(onExpand).toHaveBeenCalledTimes(1);
    expect(onUserCollapse).not.toHaveBeenCalled();
  });

  it("收起态不渲染全局通知，只保留右面板展开按钮", () => {
    const { container } = render(<RightPanelTabs {...base} open={false} />);
    const expand = screen.getByLabelText("展开右面板");
    expect(screen.queryByLabelText("通知")).not.toBeInTheDocument();
    expect(expand).toBeInTheDocument();
    const collapsed = container.querySelector(".topbar__panel--collapsed");
    expect(collapsed?.contains(expand)).toBe(true);
  });

  it("展开态不渲染全局通知，只保留右面板窗口控件", () => {
    const { container } = render(<RightPanelTabs {...base} />);
    const expandPanel = screen.getByLabelText("展开（占用 main）");
    expect(screen.queryByLabelText("通知")).not.toBeInTheDocument();
    expect(expandPanel).toBeInTheDocument();
    const wins = container.querySelector(".rptabs__wins");
    expect(wins?.contains(expandPanel)).toBe(true);
  });
});

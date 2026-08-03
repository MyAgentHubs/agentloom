import { describe, it, expect } from "vitest";
import { render, screen, within } from "@testing-library/react";
import { InputArea } from "../InputArea";

function renderInputArea(extra: Record<string, unknown> = {}) {
  return render(
    <InputArea
      composerBusy={false}
      running={false}
      memberRunning={false}
      agents={[]}
      agentId="a1"
      onAgentChange={() => {}}
      mode="normal"
      onModeChange={() => {}}
      onSend={() => {}}
      onStop={() => {}}
      {...extra}
    />,
  );
}

describe("InputArea 权限控件（诚实单 Auto）", () => {
  it("呈现单一 Auto + 信任落地说明文案", () => {
    renderInputArea();
    const control = screen.getByTestId("composer-permission");
    expect(control).toBeInTheDocument();
    // 仍标明当前是 Auto 档
    expect(within(control).getByText("Auto")).toBeInTheDocument();
    // 说明文案移进 title（hover tip）
    expect(
      control.querySelector(".composer__permission")?.getAttribute("title"),
    ).toContain("信任落地");
    expect(
      control.querySelector(".composer__permission")?.getAttribute("title"),
    ).toMatch(/Review|撤销/);
  });

  it("不伪装可切换：无下拉菜单 / 无 haspopup / 无 expanded 假交互", () => {
    renderInputArea();
    // 不应存在权限的弹出菜单（假可切换）
    expect(screen.queryByRole("menu")).not.toBeInTheDocument();
    expect(screen.queryByRole("menuitemradio")).not.toBeInTheDocument();
    // 控件不应宣称自己是可打开的菜单触发器
    const haspopup = document.querySelector(
      '[data-testid="composer-permission"] [aria-haspopup], [data-testid="composer-permission"][aria-haspopup]',
    );
    expect(haspopup).toBeNull();
    const expandable = document.querySelector(
      '[data-testid="composer-permission"] [aria-expanded], [data-testid="composer-permission"][aria-expanded]',
    );
    expect(expandable).toBeNull();
  });

  it("控件本身不是可点击 button（无误导交互可供性）", () => {
    renderInputArea();
    const control = screen.getByTestId("composer-permission");
    // 控件根不是 <button>，也不含可点击的权限按钮
    expect(control.tagName.toLowerCase()).not.toBe("button");
    expect(within(control).queryByRole("button")).not.toBeInTheDocument();
  });
});

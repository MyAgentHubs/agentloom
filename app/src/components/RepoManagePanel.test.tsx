import {
  render,
  screen,
  fireEvent,
  waitFor,
  within,
} from "@testing-library/react";
import { describe, it, expect, vi } from "vitest";
import { RepoManagePanel } from "./RepoManagePanel";
import type { RepoManagePanelProps } from "../types/repoManage";

function base(over: Partial<RepoManagePanelProps> = {}): RepoManagePanelProps {
  return {
    accounts: [
      { login: "acme", active: true, count: 2 },
      { login: "work", active: false },
    ],
    selectedLogin: "acme",
    onSelectAccount: vi.fn(),
    onConnectAccount: vi.fn(),
    onConnectLocal: vi.fn(),
    gate: { kind: "ready" },
    onInstallGh: vi.fn(),
    onRefreshAccounts: vi.fn(),
    listState: { kind: "ready", repos: [] },
    onRetryList: vi.fn(),
    search: "",
    onSearchChange: vi.fn(),
    filter: "all",
    onFilterChange: vi.fn(),
    selected: new Set(),
    onToggleSelect: vi.fn(),
    baseFolderLabel: "~/code/",
    onClone: vi.fn(),
    cloneProgress: {},
    onRetry: vi.fn(),
    onOpenSession: vi.fn(),
    ...over,
  };
}

describe("RepoManagePanel", () => {
  it("可添加本地已克隆仓库，并在认领失败时显示对应提示", () => {
    const onConnectLocal = vi.fn();
    render(
      <RepoManagePanel
        {...base({ onConnectLocal, connectError: "NOT_GIT" })}
      />,
    );

    fireEvent.click(
      screen.getByRole("button", { name: "添加本地已克隆的仓库" }),
    );
    expect(onConnectLocal).toHaveBeenCalledTimes(1);
    expect(screen.getByRole("alert")).toHaveTextContent("不是 git 仓库");
  });

  it("账户改顶部下拉·点开列账户·切账户触发 onSelectAccount", () => {
    const onSelectAccount = vi.fn();
    const selected = new Set(["github.com/acme/todo"]);
    const { container } = render(
      <RepoManagePanel {...base({ onSelectAccount, selected })} />,
    );
    expect(container.querySelector(".acct-dd .cnt")).toBeNull();
    expect(screen.getByText("0 已克隆 / 0 远程")).toBeInTheDocument();
    expect(container.querySelector(".rm-foot .ob-batchbar")).not.toBeNull();
    const summary = container.querySelector(
      ".rm-foot .ob-batchbar .summary",
    )?.textContent;
    expect(summary).toContain("已选");
    expect(summary).toContain("1");
    expect(summary).toContain("~/code/");
    expect(summary).toContain("以 @acme 身份提交");
    expect(screen.getByRole("button", { name: "克隆" })).toBeTruthy();

    const dd = screen.getByLabelText("切换账户");
    fireEvent.click(dd);
    expect(container.querySelector(".acct-menu .ct")).toBeNull();
    fireEvent.click(screen.getByRole("menuitemradio", { name: /work/ }));
    expect(onSelectAccount).toHaveBeenCalledWith("work");
  });
  it("不再渲染左栏 ob-disc-side", () => {
    const { container } = render(<RepoManagePanel {...base()} />);
    expect(container.querySelector(".ob-disc-side")).toBeNull();
  });
  it("固定底 rm-foot 永远在：ready+有选→克隆 bar / 否则→连接 GitHub 账号按钮", () => {
    const sel = render(
      <RepoManagePanel
        {...base({ selected: new Set(["github.com/acme/todo"]) })}
      />,
    );
    expect(sel.container.querySelector(".rm-foot .ob-batchbar")).not.toBeNull();
    sel.unmount();

    const gate = render(
      <RepoManagePanel
        {...base({
          gate: {
            kind: "missing",
            canBrewInstall: false,
            installing: false,
          },
        })}
      />,
    );
    expect(gate.container.querySelector(".rm-foot")).not.toBeNull();
    expect(gate.getByText("连接 GitHub 账号")).toBeInTheDocument();
  });
  it("固定底 rm-foot 在 cold-loading 时仍显示连接 GitHub 账号按钮", () => {
    const { container, getByText } = render(
      <RepoManagePanel {...base({ listState: { kind: "loading" } })} />,
    );

    expect(container.querySelector(".rm-foot")).not.toBeNull();
    expect(getByText("连接 GitHub 账号")).toBeInTheDocument();
  });
  it("固定底 rm-foot 在 cold-error 时仍显示连接 GitHub 账号按钮", () => {
    const { container, getByText } = render(
      <RepoManagePanel {...base({ listState: { kind: "offline" } })} />,
    );

    expect(container.querySelector(".rm-foot")).not.toBeNull();
    expect(getByText("连接 GitHub 账号")).toBeInTheDocument();
  });
  it("固定底 rm-foot 在 ready-empty 时仍显示连接 GitHub 账号按钮", () => {
    const { container, getByText } = render(
      <RepoManagePanel
        {...base({
          listState: { kind: "ready", repos: [] },
          selected: new Set(),
        })}
      />,
    );

    expect(container.querySelector(".rm-foot")).not.toBeNull();
    expect(getByText("连接 GitHub 账号")).toBeInTheDocument();
  });
  it("账户菜单点外部关闭后触发器仍可再次打开", async () => {
    render(<RepoManagePanel {...base()} />);

    const trigger = screen.getByLabelText("切换账户");
    fireEvent.click(trigger);
    expect(screen.getByRole("menuitemradio", { name: /work/ })).toBeTruthy();

    fireEvent.mouseDown(document.body);
    await waitFor(() => {
      expect(
        screen.queryByRole("menuitemradio", { name: /work/ }),
      ).not.toBeInTheDocument();
    });

    fireEvent.click(trigger);
    expect(screen.getByRole("menuitemradio", { name: /work/ })).toBeTruthy();
  });
  it("gate=missing+brew 显示一键安装并触发 onInstallGh", () => {
    const onInstallGh = vi.fn();
    render(
      <RepoManagePanel
        {...base({
          gate: {
            kind: "missing",
            canBrewInstall: true,
            installing: false,
          },
          onInstallGh,
        })}
      />,
    );
    fireEvent.click(screen.getByRole("button", { name: /一键安装/ }));
    expect(onInstallGh).toHaveBeenCalled();
  });
  it("gate=missing 无 brew 显示手动安装链接", () => {
    render(
      <RepoManagePanel
        {...base({
          gate: {
            kind: "missing",
            canBrewInstall: false,
            installing: false,
          },
        })}
      />,
    );
    expect(screen.getByText(/手动安装/)).toBeTruthy();
  });
  it("gate=noAccount 引导 gh auth login + 刷新", () => {
    const onRefreshAccounts = vi.fn();
    render(
      <RepoManagePanel
        {...base({
          gate: { kind: "noAccount" },
          onRefreshAccounts,
        })}
      />,
    );
    expect(screen.getByText(/gh auth login/)).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: /刷新/ }));
    expect(onRefreshAccounts).toHaveBeenCalled();
  });
  it("gate=checking 显示环境检测状态而不是读取仓库", () => {
    render(
      <RepoManagePanel
        {...base({
          accounts: [],
          selectedLogin: "",
          gate: { kind: "checking" },
        })}
      />,
    );

    expect(
      screen.getByRole("status", { name: "正在检查仓库环境" }),
    ).toHaveTextContent("正在检查环境");
    expect(screen.queryByLabelText("正在读取仓库")).toBeNull();
  });
  it("gate=missingGit 显示 Git 安装提示并可重新检测", () => {
    const onRetryList = vi.fn();
    render(
      <RepoManagePanel
        {...base({
          accounts: [],
          selectedLogin: "",
          gate: { kind: "missingGit" },
          onRetryList,
        })}
      />,
    );

    expect(screen.getByText("需要 Git")).toBeInTheDocument();
    expect(screen.getByLabelText("切换账户")).toBeDisabled();
    expect(screen.queryByLabelText("正在读取仓库")).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "重新检测" }));
    expect(onRetryList).toHaveBeenCalledTimes(1);
  });
  it("gate=accountError 将超时转成可理解错误并允许重试", () => {
    const onRefreshAccounts = vi.fn();
    render(
      <RepoManagePanel
        {...base({
          accounts: [],
          selectedLogin: "",
          gate: { kind: "accountError", message: "TIMEOUT" },
          onRefreshAccounts,
        })}
      />,
    );

    expect(screen.getByText("GitHub 账户读取失败")).toBeInTheDocument();
    expect(screen.getByText(/读取 GitHub 账户超时/)).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "重试" }));
    expect(onRefreshAccounts).toHaveBeenCalledTimes(1);
  });
  it("view=idle 不冒充读取中，用户点击后才触发读取", () => {
    const onRetryList = vi.fn();
    const { listState: _listState, ...props } = base({ onRetryList });
    render(<RepoManagePanel {...props} view={{ kind: "idle" }} />);

    expect(screen.getByText("尚未读取远端仓库")).toBeInTheDocument();
    expect(screen.queryByLabelText("正在读取仓库")).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "读取" }));
    expect(onRetryList).toHaveBeenCalledTimes(1);
  });
  it("点连接 GitHub 账号显示内联 gh auth login 引导并用刷新重读账户", () => {
    const onConnectAccount = vi.fn();
    const onRefreshAccounts = vi.fn();
    render(
      <RepoManagePanel {...base({ onConnectAccount, onRefreshAccounts })} />,
    );

    fireEvent.click(screen.getByRole("button", { name: "连接 GitHub 账号" }));

    expect(
      screen.getByText(
        (_content, element) =>
          element?.classList.contains("sub") === true &&
          element.textContent ===
            "在终端运行 gh auth login 添加账户，完成后点刷新。",
      ),
    ).toBeTruthy();
    expect(screen.getByText("gh auth login")).toBeTruthy();
    expect(onConnectAccount).not.toHaveBeenCalled();

    fireEvent.click(screen.getByRole("button", { name: "刷新" }));

    expect(onRefreshAccounts).toHaveBeenCalledTimes(1);
  });
  it("listState=offline 显示离线降级", () => {
    render(<RepoManagePanel {...base({ listState: { kind: "offline" } })} />);
    expect(screen.getByText(/检测不到网络/)).toBeTruthy();
  });
  it("listState=loading 显示本机读取状态 + skeleton", () => {
    const { container } = render(
      <RepoManagePanel {...base({ listState: { kind: "loading" } })} />,
    );
    const status = screen.getByRole("status", {
      name: /正在读取仓库/,
    });
    expect(status).toBeInTheDocument();
    expect(within(status).getByText(/正在读取仓库/)).toBeInTheDocument();
    expect(container.querySelector(".ob-sk")).toBeTruthy(); // mockup class（onboarding.html:190）
  });
  it("仓库统计显示在账户下拉右侧，列表区不重复账户标题", () => {
    render(
      <RepoManagePanel
        {...base({
          listState: {
            kind: "ready",
            repos: [
              {
                owner: "acme",
                name: "done",
                name_with_owner: "acme/done",
                is_private: false,
                is_empty: false,
                updated_at: "x",
                description: null,
                language: null,
                language_color: null,
                cloned: true,
                repo_id: "r1",
                local_path: "~/code/acme/done",
              },
              {
                owner: "acme",
                name: "todo",
                name_with_owner: "acme/todo",
                is_private: false,
                is_empty: false,
                updated_at: "x",
                description: null,
                language: null,
                language_color: null,
                cloned: false,
                repo_id: null,
                local_path: null,
              },
            ],
          },
        })}
      />,
    );

    expect(screen.getByText("1 已克隆 / 1 远程")).toBeInTheDocument();
    expect(screen.queryByText(/@acme 的仓库/)).toBeNull();
  });
  it("不把 CloneProgress 渲染成管理仓库面板的第三列", () => {
    const { container } = render(
      <RepoManagePanel
        {...base({
          cloneProgress: {
            "github.com/acme/demo": {
              login: "acme",
              owner: "acme",
              name: "demo",
              order: 0,
              phase: "cloning",
            },
          },
        })}
      />,
    );

    expect(container.querySelector(".ob-prog")).toBeNull();
  });
});

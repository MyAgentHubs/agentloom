import {
  act,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { UpdaterSnapshot } from "../../types/updater";

const { invokeMock, listenMock } = vi.hoisted(() => ({
  invokeMock: vi.fn(),
  listenMock: vi.fn(),
}));

vi.mock("@tauri-apps/api/core", () => ({ invoke: invokeMock }));
vi.mock("@tauri-apps/api/event", () => ({ listen: listenMock }));

// 更新说明没有会话上下文，MarkdownBody 应该保持不传 sessionId（只靠 B 规则放行
// 图片）——用 importOriginal 包一层 spy，既不打断既有用例依赖的真实 markdown 渲染，
// 又能核到调用方到底传了什么 props。
const markdownBodySpy = vi.fn();
vi.mock("../../lib/useMarkdown", async (importOriginal) => {
  const actual = await importOriginal<typeof import("../../lib/useMarkdown")>();
  return {
    ...actual,
    useMarkdown: (...args: Parameters<typeof actual.useMarkdown>) => {
      const Real = actual.useMarkdown(...args);
      if (!Real) return Real;
      return (props: Parameters<typeof Real>[0]) => {
        markdownBodySpy(props);
        return <Real {...props} />;
      };
    },
  };
});

import { __resetForTests } from "../../lib/updaterStore";
import { UpdateSection } from "./UpdateSection";

function snap(
  revision: number,
  kind: string,
  extra: object = {},
): UpdaterSnapshot {
  return { revision, state: { kind, ...extra } } as UpdaterSnapshot;
}

describe("UpdateSection", () => {
  beforeEach(() => {
    __resetForTests();
    invokeMock.mockReset();
    listenMock.mockReset();
    listenMock.mockImplementation(async () => vi.fn());
  });

  // 保险：健康握手用 fake timers 的用例已经自带 try/finally 复原，这里再兜
  // 一层，防止某个用例异常退出时 fake timers 泄漏进下一个用例。
  afterEach(() => {
    vi.useRealTimers();
  });

  it("disabled/dev → 显示开发构建文案，隐藏检查按钮", async () => {
    invokeMock.mockResolvedValueOnce(snap(1, "disabled", { reason: "dev" }));
    render(<UpdateSection />);
    await waitFor(() =>
      expect(
        screen.getByText("此构建不支持在线更新（开发构建）"),
      ).toBeInTheDocument(),
    );
    expect(
      screen.queryByRole("button", { name: "检查更新" }),
    ).not.toBeInTheDocument();
  });

  it("disabled/platform → 显示当前平台不支持文案", async () => {
    invokeMock.mockResolvedValueOnce(
      snap(1, "disabled", { reason: "platform" }),
    );
    render(<UpdateSection />);
    await waitFor(() =>
      expect(
        screen.getByText("此构建不支持在线更新（当前平台不支持）"),
      ).toBeInTheDocument(),
    );
  });

  it("首个 revision=0 的 disabled/unsigned 快照 → 首帧显示尚未启用签名文案", async () => {
    invokeMock.mockResolvedValueOnce(
      snap(0, "disabled", { reason: "unsigned" }),
    );
    render(<UpdateSection />);
    await waitFor(() =>
      expect(
        screen.getByText("此构建不支持在线更新（尚未启用签名）"),
      ).toBeInTheDocument(),
    );
  });

  it("首帧挂载多个 updater 消费者健康握手延迟 20s 才发生（推进前不调），且全程只调用一次", async () => {
    invokeMock.mockImplementation(async (cmd: unknown) =>
      cmd === "updater_get_state" ? snap(0, "idle") : undefined,
    );
    vi.useFakeTimers();
    try {
      render(
        <>
          <UpdateSection />
          <UpdateSection />
        </>,
      );
      // 让 start() 内部 listen()/invoke() 的微任务链跑完——fake timers 只接管
      // setTimeout，不影响 Promise 微任务，多等几轮足够稳。
      await act(async () => {
        await Promise.resolve();
        await Promise.resolve();
        await Promise.resolve();
      });
      expect(
        invokeMock.mock.calls.filter(([cmd]) => cmd === "updater_mark_healthy"),
      ).toHaveLength(0);

      await act(async () => {
        await vi.advanceTimersByTimeAsync(20_000);
      });
      expect(
        invokeMock.mock.calls.filter(([cmd]) => cmd === "updater_mark_healthy"),
      ).toHaveLength(1);
    } finally {
      vi.useRealTimers();
    }
  });

  it("up_to_date → 显示当前版本 · 上次检查 · 已是最新，且可点击手动检查（manual=true）", async () => {
    invokeMock.mockResolvedValueOnce(
      snap(1, "up_to_date", { checked_at: Date.now() }),
    );
    render(<UpdateSection />);
    await waitFor(() =>
      expect(screen.getByText(/已是最新/)).toBeInTheDocument(),
    );
    expect(screen.getByText(/刚刚/)).toBeInTheDocument();

    invokeMock.mockResolvedValueOnce(snap(2, "checking"));
    fireEvent.click(screen.getByRole("button", { name: "检查更新" }));
    expect(invokeMock).toHaveBeenCalledWith("updater_check", {
      manual: true,
    });
    await waitFor(() =>
      expect(screen.getByRole("button", { name: /检查中/ })).toBeDisabled(),
    );
  });

  it("checking → 检查按钮 disabled 且带 spinner", async () => {
    invokeMock.mockResolvedValueOnce(snap(1, "checking"));
    render(<UpdateSection />);
    await waitFor(() =>
      expect(screen.getByRole("button", { name: /检查中/ })).toBeDisabled(),
    );
  });

  it("idle → 显示从未检查", async () => {
    invokeMock.mockResolvedValueOnce(snap(1, "idle"));
    render(<UpdateSection />);
    await waitFor(() =>
      expect(screen.getByText(/从未检查/)).toBeInTheDocument(),
    );
  });

  it("up_to_date → 显示上次检查", async () => {
    invokeMock.mockResolvedValueOnce(
      snap(1, "up_to_date", { checked_at: Date.now() }),
    );
    render(<UpdateSection />);
    await waitFor(() =>
      expect(screen.getByText(/上次检查/)).toBeInTheDocument(),
    );
  });

  it("available → 不显示从未检查", async () => {
    invokeMock.mockResolvedValueOnce(
      snap(1, "available", {
        version: "0.3.0",
        notes: null,
        pub_date: null,
      }),
    );
    render(<UpdateSection />);
    await waitFor(() =>
      expect(screen.getByText(/发现新版本 v0.3.0/)).toBeInTheDocument(),
    );
    expect(screen.queryByText(/从未检查/)).not.toBeInTheDocument();
  });

  it("ready → 不显示从未检查", async () => {
    invokeMock.mockResolvedValueOnce(
      snap(1, "ready", { version: "0.3.0", staged_path: "/tmp/x.app" }),
    );
    render(<UpdateSection />);
    await waitFor(() =>
      expect(screen.getByText(/v0.3.0 已就绪/)).toBeInTheDocument(),
    );
    expect(screen.queryByText(/从未检查/)).not.toBeInTheDocument();
  });

  it("available → 无独立检查按钮，下载键触发 updater_download_and_install", async () => {
    invokeMock.mockResolvedValueOnce(
      snap(1, "available", {
        version: "0.3.0",
        notes: null,
        pub_date: null,
      }),
    );
    render(<UpdateSection />);
    await waitFor(() =>
      expect(screen.getByText(/发现新版本 v0.3.0/)).toBeInTheDocument(),
    );
    expect(
      screen.queryByRole("button", { name: "检查更新" }),
    ).not.toBeInTheDocument();

    invokeMock.mockResolvedValueOnce(
      snap(2, "downloading", { downloaded: 0, total: null }),
    );
    fireEvent.click(screen.getByRole("button", { name: "下载并安装" }));
    expect(invokeMock).toHaveBeenCalledWith("updater_download_and_install");
    await waitFor(() =>
      expect(screen.getByText(/正在下载更新/)).toBeInTheDocument(),
    );
  });

  it("更新说明没有会话上下文：MarkdownBody 不传 sessionId，只靠 B 规则放行图片", async () => {
    markdownBodySpy.mockClear();
    invokeMock.mockResolvedValueOnce(
      snap(1, "available", {
        version: "0.3.0",
        notes: "## Highlights\n\n第二行完整说明",
        pub_date: null,
      }),
    );
    render(<UpdateSection />);

    await waitFor(() => expect(markdownBodySpy).toHaveBeenCalled());
    const props = markdownBodySpy.mock.calls[0][0] as { sessionId?: unknown };
    expect(props.sessionId).toBeUndefined();
  });

  it("available → 用 MarkdownBody 渲染更新说明全文，不使用折叠容器", async () => {
    invokeMock.mockResolvedValueOnce(
      snap(1, "available", {
        version: "0.3.0",
        notes: "## Highlights\n\n**Maintenance release**\n\n第二行完整说明",
        pub_date: null,
      }),
    );
    const { container } = render(<UpdateSection />);

    await waitFor(() =>
      expect(screen.getByText(/发现新版本 v0.3.0/)).toBeInTheDocument(),
    );
    expect(screen.getByText("更新说明")).toBeInTheDocument();
    expect(
      await screen.findByRole("heading", { level: 2, name: "Highlights" }),
    ).toBeInTheDocument();
    expect(screen.queryByText("## Highlights")).not.toBeInTheDocument();
    expect(container.querySelector(".updsec__notes-body")).toHaveTextContent(
      "Highlights Maintenance release 第二行完整说明",
    );
    expect(screen.getByText("Maintenance release").tagName).toBe("STRONG");
    expect(container.querySelector("details")).toBeNull();
    expect(
      screen.getByRole("button", { name: "跳过此版本" }),
    ).toBeInTheDocument();
  });

  it("更新说明里的裸绝对路径不自动出图（P1：非聊天场景默认关闭规则 B）", async () => {
    invokeMock.mockResolvedValueOnce(
      snap(1, "available", {
        version: "0.3.0",
        notes: "详见 /Users/victim/secret.png 这张截图",
        pub_date: null,
      }),
    );
    render(<UpdateSection />);

    await waitFor(() =>
      expect(screen.getByText(/发现新版本 v0.3.0/)).toBeInTheDocument(),
    );
    await waitFor(() =>
      expect(
        screen.getByText(/详见 \/Users\/victim\/secret\.png 这张截图/),
      ).toBeInTheDocument(),
    );
    expect(invokeMock).not.toHaveBeenCalledWith(
      "read_attachment",
      expect.anything(),
    );
  });

  it("available → 跳过键触发 updater_skip_version 并折回 up_to_date", async () => {
    invokeMock.mockResolvedValueOnce(
      snap(1, "available", {
        version: "0.3.0",
        notes: null,
        pub_date: null,
      }),
    );
    render(<UpdateSection />);
    await waitFor(() =>
      expect(screen.getByText(/发现新版本 v0.3.0/)).toBeInTheDocument(),
    );

    invokeMock.mockResolvedValueOnce(snap(2, "up_to_date", { checked_at: 1 }));
    fireEvent.click(screen.getByRole("button", { name: "跳过此版本" }));
    expect(invokeMock).toHaveBeenCalledWith("updater_skip_version", {
      version: "0.3.0",
    });
    await waitFor(() =>
      expect(screen.getByText(/已是最新/)).toBeInTheDocument(),
    );
  });

  it("ready → 显示重启按钮，relaunch 失败时渲染 AL_ERR 信封为可读中文", async () => {
    invokeMock.mockResolvedValueOnce(
      snap(1, "ready", { version: "0.3.0", staged_path: "/tmp/x.app" }),
    );
    render(<UpdateSection />);
    await waitFor(() =>
      expect(
        screen.getByRole("button", { name: "重启以更新" }),
      ).toBeInTheDocument(),
    );

    invokeMock.mockRejectedValueOnce("AL_ERR:updater.relaunch_not_ready");
    fireEvent.click(screen.getByRole("button", { name: "重启以更新" }));
    await waitFor(() =>
      expect(
        screen.getByText("重启安装尚未就绪，请稍后重试"),
      ).toBeInTheDocument(),
    );
  });

  it("ready 不带 last_error → 不渲染错误行/重试提示（防误显）", async () => {
    invokeMock.mockResolvedValueOnce(
      snap(1, "ready", { version: "0.3.0", staged_path: "/tmp/x.app" }),
    );
    render(<UpdateSection />);
    await waitFor(() =>
      expect(
        screen.getByRole("button", { name: "重启以更新" }),
      ).toBeInTheDocument(),
    );
    expect(screen.queryByText(/上次更新未完成/)).not.toBeInTheDocument();
    expect(document.querySelector(".updsec__error")).toBeNull();
  });

  it("ready 带 last_error（U4 换包失败可重试）→ 按钮上方渲染可读错误 + 重试提示，按钮仍可点", async () => {
    invokeMock.mockResolvedValueOnce(
      snap(1, "ready", {
        version: "0.3.0",
        staged_path: "/tmp/x.app",
        last_error:
          'AL_ERR:updater.swap_failed:{"detail":"renameatx_np(RENAME_SWAP) failed: Operation not permitted (os error 1)"}',
      }),
    );
    render(<UpdateSection />);
    await waitFor(() =>
      expect(
        screen.getByText(
          "版本切换失败：renameatx_np(RENAME_SWAP) failed: Operation not permitted (os error 1)",
        ),
      ).toBeInTheDocument(),
    );
    expect(
      screen.getByText("上次更新未完成，可以再试一次。"),
    ).toBeInTheDocument();
    const relaunchBtn = screen.getByRole("button", { name: "重启以更新" });
    expect(relaunchBtn).not.toBeDisabled();
  });

  it("ready + last_error → relaunch 拒绝为同一错误时只渲染一次", async () => {
    const backendError =
      'AL_ERR:updater.swap_failed:{"detail":"injected fault at step: swap"}';
    const renderedError = "版本切换失败：injected fault at step: swap";
    invokeMock.mockResolvedValueOnce(
      snap(1, "ready", {
        version: "0.3.0",
        staged_path: "/tmp/x.app",
        last_error: backendError,
      }),
    );
    render(<UpdateSection />);
    await waitFor(() =>
      expect(screen.getByText(renderedError)).toBeInTheDocument(),
    );

    invokeMock.mockRejectedValueOnce(backendError);
    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: "重启以更新" }));
    });
    expect(screen.getAllByText(renderedError)).toHaveLength(1);
  });

  it("ready + last_error → relaunch 拒绝为不同错误时两条都渲染", async () => {
    const stateError =
      'AL_ERR:updater.swap_failed:{"detail":"injected fault at step: swap"}';
    const renderedStateError = "版本切换失败：injected fault at step: swap";
    const renderedActionError = "检查更新失败，请稍后重试";
    invokeMock.mockResolvedValueOnce(
      snap(1, "ready", {
        version: "0.3.0",
        staged_path: "/tmp/x.app",
        last_error: stateError,
      }),
    );
    render(<UpdateSection />);
    await waitFor(() =>
      expect(screen.getByText(renderedStateError)).toBeInTheDocument(),
    );

    invokeMock.mockRejectedValueOnce("AL_ERR:updater.check_failed");
    fireEvent.click(screen.getByRole("button", { name: "重启以更新" }));
    await waitFor(() =>
      expect(screen.getByText(renderedActionError)).toBeInTheDocument(),
    );
    expect(screen.getByText(renderedStateError)).toBeInTheDocument();
  });

  it("ready → 逃生口：显示「放弃此更新」按钮（点击调用 updater_discard_update）+「检查更新」按钮仍出现且可点", async () => {
    invokeMock.mockResolvedValueOnce(
      snap(1, "ready", { version: "0.3.0", staged_path: "/tmp/x.app" }),
    );
    render(<UpdateSection />);
    await waitFor(() =>
      expect(
        screen.getByRole("button", { name: "重启以更新" }),
      ).toBeInTheDocument(),
    );

    const checkBtn = screen.getByRole("button", { name: "检查更新" });
    expect(checkBtn).not.toBeDisabled();

    const discardBtn = screen.getByRole("button", { name: "放弃此更新" });
    invokeMock.mockResolvedValueOnce(snap(2, "idle"));
    fireEvent.click(discardBtn);
    expect(invokeMock).toHaveBeenCalledWith("updater_discard_update");
  });

  it("ready → discardUpdate 失败时渲染 AL_ERR 信封为可读中文", async () => {
    invokeMock.mockResolvedValueOnce(
      snap(1, "ready", { version: "0.3.0", staged_path: "/tmp/x.app" }),
    );
    render(<UpdateSection />);
    await waitFor(() =>
      expect(
        screen.getByRole("button", { name: "放弃此更新" }),
      ).toBeInTheDocument(),
    );

    invokeMock.mockRejectedValueOnce("AL_ERR:updater.check_failed");
    fireEvent.click(screen.getByRole("button", { name: "放弃此更新" }));
    await waitFor(() =>
      expect(screen.getByText("检查更新失败，请稍后重试")).toBeInTheDocument(),
    );
  });

  it("recovery_offered → 标题+正文说明（v{target_version} 出现在正文而非按钮）+ 换回按钮点击调用 updater_swap_back", async () => {
    invokeMock.mockResolvedValueOnce(
      snap(1, "recovery_offered", {
        bundle_path: "/Applications/AgentLoom.app",
        staged_path: "/Applications/.agentloom-update-abc123/AgentLoom.app",
        target_version: "0.3.0",
      }),
    );
    render(<UpdateSection />);
    await waitFor(() =>
      expect(screen.getByText(/正在运行备份中的旧版/)).toBeInTheDocument(),
    );
    // target_version 出现在说明正文里，不是按钮文案——按钮固定文案「恢复旧
    // 版本并重启」，不该让用户误以为点了会升到 v0.3.0（那是装失败的新版）。
    expect(screen.getByText(/v0\.3\.0 安装后未能正常启动/)).toBeInTheDocument();
    const swapBackBtn = screen.getByRole("button", {
      name: "恢复旧版本并重启",
    });
    expect(swapBackBtn).not.toHaveTextContent("0.3.0");

    invokeMock.mockResolvedValueOnce(undefined);
    fireEvent.click(swapBackBtn);
    expect(invokeMock).toHaveBeenCalledWith("updater_swap_back");
  });

  it("recovery_offered → swapBack 失败时渲染 AL_ERR 信封为可读中文", async () => {
    invokeMock.mockResolvedValueOnce(
      snap(1, "recovery_offered", {
        bundle_path: "/Applications/AgentLoom.app",
        staged_path: "/Applications/.agentloom-update-abc123/AgentLoom.app",
        target_version: "0.3.0",
      }),
    );
    render(<UpdateSection />);
    await waitFor(() =>
      expect(
        screen.getByRole("button", { name: "恢复旧版本并重启" }),
      ).toBeInTheDocument(),
    );

    invokeMock.mockRejectedValueOnce(
      'AL_ERR:updater.swap_failed:{"detail":"renameatx_np failed"}',
    );
    fireEvent.click(screen.getByRole("button", { name: "恢复旧版本并重启" }));
    await waitFor(() =>
      expect(
        screen.getByText("版本切换失败：renameatx_np failed"),
      ).toBeInTheDocument(),
    );
  });

  it("recovery_offered 不带 last_error → 不渲染错误行（防误显）", async () => {
    invokeMock.mockResolvedValueOnce(
      snap(1, "recovery_offered", {
        bundle_path: "/Applications/AgentLoom.app",
        staged_path: "/Applications/.agentloom-update-abc123/AgentLoom.app",
        target_version: "0.3.0",
      }),
    );
    render(<UpdateSection />);
    await waitFor(() =>
      expect(
        screen.getByRole("button", { name: "恢复旧版本并重启" }),
      ).toBeInTheDocument(),
    );
    expect(document.querySelector(".updsec__error")).toBeNull();
  });

  it("recovery_offered 带 last_error（P2 契约断链修复）→ 换回按钮上方渲染可读错误，按钮仍可点", async () => {
    invokeMock.mockResolvedValueOnce(
      snap(1, "recovery_offered", {
        bundle_path: "/Applications/AgentLoom.app",
        staged_path: "/Applications/.agentloom-update-abc123/AgentLoom.app",
        target_version: "0.3.0",
        last_error:
          'AL_ERR:updater.swap_failed:{"detail":"renameatx_np(RENAME_SWAP) failed: Operation not permitted (os error 1)"}',
      }),
    );
    render(<UpdateSection />);
    await waitFor(() =>
      expect(
        screen.getByText(
          "版本切换失败：renameatx_np(RENAME_SWAP) failed: Operation not permitted (os error 1)",
        ),
      ).toBeInTheDocument(),
    );
    const swapBackBtn = screen.getByRole("button", {
      name: "恢复旧版本并重启",
    });
    expect(swapBackBtn).not.toBeDisabled();
  });

  it("recovery_offered + last_error → swapBack 拒绝为同一错误时只渲染一次", async () => {
    const backendError =
      'AL_ERR:updater.swap_failed:{"detail":"injected fault at step: swap back"}';
    const renderedError = "版本切换失败：injected fault at step: swap back";
    invokeMock.mockResolvedValueOnce(
      snap(1, "recovery_offered", {
        bundle_path: "/Applications/AgentLoom.app",
        staged_path: "/Applications/.agentloom-update-abc123/AgentLoom.app",
        target_version: "0.3.0",
        last_error: backendError,
      }),
    );
    render(<UpdateSection />);
    await waitFor(() =>
      expect(screen.getByText(renderedError)).toBeInTheDocument(),
    );

    invokeMock.mockRejectedValueOnce(backendError);
    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: "恢复旧版本并重启" }));
    });
    expect(screen.getAllByText(renderedError)).toHaveLength(1);
  });

  it("recovery_offered + last_error → swapBack 拒绝为不同错误时两条都渲染", async () => {
    const stateError =
      'AL_ERR:updater.swap_failed:{"detail":"injected fault at step: swap back"}';
    const renderedStateError =
      "版本切换失败：injected fault at step: swap back";
    const renderedActionError = "检查更新失败，请稍后重试";
    invokeMock.mockResolvedValueOnce(
      snap(1, "recovery_offered", {
        bundle_path: "/Applications/AgentLoom.app",
        staged_path: "/Applications/.agentloom-update-abc123/AgentLoom.app",
        target_version: "0.3.0",
        last_error: stateError,
      }),
    );
    render(<UpdateSection />);
    await waitFor(() =>
      expect(screen.getByText(renderedStateError)).toBeInTheDocument(),
    );

    invokeMock.mockRejectedValueOnce("AL_ERR:updater.check_failed");
    fireEvent.click(screen.getByRole("button", { name: "恢复旧版本并重启" }));
    await waitFor(() =>
      expect(screen.getByText(renderedActionError)).toBeInTheDocument(),
    );
    expect(screen.getByText(renderedStateError)).toBeInTheDocument();
  });

  it("error → 一行可读错误（AL_ERR 信封经既有解析器渲染）", async () => {
    invokeMock.mockResolvedValueOnce(
      snap(1, "error", {
        msg: "AL_ERR:updater.check_failed",
        checked_at: Date.now(),
      }),
    );
    render(<UpdateSection />);
    await waitFor(() =>
      expect(screen.getByText("检查更新失败，请稍后重试")).toBeInTheDocument(),
    );
  });

  it("error(reopen) → reopen 拒绝为同一错误时只渲染一次", async () => {
    const backendError =
      'AL_ERR:updater.reopen_failed:{"detail":"injected reopen fault"}';
    const renderedError = "未能重新打开新版本，请手动重新打开 AgentLoom";
    invokeMock.mockResolvedValueOnce(
      snap(1, "error", {
        msg: backendError,
        checked_at: Date.now(),
        retry: "reopen",
      }),
    );
    render(<UpdateSection />);
    await waitFor(() =>
      expect(screen.getByText(renderedError)).toBeInTheDocument(),
    );

    invokeMock.mockRejectedValueOnce(backendError);
    await act(async () => {
      fireEvent.click(screen.getByRole("button", { name: "重新打开新版本" }));
    });
    expect(screen.getAllByText(renderedError)).toHaveLength(1);
  });

  it("error(reopen) → reopen 拒绝为不同错误时两条都渲染", async () => {
    const renderedStateError =
      "已完成安装但未能自动重启，请手动重新打开 AgentLoom";
    const renderedActionError = "未能重新打开新版本，请手动重新打开 AgentLoom";
    invokeMock.mockResolvedValueOnce(
      snap(1, "error", {
        msg: "AL_ERR:updater.relaunch_failed",
        checked_at: Date.now(),
        retry: "reopen",
      }),
    );
    render(<UpdateSection />);
    await waitFor(() =>
      expect(screen.getByText(renderedStateError)).toBeInTheDocument(),
    );

    invokeMock.mockRejectedValueOnce("AL_ERR:updater.reopen_failed");
    fireEvent.click(screen.getByRole("button", { name: "重新打开新版本" }));
    await waitFor(() =>
      expect(screen.getByText(renderedActionError)).toBeInTheDocument(),
    );
    expect(screen.getByText(renderedStateError)).toBeInTheDocument();
  });

  it("error(reopen) 显示重新打开新版本按钮，点击只调用 updater_reopen，不检查也不下载", async () => {
    invokeMock.mockResolvedValueOnce(
      snap(1, "error", {
        msg: "AL_ERR:updater.relaunch_failed",
        checked_at: Date.now(),
        retry: "reopen",
      }),
    );
    render(<UpdateSection />);
    const button = await screen.findByRole("button", {
      name: "重新打开新版本",
    });

    invokeMock.mockResolvedValueOnce(undefined);
    fireEvent.click(button);

    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("updater_reopen"),
    );
    expect(invokeMock).not.toHaveBeenCalledWith("updater_check", {
      manual: true,
    });
    expect(invokeMock).not.toHaveBeenCalledWith("updater_download_and_install");
  });

  it("error 缺省 retry 点击检查更新按钮 → 仍调用 updater_check，不误走 reopen", async () => {
    invokeMock.mockResolvedValueOnce(
      snap(1, "error", {
        msg: "AL_ERR:updater.check_failed",
        checked_at: Date.now(),
      }),
    );
    render(<UpdateSection />);
    const button = await screen.findByRole("button", { name: "检查更新" });

    invokeMock.mockResolvedValueOnce(snap(2, "checking"));
    fireEvent.click(button);

    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("updater_check", {
        manual: true,
      }),
    );
    expect(invokeMock).not.toHaveBeenCalledWith("updater_reopen");
  });
});

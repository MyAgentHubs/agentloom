import {
  act,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { invoke } from "@tauri-apps/api/core";
import { SettingsSearch } from "./SettingsSearch";

declare const process: { env: { VITEST_DEFER_INVOKE?: string } };

vi.mock("@tauri-apps/api/core", () => {
  const base = vi.fn();
  // VITEST_DEFER_INVOKE=1 makes every invoke settle one macrotask later, which
  // deterministically exposes assertions that read state landing from a *different*
  // async source than the one they awaited. CI runners are ~12x slower than a dev
  // machine and lose those races for real; this switch reproduces it on purpose.
  return {
    invoke: process.env.VITEST_DEFER_INVOKE
      ? new Proxy(base, {
          apply: (t, self, args) =>
            new Promise((r) => setTimeout(r, 0)).then(() =>
              Reflect.apply(t, self, args),
            ),
        })
      : base,
  };
});
vi.mock("@tauri-apps/plugin-opener", () => ({ openUrl: vi.fn() }));

describe("SettingsSearch", () => {
  const invokeMock = vi.mocked(invoke);

  beforeEach(() => {
    invokeMock.mockReset();
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "get_active_backend") return "brave";
      return undefined;
    });
  });

  function callsFor(command: string) {
    return invokeMock.mock.calls.filter(([cmd]) => cmd === command);
  }

  it("挂载只读 active backend·不读 keychain·显示未检查", async () => {
    render(<SettingsSearch />);

    expect(screen.getByText("未检查")).toBeInTheDocument();
    await waitFor(() => {
      expect(callsFor("get_active_backend")).toHaveLength(1);
    });
    expect(callsFor("get_search_key")).toHaveLength(0);
  });

  it("挂载读取 duckduckgo 后选中 DuckDuckGo", async () => {
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "get_active_backend") return "duckduckgo";
      return undefined;
    });

    render(<SettingsSearch />);

    await waitFor(() => {
      expect(screen.getByRole("radio", { name: "DuckDuckGo" })).toHaveAttribute(
        "aria-checked",
        "true",
      );
    });
    expect(callsFor("get_search_key")).toHaveLength(0);
  });

  it("挂载读取 active backend 失败时静默回退 Brave", async () => {
    invokeMock.mockRejectedValue(new Error("boom"));

    render(<SettingsSearch />);

    await waitFor(() => {
      expect(callsFor("get_active_backend")).toHaveLength(1);
    });
    expect(screen.getByRole("radio", { name: "Brave" })).toHaveAttribute(
      "aria-checked",
      "true",
    );
    expect(screen.queryByText("切换失败，请稍后重试")).not.toBeInTheDocument();
  });

  it("挂载读取返回前手动切换时不覆盖用户选择", async () => {
    let resolveActiveBackend: ((value: string) => void) | undefined;
    invokeMock.mockImplementation((cmd: string) => {
      if (cmd === "get_active_backend") {
        return new Promise<string>((resolve) => {
          resolveActiveBackend = resolve;
        });
      }
      return Promise.resolve(undefined);
    });

    render(<SettingsSearch />);
    fireEvent.click(screen.getByRole("radio", { name: "Exa" }));

    await waitFor(() => {
      expect(resolveActiveBackend).toBeDefined();
    });
    await act(async () => {
      resolveActiveBackend?.("duckduckgo");
    });

    expect(screen.getByRole("radio", { name: "Exa" })).toHaveAttribute(
      "aria-checked",
      "true",
    );
  });

  it("点检查→只读 key 状态·未配置", async () => {
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "get_active_backend") return "brave";
      if (cmd === "get_search_key") return false;
      return undefined;
    });

    render(<SettingsSearch />);

    fireEvent.click(screen.getByRole("button", { name: "检查" }));

    expect(await screen.findByText("未配置")).toBeInTheDocument();
    expect(callsFor("get_active_backend")).toHaveLength(1);
    expect(invokeMock).toHaveBeenCalledWith("get_search_key", {
      backend: "brave",
    });
  });

  it("点检查→已配置", async () => {
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "get_active_backend") return "brave";
      if (cmd === "get_search_key") return true;
      return undefined;
    });

    render(<SettingsSearch />);
    fireEvent.click(screen.getByRole("button", { name: "检查" }));

    expect(await screen.findByText("已配置")).toBeInTheDocument();
  });

  it("Brave·填 key·点测试→ok 显连接正常", async () => {
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "test_search_service") return { ok: true, category: "ok" };
      return undefined;
    });

    render(<SettingsSearch />);

    expect(screen.getByRole("radio", { name: "Brave" })).toHaveAttribute(
      "aria-checked",
      "true",
    );
    expect(
      screen.getByRole("radio", { name: "SearXNG（即将）" }),
    ).toBeDisabled();

    fireEvent.change(screen.getByLabelText("API Key"), {
      target: { value: "brave-key" },
    });
    fireEvent.click(screen.getByRole("button", { name: "测试连接" }));

    expect(await screen.findByText("连接正常")).toBeInTheDocument();
    expect(invokeMock).toHaveBeenCalledWith("test_search_service", {
      backend: "brave",
      apiKey: "brave-key",
    });
    expect(invokeMock).not.toHaveBeenCalledWith(
      "get_search_key",
      expect.anything(),
    );
    expect(callsFor("get_active_backend")).toHaveLength(1);
  });

  it("测试返回 auth → 显 key 无效", async () => {
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "test_search_service") {
        return { ok: false, category: "auth" };
      }
      return undefined;
    });

    render(<SettingsSearch />);
    fireEvent.change(screen.getByLabelText("API Key"), {
      target: { value: "bad-key" },
    });
    fireEvent.click(screen.getByRole("button", { name: "测试连接" }));

    expect(await screen.findByText("key 无效或无权")).toBeInTheDocument();
    expect(invokeMock).toHaveBeenCalledWith("test_search_service", {
      backend: "brave",
      apiKey: "bad-key",
    });
  });

  it("保存 key 成功后回读 active backend", async () => {
    let activeReads = 0;
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "get_active_backend") {
        activeReads += 1;
        return activeReads === 1 ? "brave" : "exa";
      }
      return undefined;
    });

    render(<SettingsSearch />);
    fireEvent.change(screen.getByLabelText("API Key"), {
      target: { value: "save-key" },
    });
    fireEvent.click(screen.getByRole("button", { name: "保存" }));

    expect(await screen.findByText("已保存")).toBeInTheDocument();
    expect(invokeMock).toHaveBeenCalledWith("set_search_key", {
      backend: "brave",
      key: "save-key",
    });
    expect(callsFor("get_active_backend")).toHaveLength(2);
    expect(screen.getByRole("radio", { name: "Exa" })).toHaveAttribute(
      "aria-checked",
      "true",
    );
    expect(invokeMock.mock.calls.map(([command]) => command)).toEqual([
      "get_active_backend",
      "set_search_key",
      "get_active_backend",
    ]);
  });

  it("有「去 Brave 注册 key」外链", async () => {
    render(<SettingsSearch />);

    const link = screen.getByRole("link", { name: "去 Brave 注册 key" });
    expect(link).toHaveAttribute("href", "https://brave.com/search/api");
    await waitFor(() => {
      expect(callsFor("get_active_backend")).toHaveLength(1);
    });
  });

  it("选 Exa → 注册链接变 Exa·保存带 backend=exa", async () => {
    render(<SettingsSearch />);
    fireEvent.click(screen.getByRole("radio", { name: "Exa" }));
    fireEvent.change(screen.getByLabelText("API Key"), {
      target: { value: "exa-key" },
    });
    fireEvent.click(screen.getByRole("button", { name: "保存" }));
    expect(await screen.findByText("已保存")).toBeInTheDocument();
    expect(invokeMock).toHaveBeenCalledWith("set_search_key", {
      backend: "exa",
      key: "exa-key",
    });
  });

  it("选 Exa → 测试带 backend=exa·注册链接变 Exa", async () => {
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "test_search_service") return { ok: true, category: "ok" };
      return undefined;
    });
    render(<SettingsSearch />);
    fireEvent.click(screen.getByRole("radio", { name: "Exa" }));
    fireEvent.change(screen.getByLabelText("API Key"), {
      target: { value: "ek" },
    });
    fireEvent.click(screen.getByRole("button", { name: "测试连接" }));
    expect(await screen.findByText("连接正常")).toBeInTheDocument();
    expect(invokeMock).toHaveBeenCalledWith("test_search_service", {
      backend: "exa",
      apiKey: "ek",
    });
    expect(
      screen.getByRole("link", { name: "去 Exa 注册 key" }),
    ).toHaveAttribute("href", "https://exa.ai");
  });

  it("切换 backend 后状态回到未检查·不自动读 keychain", async () => {
    render(<SettingsSearch />);
    fireEvent.click(screen.getByRole("radio", { name: "Exa" }));

    expect(screen.getByText("未检查")).toBeInTheDocument();
    await waitFor(() => {
      expect(callsFor("get_active_backend")).toHaveLength(1);
    });
    expect(callsFor("get_search_key")).toHaveLength(0);
  });

  it("先手选 backend 再点检查→不重复拉 active backend·不被静默改回", async () => {
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "get_active_backend") return "brave";
      if (cmd === "get_search_key") return true;
      return undefined;
    });

    render(<SettingsSearch />);

    fireEvent.click(screen.getByRole("radio", { name: "Exa" }));

    fireEvent.click(screen.getByRole("button", { name: "检查" }));

    expect(await screen.findByText("已配置")).toBeInTheDocument();
    expect(callsFor("get_active_backend")).toHaveLength(1);
    expect(invokeMock).toHaveBeenCalledWith("get_search_key", {
      backend: "exa",
    });
    expect(screen.getByRole("radio", { name: "Exa" })).toHaveAttribute(
      "aria-checked",
      "true",
    );
  });

  it("页首说明文案渲染", async () => {
    render(<SettingsSearch />);

    expect(
      screen.getByText(
        "不是每个模型都自带联网搜索。AgentLoom 接入第三方搜索服务，让任何 agent 都能查网页。DuckDuckGo 开箱即用、无需 key；配置 Brave 或 Exa API key 可获得更高质量的搜索结果。",
      ),
    ).toBeInTheDocument();
    await waitFor(() => {
      expect(callsFor("get_active_backend")).toHaveLength(1);
    });
  });

  it("默认 Brave·渲染 key 输入 + 保存说明·不显示 DuckDuckGo 说明", async () => {
    render(<SettingsSearch />);

    expect(screen.getByLabelText("API Key")).toBeInTheDocument();
    expect(
      screen.getByText("保存后 key 存入系统钥匙串，并将该服务设为当前使用。"),
    ).toBeInTheDocument();
    expect(
      screen.queryByText("DuckDuckGo 无需 API key。"),
    ).not.toBeInTheDocument();
    await waitFor(() => {
      expect(callsFor("get_active_backend")).toHaveLength(1);
    });
  });

  it("选 DuckDuckGo → 隐藏 key 输入·显示无需 API key 说明·隐藏状态检查区", async () => {
    render(<SettingsSearch />);

    fireEvent.click(screen.getByRole("radio", { name: "DuckDuckGo" }));

    expect(screen.queryByLabelText("API Key")).not.toBeInTheDocument();
    expect(screen.getByText("DuckDuckGo 无需 API key。")).toBeInTheDocument();
    expect(screen.queryByText("未检查")).not.toBeInTheDocument();
    expect(
      screen.queryByRole("button", { name: "检查" }),
    ).not.toBeInTheDocument();
    await waitFor(() => {
      expect(callsFor("get_active_backend")).toHaveLength(1);
    });
    expect(callsFor("get_search_key")).toHaveLength(0);
  });

  it("选 DuckDuckGo 本身不触发额外 invoke·只有点「设为当前」才发", async () => {
    render(<SettingsSearch />);

    fireEvent.click(screen.getByRole("radio", { name: "DuckDuckGo" }));

    expect(
      screen.getByRole("button", { name: "设为当前" }),
    ).toBeInTheDocument();
    await waitFor(() => {
      expect(callsFor("get_active_backend")).toHaveLength(1);
    });
    expect(callsFor("set_active_search_backend")).toHaveLength(0);
  });

  it("选 DuckDuckGo → 点「设为当前」→ 写后回读匹配才显示成功反馈", async () => {
    let activeReads = 0;
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "get_active_backend") {
        activeReads += 1;
        return activeReads === 1 ? "brave" : "duckduckgo";
      }
      if (cmd === "set_active_search_backend") return undefined;
      return undefined;
    });

    render(<SettingsSearch />);

    fireEvent.click(screen.getByRole("radio", { name: "DuckDuckGo" }));
    fireEvent.click(screen.getByRole("button", { name: "设为当前" }));

    expect(
      await screen.findByText("已切换为 DuckDuckGo。"),
    ).toBeInTheDocument();
    expect(invokeMock).toHaveBeenCalledWith("set_active_search_backend", {
      backend: "duckduckgo",
    });
    expect(invokeMock.mock.calls.map(([command]) => command)).toEqual([
      "get_active_backend",
      "set_active_search_backend",
      "get_active_backend",
    ]);
  });

  it("选 DuckDuckGo → 点「设为当前」失败 → 显示切换失败提示", async () => {
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "set_active_search_backend") {
        throw new Error("boom");
      }
      return undefined;
    });

    render(<SettingsSearch />);

    fireEvent.click(screen.getByRole("radio", { name: "DuckDuckGo" }));
    fireEvent.click(screen.getByRole("button", { name: "设为当前" }));

    expect(await screen.findByText("切换失败，请稍后重试")).toBeInTheDocument();
  });
});

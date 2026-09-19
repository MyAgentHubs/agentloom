import {
  fireEvent,
  render,
  screen,
  waitFor,
  within,
} from "@testing-library/react";
import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { invoke } from "@tauri-apps/api/core";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import { openUrl } from "@tauri-apps/plugin-opener";
import { AgentForm } from "./AgentForm";
import {
  agent,
  detectReady,
  clickEngine,
  providerChip,
  openMoreOptions,
} from "./AgentForm.test-helpers";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/plugin-dialog", () => ({ open: vi.fn() }));
vi.mock("@tauri-apps/plugin-opener", () => ({ openUrl: vi.fn() }));

describe("AgentForm", () => {
  const invokeMock = vi.mocked(invoke);

  beforeEach(() => {
    invokeMock.mockReset();
    invokeMock.mockResolvedValue(undefined);
    vi.mocked(openDialog).mockReset();
    vi.mocked(openDialog).mockResolvedValue(null);
    vi.mocked(openUrl).mockReset();
    vi.mocked(openUrl).mockResolvedValue(undefined);
    localStorage.clear();
  });

  afterEach(() => {
    vi.restoreAllMocks();
  });

  it("引擎未安装时用键盘可达按钮打开对应安装指引", async () => {
    invokeMock.mockImplementation((cmd: string) => {
      if (cmd === "detect_runtime") {
        return Promise.resolve({
          claude: { available: false, creds_hint: null },
          codex: { available: false, creds_hint: null },
        });
      }
      return Promise.resolve();
    });

    render(<AgentForm agent={null} onCancel={vi.fn()} onSaved={vi.fn()} />);

    const installGuide = await screen.findByRole("button", {
      name: "Codex CLI 安装指引",
    });
    expect(
      screen.queryByRole("link", { name: "Codex CLI 安装指引" }),
    ).toBeNull();

    fireEvent.click(installGuide);

    expect(openUrl).toHaveBeenCalledWith("https://github.com/openai/codex");
  });

  it("native CLI 未安装时查看安装指引按钮打开对应官方页面", async () => {
    invokeMock.mockImplementation((cmd: string) => {
      if (cmd === "detect_runtime") {
        return Promise.resolve({
          claude: { available: false, creds_hint: null },
          codex: { available: true, creds_hint: true },
        });
      }
      return Promise.resolve();
    });

    render(<AgentForm agent={null} onCancel={vi.fn()} onSaved={vi.fn()} />);
    fireEvent.click(providerChip("Anthropic 账号"));

    fireEvent.click(
      await screen.findByRole("button", { name: "查看安装指引" }),
    );

    expect(openUrl).toHaveBeenCalledWith("https://claude.com/claude-code");
  });

  it("native CLI 未检测到时仍可保存并显示非阻塞警告", async () => {
    invokeMock.mockImplementation((cmd: string) => {
      if (cmd === "detect_runtime") {
        return Promise.resolve({
          claude: {
            available: false,
            version: null,
            path: null,
            creds_hint: null,
          },
          codex: {
            available: true,
            version: null,
            path: null,
            creds_hint: false,
          },
        });
      }
      return Promise.resolve();
    });

    const { unmount } = render(
      <AgentForm onCancel={vi.fn()} onSaved={vi.fn()} />,
    );

    fireEvent.click(providerChip("Anthropic 账号"));

    expect(await screen.findByText("⚠ 未检测到 claude CLI")).toHaveStyle({
      color: "var(--amber-ink)",
    });
    expect(
      screen.getByText(
        "未检测到 claude CLI —— 仍然可以保存，但这个 agent 装好之前跑不起来。",
      ),
    ).toHaveStyle({ color: "var(--amber-ink)" });
    expect(screen.getByRole("button", { name: "添加" })).not.toBeDisabled();

    clickEngine("Codex CLI");

    expect(await screen.findByText(/未探测到登录凭据/)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "添加" })).not.toBeDisabled();

    unmount();
    invokeMock.mockImplementation((cmd: string) => {
      if (cmd === "detect_runtime") {
        return Promise.resolve({
          claude: {
            available: true,
            version: null,
            path: null,
            creds_hint: true,
          },
          codex: {
            available: false,
            version: null,
            path: null,
            creds_hint: null,
          },
        });
      }
      return Promise.resolve();
    });

    render(<AgentForm onCancel={vi.fn()} onSaved={vi.fn()} />);
    clickEngine("Codex CLI");

    expect(
      await screen.findByText(
        "如果刚装好 Codex CLI，重开一次 AgentLoom 可能会有帮助；还没安装的话，请查看安装指引。",
      ),
    ).toBeInTheDocument();
    expect(screen.queryByText(/Claude 桌面版 App/)).toBeNull();
    expect(screen.getByRole("button", { name: "添加" })).not.toBeDisabled();
  });

  it("指定 CLI 路径后调用 set_cli_path 并用返回结果刷新界面", async () => {
    const path = "/opt/custom/bin/claude";
    vi.mocked(openDialog).mockResolvedValue(path);
    invokeMock.mockImplementation((cmd: string) => {
      if (cmd === "detect_runtime") {
        return Promise.resolve({
          claude: {
            available: false,
            version: null,
            path: null,
            creds_hint: null,
            overridden: false,
          },
          codex: {
            available: true,
            version: null,
            path: "/usr/local/bin/codex",
            creds_hint: true,
            overridden: false,
          },
        });
      }
      if (cmd === "set_cli_path") {
        return Promise.resolve({
          claude: {
            available: true,
            version: null,
            path,
            creds_hint: true,
            overridden: true,
          },
          codex: {
            available: true,
            version: null,
            path: "/usr/local/bin/codex",
            creds_hint: true,
            overridden: false,
          },
        });
      }
      return Promise.resolve();
    });

    render(<AgentForm onCancel={vi.fn()} onSaved={vi.fn()} />);
    fireEvent.click(providerChip("Anthropic 账号"));
    fireEvent.click(await screen.findByRole("button", { name: "指定路径…" }));

    await waitFor(() => expect(openDialog).toHaveBeenCalledTimes(1));
    expect(openDialog).toHaveBeenCalledWith({
      directory: false,
      multiple: false,
    });
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("set_cli_path", {
        cli: "claude",
        path,
      }),
    );
    expect(await screen.findByText("✓ 已指定路径")).toBeInTheDocument();
    expect(screen.getByText(path)).toBeInTheDocument();
    expect(
      screen.queryByText(
        "未检测到 claude CLI —— 仍然可以保存，但这个 agent 装好之前跑不起来。",
      ),
    ).toBeNull();
  });

  it("set_cli_path 失败时显示后端错误且保留未检测到状态", async () => {
    const backendError =
      'AL_ERR:runtime.invalidCliPath:{"detail":"not an executable file"}';
    vi.mocked(openDialog).mockResolvedValue("/tmp/not-a-cli.txt");
    invokeMock.mockImplementation((cmd: string) => {
      if (cmd === "detect_runtime") {
        return Promise.resolve({
          claude: {
            available: false,
            path: null,
            creds_hint: null,
            overridden: false,
          },
          codex: {
            available: true,
            path: "/usr/local/bin/codex",
            creds_hint: true,
            overridden: false,
          },
        });
      }
      if (cmd === "set_cli_path") return Promise.reject(backendError);
      return Promise.resolve();
    });

    render(<AgentForm onCancel={vi.fn()} onSaved={vi.fn()} />);
    fireEvent.click(providerChip("Anthropic 账号"));
    fireEvent.click(await screen.findByRole("button", { name: "指定路径…" }));

    expect(await screen.findByText(backendError)).toBeInTheDocument();
    expect(screen.getByText("⚠ 未检测到 claude CLI")).toBeInTheDocument();
    expect(screen.queryByText("✓ 已指定路径")).toBeNull();
  });

  it("手动指定的 CLI 路径失效且后端未返回路径时不渲染空路径行", async () => {
    invokeMock.mockImplementation((cmd: string) => {
      if (cmd === "detect_runtime") {
        return Promise.resolve({
          claude: {
            available: false,
            path: null,
            creds_hint: null,
            overridden: true,
          },
          codex: {
            available: true,
            path: "/usr/local/bin/codex",
            creds_hint: true,
            overridden: false,
          },
        });
      }
      return Promise.resolve();
    });

    render(<AgentForm onCancel={vi.fn()} onSaved={vi.fn()} />);
    fireEvent.click(providerChip("Anthropic 账号"));

    expect(
      await screen.findByText("⚠ 你指定的 claude CLI 路径用不了"),
    ).toBeInTheDocument();
    expect(screen.queryByTestId("native-runtime-path")).not.toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: "指定路径…" }),
    ).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "清除" })).toBeInTheDocument();
    expect(screen.queryByText("⚠ 未检测到 claude CLI")).toBeNull();
    expect(
      screen.queryByText(
        "需要的是 Claude Code 命令行工具（不是 Claude 桌面版 App）。刚装好的话，重开一次 AgentLoom 可能会有帮助；还没安装的话，请查看安装指引。",
      ),
    ).toBeNull();
  });

  it("手动指定的 CLI 路径失效但后端返回路径时显示该路径", async () => {
    const path = "C:\\x\\claude.exe";
    invokeMock.mockImplementation((cmd: string) => {
      if (cmd === "detect_runtime") {
        return Promise.resolve({
          claude: {
            available: false,
            path,
            creds_hint: null,
            overridden: true,
          },
          codex: {
            available: true,
            path: "C:\\x\\codex.exe",
            creds_hint: true,
            overridden: false,
          },
        });
      }
      return Promise.resolve();
    });

    render(<AgentForm onCancel={vi.fn()} onSaved={vi.fn()} />);
    fireEvent.click(providerChip("Anthropic 账号"));

    expect(
      await screen.findByText("⚠ 你指定的 claude CLI 路径用不了"),
    ).toBeInTheDocument();
    expect(screen.getByTestId("native-runtime-path")).toHaveTextContent(path);
  });

  it("已指定可用 CLI 路径时显示路径并可清除 override", async () => {
    const path = "/Applications/Claude CLI/bin/claude";
    invokeMock.mockImplementation((cmd: string) => {
      if (cmd === "detect_runtime") {
        return Promise.resolve({
          claude: {
            available: true,
            path,
            creds_hint: true,
            overridden: true,
          },
          codex: {
            available: true,
            path: "/usr/local/bin/codex",
            creds_hint: true,
            overridden: false,
          },
        });
      }
      if (cmd === "set_cli_path") return Promise.resolve(detectReady());
      return Promise.resolve();
    });

    render(<AgentForm onCancel={vi.fn()} onSaved={vi.fn()} />);
    fireEvent.click(providerChip("Anthropic 账号"));

    expect(await screen.findByText("✓ 已指定路径")).toBeInTheDocument();
    expect(screen.getByTestId("native-runtime-path")).toHaveTextContent(path);
    const clearButton = screen.getByRole("button", { name: "清除" });
    expect(clearButton).toBeEnabled();
    fireEvent.click(clearButton);

    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("set_cli_path", {
        cli: "claude",
        path: null,
      }),
    );
  });

  it("取消 CLI 文件选择时不调用 set_cli_path", async () => {
    vi.mocked(openDialog).mockResolvedValue(null);
    invokeMock.mockImplementation((cmd: string) => {
      if (cmd === "detect_runtime") {
        return Promise.resolve({
          claude: {
            available: false,
            path: null,
            creds_hint: null,
            overridden: false,
          },
          codex: {
            available: true,
            path: "/usr/local/bin/codex",
            creds_hint: true,
            overridden: false,
          },
        });
      }
      return Promise.resolve();
    });

    render(<AgentForm onCancel={vi.fn()} onSaved={vi.fn()} />);
    fireEvent.click(providerChip("Anthropic 账号"));
    fireEvent.click(await screen.findByRole("button", { name: "指定路径…" }));

    await waitFor(() => expect(openDialog).toHaveBeenCalledTimes(1));
    expect(invokeMock.mock.calls.some(([cmd]) => cmd === "set_cli_path")).toBe(
      false,
    );
  });

  it("native CLI 未检测到时提交仍会保存", async () => {
    invokeMock.mockImplementation((cmd: string) => {
      if (cmd === "detect_runtime") {
        return Promise.resolve({
          claude: {
            available: false,
            version: null,
            path: null,
            creds_hint: null,
          },
          codex: {
            available: true,
            version: null,
            path: null,
            creds_hint: true,
          },
        });
      }
      return Promise.resolve();
    });
    const onSaved = vi.fn();

    render(<AgentForm onCancel={vi.fn()} onSaved={onSaved} />);
    fireEvent.click(providerChip("Anthropic 账号"));

    await screen.findByText("⚠ 未检测到 claude CLI");
    fireEvent.click(screen.getByRole("button", { name: "添加" }));

    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("upsert_agent", {
        profile: expect.objectContaining({
          access: "native",
          provider: "claude",
        }),
      }),
    );
    expect(onSaved).toHaveBeenCalledTimes(1);
  });

  it("Claude CLI 未检测到时说明需要命令行工具而不是桌面版 App", async () => {
    invokeMock.mockImplementation((cmd: string) => {
      if (cmd === "detect_runtime") {
        return Promise.resolve({
          claude: {
            available: false,
            version: null,
            path: null,
            creds_hint: null,
          },
          codex: {
            available: true,
            version: null,
            path: null,
            creds_hint: true,
          },
        });
      }
      return Promise.resolve();
    });

    render(<AgentForm onCancel={vi.fn()} onSaved={vi.fn()} />);
    fireEvent.click(providerChip("Anthropic 账号"));

    expect(
      await screen.findByText(
        "需要的是 Claude Code 命令行工具（不是 Claude 桌面版 App）。刚装好的话，重开一次 AgentLoom 可能会有帮助；还没安装的话，请查看安装指引。",
      ),
    ).toBeInTheDocument();
  });

  it("save_without_key_skips_set_key", async () => {
    vi.spyOn(Date, "now").mockReturnValue(1_700_000_000_000);
    invokeMock.mockResolvedValue(undefined);
    const onSaved = vi.fn();

    render(
      <AgentForm
        agent={agent({ has_key: true })}
        onCancel={vi.fn()}
        onSaved={onSaved}
        nextSortOrder={3}
      />,
    );

    fireEvent.change(screen.getByLabelText("名称"), {
      target: { value: "DeepSeek Main" },
    });
    fireEvent.click(screen.getByRole("button", { name: "保存" }));

    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("upsert_agent", {
        profile: expect.objectContaining({
          id: "deepseek",
          name: "DeepSeek Main",
          access: "borrow",
        }),
      }),
    );
    expect(invokeMock.mock.calls.some(([cmd]) => cmd === "set_agent_key")).toBe(
      false,
    );
    expect(onSaved).toHaveBeenCalledTimes(1);
  });

  it("新增 Claude CLI 原生 agent 可设置默认 reasoning 且不写 key", async () => {
    vi.spyOn(Date, "now").mockReturnValue(1_700_000_000_000);
    invokeMock.mockResolvedValue(undefined);

    render(
      <AgentForm onCancel={vi.fn()} onSaved={vi.fn()} nextSortOrder={3} />,
    );

    fireEvent.click(providerChip("Anthropic 账号"));
    expect(screen.queryByLabelText("API Key")).not.toBeInTheDocument();
    expect(screen.queryByTestId("test-conn-btn")).not.toBeInTheDocument();

    openMoreOptions();
    const reasoning = screen.getByLabelText("reasoning 默认档");
    expect(
      within(reasoning).getByRole("button", { name: "xhigh" }),
    ).toBeInTheDocument();
    expect(
      within(reasoning).getByRole("button", { name: "max" }),
    ).toBeInTheDocument();
    fireEvent.click(within(reasoning).getByRole("button", { name: "high" }));
    fireEvent.click(screen.getByRole("button", { name: "添加" }));

    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("upsert_agent", {
        profile: expect.objectContaining({
          id: "claude-cli",
          name: "Claude CLI",
          access: "native",
          provider: "claude",
          primary_model: "sonnet",
          endpoint: null,
          auth_mode: null,
          reasoning_default: "high",
          cap_reasoning: "low,medium,high,xhigh,max",
          cap_lead: "native_cli",
          sort_order: 3,
        }),
      }),
    );
    expect(invokeMock.mock.calls.some(([cmd]) => cmd === "set_agent_key")).toBe(
      false,
    );
  });

  it("新增 Codex CLI 原生 agent 使用 codex reasoning 能力", async () => {
    invokeMock.mockResolvedValue(undefined);

    render(<AgentForm onCancel={vi.fn()} onSaved={vi.fn()} />);

    fireEvent.click(screen.getByRole("button", { name: "Codex CLI" }));
    openMoreOptions();
    const reasoning = screen.getByLabelText("reasoning 默认档");

    expect(
      within(reasoning).getByRole("button", { name: "minimal" }),
    ).toBeInTheDocument();
    expect(
      within(reasoning).getByRole("button", { name: "xhigh" }),
    ).toBeInTheDocument();
    expect(within(reasoning).queryByRole("button", { name: "max" })).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "添加" }));

    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("upsert_agent", {
        profile: expect.objectContaining({
          name: "Codex CLI",
          access: "native",
          provider: "codex",
          primary_model: "gpt-5",
          cap_reasoning: "minimal,low,medium,high,xhigh",
          cap_lead: null,
        }),
      }),
    );
  });

  it("编辑 native agent 只保留模型和 reasoning，不暴露远端接入配置", async () => {
    invokeMock.mockResolvedValue(undefined);

    render(
      <AgentForm
        agent={agent({
          id: "claude",
          name: "Claude Code",
          access: "native",
          provider: "claude",
          endpoint: null,
          cap_lead: "native_cli",
        })}
        onCancel={vi.fn()}
        onSaved={vi.fn()}
      />,
    );

    expect(screen.queryByText(/有自己的 CLI/)).not.toBeInTheDocument();
    expect(screen.queryByLabelText("引擎")).not.toBeInTheDocument();
    expect(screen.queryByLabelText("LLM Provider")).not.toBeInTheDocument();
    expect(screen.getByLabelText("名称")).toHaveValue("Claude Code");
    expect(screen.queryByLabelText("API Key")).not.toBeInTheDocument();
    expect(screen.queryByTestId("test-conn-btn")).not.toBeInTheDocument();
    expect(screen.queryByTestId("test-state")).not.toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: /更多选项/ }),
    ).toBeInTheDocument();
    expect(screen.getByRole("button", { name: /更多选项/ })).toHaveAttribute(
      "aria-expanded",
      "true",
    );
    expect(screen.queryByLabelText("Endpoint")).not.toBeInTheDocument();
    expect(screen.queryByLabelText("鉴权方式")).not.toBeInTheDocument();
    expect(screen.queryByLabelText("模型映射")).not.toBeInTheDocument();
    expect(screen.getByLabelText("模型")).toBeInTheDocument();
    expect(screen.getByLabelText("reasoning 默认档")).toBeInTheDocument();

    fireEvent.change(screen.getByLabelText("名称"), {
      target: { value: "我的 Claude" },
    });
    fireEvent.click(screen.getByRole("button", { name: "保存" }));

    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("upsert_agent", {
        profile: expect.objectContaining({
          id: "claude",
          name: "我的 Claude",
          access: "native",
          endpoint: null,
          cap_lead: "native_cli",
        }),
      }),
    );
  });

  it("编辑 native agent 可保存空模型作为 CLI 默认", async () => {
    invokeMock.mockResolvedValue(undefined);

    render(
      <AgentForm
        agent={agent({
          id: "codex",
          name: "Codex",
          access: "native",
          provider: "codex",
          primary_model: null,
          endpoint: null,
        })}
        onCancel={vi.fn()}
        onSaved={vi.fn()}
      />,
    );

    expect(
      screen.getByRole("button", { name: /CLI 默认/ }),
    ).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "保存" }));

    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("upsert_agent", {
        profile: expect.objectContaining({
          access: "native",
          provider: "codex",
          primary_model: null,
        }),
      }),
    );
  });

  it("native agent 不显示测试连接按钮", () => {
    render(
      <AgentForm
        agent={agent({
          id: "claude",
          access: "native",
          provider: "claude",
          endpoint: null,
        })}
        onCancel={vi.fn()}
        onSaved={vi.fn()}
      />,
    );

    expect(screen.queryByTestId("test-conn-btn")).not.toBeInTheDocument();
  });
});

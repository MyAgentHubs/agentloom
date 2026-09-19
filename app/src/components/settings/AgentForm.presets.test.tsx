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
import { I18nProvider } from "../../i18n";
import { AgentForm } from "./AgentForm";
import { autoAgentName } from "./agentFormHelpers";
import {
  agent,
  detectReady,
  engineRegion,
  clickEngine,
  providerRegion,
  providerChip,
  borrowPreset,
  clickBorrowPreset,
  clickHarnessPreset,
  passConnectionTest,
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

  it("en locale renders managed form chrome without hardcoded Chinese", () => {
    render(
      <I18nProvider initialLocale="en">
        <AgentForm agent={null} onCancel={vi.fn()} onSaved={vi.fn()} />
      </I18nProvider>,
    );

    expect(
      screen.getByRole("form", { name: "Add / edit agent" }),
    ).toBeInTheDocument();
    expect(screen.getByText("Add agent")).toBeInTheDocument();
    expect(screen.getByText("Basic")).toBeInTheDocument();
    expect(screen.getByLabelText("Engine")).toBeInTheDocument();
    expect(screen.getByText(/Local claude command/i)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Custom" })).toBeInTheDocument();
    expect(screen.queryByText("添加 agent")).toBeNull();
    expect(screen.queryByText("基础")).toBeNull();
    expect(screen.queryByLabelText("引擎")).toBeNull();
    expect(screen.queryByText(/本机|自研|自定义/)).toBeNull();

    fireEvent.click(
      within(screen.getByLabelText("Engine")).getByRole("button", {
        name: "myagent",
      }),
    );

    expect(screen.getByText(/Custom harness/i)).toBeInTheDocument();
    expect(screen.queryByText(/本机|自研|自定义/)).toBeNull();
  });

  it("基础表单无接入方式 toggle", () => {
    render(<AgentForm agent={null} onCancel={vi.fn()} onSaved={vi.fn()} />);

    expect(screen.queryByLabelText("接入方式")).not.toBeInTheDocument();
    expect(
      screen.queryByRole("button", { name: "借壳 Claude Code" }),
    ).not.toBeInTheDocument();
  });

  it("新建态渲染三张引擎卡，点击 myagent 后 provider chip 收窄到 myagent 组", () => {
    render(<AgentForm agent={null} onCancel={vi.fn()} onSaved={vi.fn()} />);

    const engines = engineRegion();

    expect(
      within(engines).getByRole("button", { name: "Claude Code CLI" }),
    ).toBeInTheDocument();
    expect(
      within(engines).getByRole("button", { name: "Codex CLI" }),
    ).toBeInTheDocument();
    expect(
      within(engines).getByRole("button", { name: "myagent" }),
    ).toBeInTheDocument();

    clickEngine("myagent");

    const providers = providerRegion();
    expect(
      within(providers).getByRole("button", { name: "DeepSeek" }),
    ).toHaveAttribute("aria-pressed", "true");
    expect(
      within(providers).getByRole("button", { name: "GLM · 智谱" }),
    ).toBeInTheDocument();
    expect(
      within(providers).getByRole("button", { name: "Kimi" }),
    ).toBeInTheDocument();
    expect(
      within(providers).queryByRole("button", { name: /Claude CLI/ }),
    ).toBeNull();
    expect(
      within(providers).queryByRole("button", { name: /Codex CLI/ }),
    ).toBeNull();
  });

  it("Codex 引擎 api_key 组显示灰占位 chip，点击不改变 preset", () => {
    render(<AgentForm agent={null} onCancel={vi.fn()} onSaved={vi.fn()} />);

    clickEngine("Codex CLI");

    expect(providerChip("OpenAI 账号")).toHaveAttribute("aria-pressed", "true");
    const placeholder = within(providerRegion()).getByText("借壳 · 后续版本");
    expect(placeholder).toHaveAttribute("aria-disabled", "true");

    fireEvent.click(placeholder);

    expect(providerChip("OpenAI 账号")).toHaveAttribute("aria-pressed", "true");
  });

  it("账号组 chip 使用账号归属文案，不使用 CLI preset label", () => {
    render(<AgentForm agent={null} onCancel={vi.fn()} onSaved={vi.fn()} />);

    expect(providerChip("Anthropic 账号")).toBeInTheDocument();

    clickEngine("Codex CLI");

    expect(providerChip("OpenAI 账号")).toBeInTheDocument();
  });

  it("智谱合并为单项·选 Kimi 显接入点 segment·DeepSeek 不显", () => {
    render(<AgentForm agent={null} onCancel={vi.fn()} onSaved={vi.fn()} />);

    expect(
      screen.getByRole("button", { name: "智谱 GLM" }),
    ).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Z.AI(GLM)" })).toBeNull();

    clickBorrowPreset("Kimi");

    expect(screen.getByText(/中国 · api\.moonshot\.cn/)).toBeInTheDocument();
    expect(screen.getByText(/国际 · api\.moonshot\.ai/)).toBeInTheDocument();

    clickBorrowPreset("DeepSeek");

    expect(screen.queryByLabelText("接入点")).toBeNull();
  });

  it("reasoning 默认档按 provider 能力渲染，不显示 auto 作为真实档位", () => {
    render(<AgentForm agent={null} onCancel={vi.fn()} onSaved={vi.fn()} />);

    clickBorrowPreset("DeepSeek");
    openMoreOptions();
    const reasoning = screen.getByLabelText("reasoning 默认档");

    expect(
      within(reasoning).queryByRole("button", { name: "auto" }),
    ).toBeNull();
    expect(
      within(reasoning).getByRole("button", { name: "medium" }),
    ).toHaveAttribute("aria-pressed", "true");
    expect(within(reasoning).queryByRole("button", { name: "max" })).toBeNull();
  });

  it("切接入点覆盖 endpoint/主模型/映射/timeout + resetTest + 退custom + 保key·不动provider-level", () => {
    render(<AgentForm agent={null} onCancel={vi.fn()} onSaved={vi.fn()} />);

    fireEvent.click(screen.getByRole("button", { name: "智谱 GLM" }));
    fireEvent.change(screen.getByLabelText("API Key"), {
      target: { value: "sk-keep" },
    });
    fireEvent.click(screen.getByText(/国际 · z\.ai/));
    openMoreOptions();

    expect(screen.getByLabelText("Endpoint")).toHaveValue(
      "https://api.z.ai/api/anthropic",
    );
    expect(screen.getByLabelText("api timeout")).toHaveValue("3000000");
    expect(screen.getByLabelText("opus")).toHaveValue("glm-4.7");
    expect(screen.getByLabelText("haiku")).toHaveValue("glm-4.5-air");
    expect(screen.getByLabelText("API Key")).toHaveValue("sk-keep");
    expect(
      screen.getByRole("button", { name: /glm-4\.7/ }),
    ).toBeInTheDocument();
    expect(screen.queryByLabelText("主模型")).toBeNull();
    expect(screen.getByRole("button", { name: /Bearer/ })).toHaveAttribute(
      "aria-pressed",
      "true",
    );
  });

  it("harness 切接入点联动 endpoint/name，测试连接按当前 endpoint 拉 models", async () => {
    const calls: any[] = [];
    invokeMock.mockImplementation((cmd: string, args?: any) => {
      calls.push([cmd, args]);
      if (cmd === "detect_runtime") return Promise.resolve(detectReady());
      if (cmd === "fetch_agent_models") return Promise.resolve(["glm-x"]);
      return Promise.resolve();
    });

    render(<AgentForm agent={null} onCancel={vi.fn()} onSaved={vi.fn()} />);

    clickHarnessPreset("GLM · 智谱");
    expect(screen.getByLabelText("名称")).toHaveValue("GLM · 智谱（myagent）");
    fireEvent.click(screen.getByText(/国际 · z\.ai/));
    openMoreOptions();

    expect(screen.getByLabelText("名称")).toHaveValue("GLM · 智谱（myagent）");
    expect(screen.getByLabelText("Endpoint")).toHaveValue(
      "https://api.z.ai/api/paas/v4",
    );

    fireEvent.change(screen.getByLabelText("API Key"), {
      target: { value: "sk-glm" },
    });
    await passConnectionTest();

    expect(calls.some((call) => call[0] === "test_agent_connection")).toBe(
      false,
    );
    expect(calls.find((call) => call[0] === "fetch_agent_models")?.[1]).toEqual(
      expect.objectContaining({
        modelsEndpoint: "https://api.z.ai/api/paas/v4/models",
        authMode: null,
        apiKey: "sk-glm",
      }),
    );
  });

  it("harness-glm 选 intl-coding 接入点联动 endpoint/name（Coding 套餐走专用路由）", async () => {
    const calls: any[] = [];
    invokeMock.mockImplementation((cmd: string, args?: any) => {
      calls.push([cmd, args]);
      if (cmd === "detect_runtime") return Promise.resolve(detectReady());
      if (cmd === "fetch_agent_models") return Promise.resolve(["glm-x"]);
      return Promise.resolve();
    });

    render(<AgentForm agent={null} onCancel={vi.fn()} onSaved={vi.fn()} />);

    clickHarnessPreset("GLM · 智谱");
    fireEvent.click(screen.getByText(/国际 · Coding 套餐 · z\.ai/));
    openMoreOptions();

    expect(screen.getByLabelText("名称")).toHaveValue("GLM · 智谱（myagent）");
    expect(screen.getByLabelText("Endpoint")).toHaveValue(
      "https://api.z.ai/api/coding/paas/v4",
    );

    fireEvent.change(screen.getByLabelText("API Key"), {
      target: { value: "sk-glm" },
    });
    await passConnectionTest();

    expect(calls.find((call) => call[0] === "fetch_agent_models")?.[1]).toEqual(
      expect.objectContaining({
        modelsEndpoint: "https://api.z.ai/api/coding/paas/v4/models",
        authMode: null,
        apiKey: "sk-glm",
      }),
    );
  });

  it("切接入点 resetTest 清掉上一区测试成功态", async () => {
    invokeMock.mockImplementation((cmd: string) => {
      if (cmd === "test_agent_connection") return Promise.resolve({ ok: true });
      if (cmd === "fetch_agent_models") return Promise.resolve(["kimi-k2.5"]);
      return Promise.resolve();
    });
    render(<AgentForm agent={null} onCancel={vi.fn()} onSaved={vi.fn()} />);

    clickBorrowPreset("Kimi");
    fireEvent.change(screen.getByLabelText("API Key"), {
      target: { value: "sk-x" },
    });
    fireEvent.click(screen.getByTestId("test-conn-btn"));

    await waitFor(() =>
      expect(screen.getByText(/连接成功/)).toBeInTheDocument(),
    );

    fireEvent.click(screen.getByText(/国际 · api\.moonshot\.ai/));

    expect(screen.queryByText(/连接成功/)).toBeNull();
  });

  it("编辑 .cn endpoint 的 Kimi agent → 回填 Kimi + 中国（初始化·本 task）", () => {
    render(
      <AgentForm
        agent={agent({
          provider: "kimi",
          endpoint: "https://api.moonshot.cn/anthropic",
          primary_model: "kimi-k2.5",
        })}
        onCancel={vi.fn()}
        onSaved={vi.fn()}
      />,
    );

    expect(borrowPreset("Kimi")).toHaveAttribute("aria-pressed", "true");
    expect(
      screen.getByText(/中国 · api\.moonshot\.cn/).closest("button"),
    ).toHaveAttribute("aria-pressed", "true");
  });

  it("多接入点 + 已有 key（编辑存量 agent 一进表单即显·不依赖切动作）", () => {
    render(
      <AgentForm
        agent={agent({
          provider: "kimi",
          endpoint: "https://api.moonshot.ai/anthropic",
          has_key: true,
        })}
        onCancel={vi.fn()}
        onSaved={vi.fn()}
      />,
    );

    expect(screen.getByText(/key 不通用/)).toBeInTheDocument();
  });

  it("单接入点(DeepSeek) 不显 key 跨区提示", () => {
    render(<AgentForm agent={null} onCancel={vi.fn()} onSaved={vi.fn()} />);

    clickBorrowPreset("DeepSeek");
    fireEvent.change(screen.getByLabelText("API Key"), {
      target: { value: "sk-x" },
    });

    expect(screen.queryByText(/key 不通用/)).toBeNull();
  });

  it("编辑坏 endpoint agent 不崩溃 → 落 custom（初始化·本 task）", () => {
    render(
      <AgentForm
        agent={agent({ provider: "kimi", endpoint: "garbage" })}
        onCancel={vi.fn()}
        onSaved={vi.fn()}
      />,
    );

    expect(providerChip("自定义")).toHaveAttribute("aria-pressed", "true");
  });

  it("form_preset_deepseek_autofills", () => {
    render(<AgentForm onCancel={vi.fn()} onSaved={vi.fn()} />);

    clickBorrowPreset("DeepSeek");
    openMoreOptions();

    expect(screen.getByLabelText("Endpoint")).toHaveValue(
      "https://api.deepseek.com/anthropic",
    );
    expect(screen.getByRole("button", { name: /Bearer/ })).toHaveAttribute(
      "aria-pressed",
      "true",
    );
    expect(screen.getByLabelText("compat proxy")).toHaveValue(
      "thinking_passback",
    );
  });

  it("切预设和接入点会自动填名称，用户手改后不再覆盖", () => {
    render(<AgentForm onCancel={vi.fn()} onSaved={vi.fn()} />);

    clickBorrowPreset("Kimi");

    expect(screen.getByLabelText("名称")).toHaveValue(
      autoAgentName("kimi", "cn"),
    );

    fireEvent.click(screen.getByText(/国际 · api\.moonshot\.ai/));

    expect(screen.getByLabelText("名称")).toHaveValue(
      autoAgentName("kimi", "intl"),
    );

    fireEvent.change(screen.getByLabelText("名称"), {
      target: { value: "My Kimi" },
    });
    fireEvent.click(screen.getByText(/中国 · api\.moonshot\.cn/));

    expect(screen.getByLabelText("名称")).toHaveValue("My Kimi");
  });

  it("advanced_collapsed_default", () => {
    render(<AgentForm onCancel={vi.fn()} onSaved={vi.fn()} />);

    expect(screen.getByRole("button", { name: /更多选项/ })).toHaveAttribute(
      "aria-expanded",
      "false",
    );
    expect(screen.queryByLabelText("Endpoint")).not.toBeInTheDocument();
  });

  it("更多选项展开后是单层结构，不再显示高级用户选项标题", () => {
    render(<AgentForm onCancel={vi.fn()} onSaved={vi.fn()} />);

    clickBorrowPreset("DeepSeek");
    openMoreOptions();

    expect(screen.queryByText(/高级用户选项/)).toBeNull();
    expect(screen.getByLabelText("compat proxy")).toBeInTheDocument();
  });

  it("borrow 的模型和 reasoning 默认档默认收起，展开更多选项后可见", () => {
    render(<AgentForm onCancel={vi.fn()} onSaved={vi.fn()} />);

    clickBorrowPreset("DeepSeek");

    expect(screen.queryByText("主模型")).toBeNull();
    expect(screen.queryByLabelText("reasoning 默认档")).not.toBeInTheDocument();

    openMoreOptions();

    expect(screen.getByText("主模型")).toBeInTheDocument();
    expect(screen.getByLabelText("reasoning 默认档")).toBeInTheDocument();
  });

  it("harness 更多选项显示模型 endpoint 超时，不显示鉴权方式/模型映射/兼容性", () => {
    render(<AgentForm onCancel={vi.fn()} onSaved={vi.fn()} />);

    clickHarnessPreset("GLM · 智谱");
    openMoreOptions();

    expect(screen.getByLabelText("模型")).toBeInTheDocument();
    expect(screen.getByLabelText("reasoning 默认档")).toBeInTheDocument();
    expect(screen.getByLabelText("Endpoint")).toBeInTheDocument();
    expect(screen.getByLabelText("api timeout")).toBeInTheDocument();
    expect(screen.queryByLabelText("compat proxy")).not.toBeInTheDocument();
    expect(screen.queryByLabelText("关 thinking")).not.toBeInTheDocument();
    expect(screen.queryByLabelText("关 betas")).not.toBeInTheDocument();
    expect(screen.queryByLabelText("关非必要流量")).not.toBeInTheDocument();
    expect(screen.queryByLabelText("鉴权方式")).not.toBeInTheDocument();
    expect(screen.queryByLabelText("模型映射")).not.toBeInTheDocument();
    expect(screen.queryByLabelText("opus")).not.toBeInTheDocument();
    expect(
      screen.queryByLabelText("max output tokens"),
    ).not.toBeInTheDocument();
  });

  it("borrow 更多选项显示借壳兼容性细调开关", () => {
    render(<AgentForm onCancel={vi.fn()} onSaved={vi.fn()} />);

    clickBorrowPreset("DeepSeek");
    openMoreOptions();

    expect(
      screen.getByText("兼容性开关（借壳细调 · 一般不动）"),
    ).toBeInTheDocument();
    expect(screen.getByLabelText("compat proxy")).toBeInTheDocument();
  });

  it("编辑 harness agent 手改 endpoint 后仍归属 myagent 且保留原 endpoint", () => {
    const customEndpoint = "https://custom-harness-proxy.example/v1";

    render(
      <AgentForm
        agent={agent({
          access: "harness",
          provider: "glm",
          endpoint: customEndpoint,
          primary_model: null,
          auth_mode: null,
          compat_proxy: null,
          compat_disable_nonessential: false,
        })}
        onCancel={vi.fn()}
        onSaved={vi.fn()}
      />,
    );

    expect(
      within(engineRegion()).getByRole("button", { name: "myagent" }),
    ).toHaveAttribute("aria-pressed", "true");
    openMoreOptions();
    expect(screen.getByLabelText("Endpoint")).toHaveValue(customEndpoint);
  });

  it("编辑态锁 access 家族：编辑 harness 只显 harness 组、编辑 borrow 不显 harness 组", () => {
    // codex 审出的 Medium：跨族点击会存出「access 与 preset 脱钩」的坏配置
    //（如 harness agent 配上借壳 /anthropic 端点）——编辑态直接不显示跨族预设组。
    const { unmount } = render(
      <AgentForm
        agent={agent({
          access: "harness",
          endpoint: "https://api.deepseek.com/v1",
          primary_model: null,
        })}
        onCancel={vi.fn()}
        onSaved={vi.fn()}
      />,
    );
    expect(
      within(engineRegion()).getByRole("button", { name: "myagent" }),
    ).toBeInTheDocument();
    expect(
      within(engineRegion()).queryByRole("button", {
        name: "Claude Code CLI",
      }),
    ).toBeNull();
    expect(
      within(engineRegion()).queryByRole("button", { name: "Codex CLI" }),
    ).toBeNull();
    expect(providerChip("DeepSeek")).toHaveAttribute("aria-pressed", "true");
    unmount();

    render(
      <AgentForm agent={agent({})} onCancel={vi.fn()} onSaved={vi.fn()} />,
    );
    expect(
      within(engineRegion()).getByRole("button", { name: "Claude Code CLI" }),
    ).toBeInTheDocument();
    expect(
      within(engineRegion()).queryByRole("button", { name: "myagent" }),
    ).toBeNull();
    expect(providerChip("DeepSeek")).toBeInTheDocument();
    expect(
      within(providerRegion()).queryByRole("button", { name: /GLM · 智谱/ }),
    ).toBeNull();
  });
});

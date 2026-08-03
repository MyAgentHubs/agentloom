import {
  act,
  fireEvent,
  render,
  screen,
  waitFor,
  within,
} from "@testing-library/react";
import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { invoke } from "@tauri-apps/api/core";
import { openUrl } from "@tauri-apps/plugin-opener";
import { I18nProvider } from "../../i18n";
import type { AgentProfile, ConnectionTestResult } from "../../types/agent";
import { AgentForm } from "./AgentForm";
import {
  autoAgentName,
  deriveModelMapping,
  writeModelCache,
} from "./agentFormHelpers";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/plugin-opener", () => ({ openUrl: vi.fn() }));

function agent(overrides: Partial<AgentProfile>): AgentProfile {
  return {
    id: "deepseek",
    name: "DeepSeek",
    access: "borrow",
    provider: "deepseek",
    primary_model: "deepseek-v4-pro",
    endpoint: "https://api.deepseek.com/anthropic",
    auth_mode: "bearer",
    model_opus: null,
    model_sonnet: null,
    model_haiku: null,
    model_subagent: null,
    reasoning_default: "auto",
    max_output_tokens: null,
    api_timeout_ms: null,
    compat_disable_betas: false,
    compat_disable_nonessential: true,
    compat_disable_thinking: false,
    compat_proxy: "thinking_passback",
    custom_headers: null,
    extra_body: null,
    cap_reasoning: null,
    cap_computer_use: null,
    cap_lead: null,
    has_key: false,
    is_builtin: false,
    enabled: true,
    sort_order: 0,
    created_at: 0,
    updated_at: 0,
    ...overrides,
  };
}

function deferred<T>() {
  let resolve!: (value: T | PromiseLike<T>) => void;
  let reject!: (reason?: unknown) => void;
  const promise = new Promise<T>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

function detectReady() {
  return {
    claude: { available: true, version: null, path: null, creds_hint: true },
    codex: { available: true, version: null, path: null, creds_hint: true },
  };
}

function invokeWithConnectionOk(cmd: string) {
  if (cmd === "detect_runtime") return Promise.resolve(detectReady());
  if (cmd === "test_agent_connection") return Promise.resolve({ ok: true });
  if (cmd === "fetch_agent_models") return Promise.resolve([]);
  return Promise.resolve();
}

function escapeRegExp(value: string) {
  return value.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

function engineRegion() {
  return screen.getByLabelText("引擎");
}

function clickEngine(name: string) {
  fireEvent.click(within(engineRegion()).getByRole("button", { name }));
}

function providerRegion() {
  return screen.getByLabelText("LLM Provider");
}

function providerChip(name: string) {
  return within(providerRegion()).getByRole("button", {
    name: new RegExp(`^${escapeRegExp(name)}`),
  });
}

function borrowPreset(name: string) {
  return providerChip(name);
}

function clickBorrowPreset(name: string) {
  fireEvent.click(borrowPreset(name));
}

function harnessPreset(name: string) {
  clickEngine("myagent");
  return providerChip(name);
}

function clickHarnessPreset(name: string) {
  fireEvent.click(harnessPreset(name));
}

async function passConnectionTest() {
  fireEvent.click(screen.getByTestId("test-conn-btn"));
  await screen.findByText(/连接成功/);
}

function openMoreOptions() {
  fireEvent.click(screen.getByRole("button", { name: /更多选项/ }));
}

const HARNESS_DEEPSEEK_ENDPOINT = "https://api.deepseek.com/v1";

function writeHarnessDeepSeekModelCache(models = ["model-a", "model-b"]) {
  writeModelCache("harness-deepseek", HARNESS_DEEPSEEK_ENDPOINT, models);
}

function expectAutoMark(label: string) {
  const field = screen.getByLabelText(label).closest("div");
  expect(field).not.toBeNull();
  expect(within(field!).getByText(/自动/)).toBeInTheDocument();
}

function expectNoAutoMark(label: string) {
  const field = screen.getByLabelText(label).closest("div");
  expect(field).not.toBeNull();
  expect(within(field!).queryByText(/自动/)).toBeNull();
}

describe("AgentForm", () => {
  const invokeMock = vi.mocked(invoke);

  beforeEach(() => {
    invokeMock.mockReset();
    invokeMock.mockResolvedValue(undefined);
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

  it("主模型为下拉·选预设填默认·可选已知模型", async () => {
    invokeMock.mockImplementation(invokeWithConnectionOk);
    const onSaved = vi.fn();

    render(<AgentForm agent={null} onCancel={vi.fn()} onSaved={onSaved} />);

    clickBorrowPreset("Kimi");
    openMoreOptions();
    fireEvent.click(screen.getByRole("button", { name: /kimi-k2\.5/ }));
    fireEvent.click(
      screen.getByRole("menuitemradio", { name: /^kimi-k2\.6$/ }),
    );

    expect(
      screen.getByRole("button", { name: /kimi-k2\.6/ }),
    ).toBeInTheDocument();

    fireEvent.change(screen.getByLabelText("API Key"), {
      target: { value: "sk-kimi" },
    });
    await passConnectionTest();
    fireEvent.click(screen.getByRole("button", { name: "添加" }));

    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("upsert_agent", {
        profile: expect.objectContaining({
          primary_model: "kimi-k2.6",
        }),
      }),
    );
    expect(onSaved).toHaveBeenCalledTimes(1);
  });

  it("编辑历史自定义主模型退化为 input", () => {
    render(
      <AgentForm
        agent={agent({
          id: "x",
          access: "borrow",
          provider: "deepseek",
          primary_model: "my-custom-old-model",
        })}
        onCancel={vi.fn()}
        onSaved={vi.fn()}
      />,
    );

    openMoreOptions();
    const input = screen.getByLabelText("主模型");
    expect(input).toBeInstanceOf(HTMLInputElement);
    expect(input).toHaveValue("my-custom-old-model");
  });

  it("borrow 编辑态：自定义主模型即使命中缓存也维持 input（缓存只影响 harness·回归）", () => {
    writeModelCache("deepseek", "https://api.deepseek.com/anthropic", [
      "my-custom-old-model",
    ]);
    render(
      <AgentForm
        agent={agent({
          id: "x",
          access: "borrow",
          provider: "deepseek",
          primary_model: "my-custom-old-model",
        })}
        onCancel={vi.fn()}
        onSaved={vi.fn()}
      />,
    );

    openMoreOptions();
    const input = screen.getByLabelText("主模型");
    expect(input).toBeInstanceOf(HTMLInputElement);
    expect(input).toHaveValue("my-custom-old-model");
  });

  it("选「自定义…」退化为可手敲 input（保留 aria-label 主模型）", () => {
    render(<AgentForm agent={null} onCancel={vi.fn()} onSaved={vi.fn()} />);

    clickBorrowPreset("DeepSeek");
    openMoreOptions();
    fireEvent.click(screen.getByRole("button", { name: /deepseek-v4-pro/ }));
    fireEvent.click(screen.getByRole("menuitemradio", { name: /自定义/ }));

    const input = screen.getByLabelText("主模型");
    expect(input).toBeInstanceOf(HTMLInputElement);
  });

  it("选自定义后可经「从列表选择」入口切回下拉", () => {
    render(<AgentForm agent={null} onCancel={vi.fn()} onSaved={vi.fn()} />);

    clickBorrowPreset("DeepSeek");
    openMoreOptions();
    fireEvent.click(screen.getByRole("button", { name: /deepseek-v4-pro/ }));
    fireEvent.click(screen.getByRole("menuitemradio", { name: /自定义/ }));

    expect(screen.getByLabelText("主模型")).toBeInstanceOf(HTMLInputElement);

    fireEvent.click(screen.getByRole("button", { name: "↩ 从列表选择" }));

    expect(
      screen.getByRole("button", { name: /deepseek-v4-pro/ }),
    ).toBeInTheDocument();
    expect(screen.queryByLabelText("主模型")).not.toBeInTheDocument();
  });

  function setupTestConnectionForm() {
    render(<AgentForm agent={null} onCancel={vi.fn()} onSaved={vi.fn()} />);
    clickBorrowPreset("DeepSeek");
    fireEvent.change(screen.getByLabelText("API Key"), {
      target: { value: "sk-x" },
    });
  }

  it("测试连接成功 → 显「连接成功」+ 链式拉模型灌下拉(带实时 badge)", async () => {
    invokeMock.mockImplementation((cmd: string) => {
      if (cmd === "test_agent_connection") {
        return Promise.resolve({ ok: true, category: null, raw_error: null });
      }
      if (cmd === "fetch_agent_models") {
        return Promise.resolve(["deepseek-live-x"]);
      }
      return Promise.resolve();
    });

    setupTestConnectionForm();
    fireEvent.click(screen.getByTestId("test-conn-btn"));

    expect(await screen.findByText(/连接成功/)).toBeInTheDocument();
    openMoreOptions();
    fireEvent.click(screen.getByRole("button", { name: /deepseek-v4-pro/ }));
    const item = await screen.findByRole("menuitemradio", {
      name: /deepseek-live-x/,
    });
    expect(within(item).getByText("实时")).toBeInTheDocument();
  });

  it("测试连接成功→用当前接入点 modelsEndpoint 拉模型", async () => {
    const calls: any[] = [];
    invokeMock.mockImplementation((cmd: string, args?: any) => {
      calls.push([cmd, args]);
      if (cmd === "test_agent_connection") return Promise.resolve({ ok: true });
      if (cmd === "fetch_agent_models")
        return Promise.resolve(["kimi-k2.5", "x-live"]);
      return Promise.resolve();
    });
    render(<AgentForm agent={null} onCancel={vi.fn()} onSaved={vi.fn()} />);
    clickBorrowPreset("Kimi");
    fireEvent.change(screen.getByLabelText("API Key"), {
      target: { value: "sk-x" },
    });
    fireEvent.click(screen.getByTestId("test-conn-btn"));
    await waitFor(() =>
      expect(
        calls.find((c) => c[0] === "fetch_agent_models")?.[1].modelsEndpoint,
      ).toBe("https://api.moonshot.cn/v1/models"),
    );
  });

  it("智谱借壳测试成功后使用 OpenAI 兼容 modelsEndpoint 拉模型并自动推导映射", async () => {
    const models = ["glm-4.7", "glm-5", "glm-5-air", "glm-4.5-air"];
    const expected = deriveModelMapping(models);
    const calls: any[] = [];
    expect(expected).not.toBeNull();
    invokeMock.mockImplementation((cmd: string, args?: any) => {
      calls.push([cmd, args]);
      if (cmd === "test_agent_connection") return Promise.resolve({ ok: true });
      if (cmd === "fetch_agent_models") return Promise.resolve(models);
      return Promise.resolve();
    });

    render(<AgentForm agent={null} onCancel={vi.fn()} onSaved={vi.fn()} />);

    clickBorrowPreset("智谱 GLM");
    fireEvent.change(screen.getByLabelText("API Key"), {
      target: { value: "sk-glm" },
    });
    await passConnectionTest();

    await waitFor(() =>
      expect(
        calls.find((call) => call[0] === "fetch_agent_models")?.[1]
          .modelsEndpoint,
      ).toBe("https://open.bigmodel.cn/api/paas/v4/models"),
    );
    openMoreOptions();
    await waitFor(() =>
      expect(screen.getByLabelText("opus")).toHaveValue(expected!.opus),
    );
    expect(screen.getByLabelText("sonnet")).toHaveValue(expected!.sonnet);
    expect(screen.getByLabelText("haiku")).toHaveValue(expected!.haiku);
    expect(screen.getByLabelText("subagent")).toHaveValue(expected!.subagent);
    expect(
      screen.getByRole("button", { name: new RegExp(expected!.primary!) }),
    ).toBeInTheDocument();
  });

  it("手改 endpoint ≠ 选中接入点 → 不链式拉、cache guard 不回灌 drift cached", async () => {
    const calls: string[] = [];
    invokeMock.mockImplementation((cmd: string) => {
      calls.push(cmd);
      if (cmd === "test_agent_connection") return Promise.resolve({ ok: true });
      return Promise.resolve();
    });
    writeModelCache("kimi", "https://other.example/anthropic", [
      "drift-cached",
    ]);
    render(<AgentForm agent={null} onCancel={vi.fn()} onSaved={vi.fn()} />);
    clickBorrowPreset("Kimi");
    fireEvent.change(screen.getByLabelText("API Key"), {
      target: { value: "sk-x" },
    });
    openMoreOptions();
    fireEvent.change(screen.getByLabelText("Endpoint"), {
      target: { value: "https://other.example/anthropic" },
    });
    fireEvent.click(screen.getByTestId("test-conn-btn"));
    await waitFor(() =>
      expect(calls.filter((c) => c === "test_agent_connection").length).toBe(1),
    );
    expect(calls).not.toContain("fetch_agent_models");
    fireEvent.click(
      screen.getByRole("button", { name: /kimi-k2\.5|选择模型/ }),
    );
    expect(screen.queryByText("drift-cached")).toBeNull();
  });

  it("尾斜杠/大小写归一后命中同一 cache（endpoint==选中接入点）", () => {
    writeModelCache("kimi", "https://API.moonshot.CN/anthropic/", [
      "kimi-k2.5",
      "cached-x",
    ]);
    render(<AgentForm agent={null} onCancel={vi.fn()} onSaved={vi.fn()} />);
    clickBorrowPreset("Kimi");
    openMoreOptions();
    fireEvent.click(
      screen.getByRole("button", { name: /kimi-k2\.5|选择模型/ }),
    );
    expect(screen.getByText("cached-x")).toBeInTheDocument();
  });

  it("测试连接失败 → 友好分类 + 可展开/收起原始错误", async () => {
    invokeMock.mockImplementation((cmd: string) => {
      if (cmd === "test_agent_connection") {
        return Promise.resolve({
          ok: false,
          category: "auth",
          raw_error: "HTTP 401: bad key",
        });
      }
      return Promise.resolve();
    });

    setupTestConnectionForm();
    fireEvent.click(screen.getByTestId("test-conn-btn"));

    expect(await screen.findByText(/key 无效或无权限/)).toBeInTheDocument();
    const rawToggle = screen.getByRole("button", { name: /展开原始错误/ });
    expect(rawToggle).toHaveAttribute("aria-expanded", "false");

    fireEvent.click(rawToggle);
    expect(rawToggle).toHaveAttribute("aria-expanded", "true");
    expect(screen.getByText(/HTTP 401: bad key/)).toBeInTheDocument();

    fireEvent.click(rawToggle);
    expect(rawToggle).toHaveAttribute("aria-expanded", "false");
    expect(screen.queryByText(/HTTP 401: bad key/)).not.toBeInTheDocument();
  });

  it("改 key 后测试状态 reset 回 idle 且清掉实时模型", async () => {
    invokeMock.mockImplementation((cmd: string) => {
      if (cmd === "test_agent_connection") {
        return Promise.resolve({ ok: true, category: null, raw_error: null });
      }
      if (cmd === "fetch_agent_models") {
        return Promise.resolve(["deepseek-live-x"]);
      }
      return Promise.resolve();
    });

    setupTestConnectionForm();
    fireEvent.click(screen.getByTestId("test-conn-btn"));
    expect(await screen.findByText(/连接成功/)).toBeInTheDocument();
    openMoreOptions();
    fireEvent.click(screen.getByRole("button", { name: /deepseek-v4-pro/ }));
    const liveItem = await screen.findByRole("menuitemradio", {
      name: /deepseek-live-x/,
    });
    expect(within(liveItem).getByText("实时")).toBeInTheDocument();

    fireEvent.change(screen.getByLabelText("API Key"), {
      target: { value: "sk-y" },
    });

    expect(screen.queryByText(/连接成功/)).not.toBeInTheDocument();
    expect(
      screen.queryByRole("menuitemradio", { name: /deepseek-live-x/ }),
    ).not.toBeInTheDocument();
  });

  it("切换鉴权方式后测试状态 reset 回 idle（连接成功消失）", async () => {
    invokeMock.mockImplementation((cmd: string) => {
      if (cmd === "test_agent_connection") {
        return Promise.resolve({ ok: true, category: null, raw_error: null });
      }
      if (cmd === "fetch_agent_models") {
        return Promise.resolve([]);
      }
      return Promise.resolve();
    });

    setupTestConnectionForm();
    fireEvent.click(screen.getByTestId("test-conn-btn"));
    expect(await screen.findByText(/连接成功/)).toBeInTheDocument();

    openMoreOptions();
    fireEvent.click(screen.getByRole("button", { name: /x-api-key/ }));

    expect(screen.queryByText(/连接成功/)).not.toBeInTheDocument();
  });

  it("链式拉模型失败仍保留测试连接成功状态", async () => {
    invokeMock.mockImplementation((cmd: string) => {
      if (cmd === "test_agent_connection") {
        return Promise.resolve({ ok: true, category: null, raw_error: null });
      }
      if (cmd === "fetch_agent_models") {
        return Promise.reject(new Error("models unavailable"));
      }
      return Promise.resolve();
    });

    setupTestConnectionForm();
    fireEvent.click(screen.getByTestId("test-conn-btn"));

    expect(await screen.findByText("连接成功")).toBeInTheDocument();
    expect(screen.queryByText(/已拉取/)).not.toBeInTheDocument();
  });

  it("测试连接中按钮禁用", async () => {
    const testDeferred = deferred<ConnectionTestResult>();
    invokeMock.mockImplementation((cmd: string) => {
      if (cmd === "test_agent_connection") {
        return testDeferred.promise;
      }
      if (cmd === "fetch_agent_models") {
        return Promise.resolve([]);
      }
      return Promise.resolve();
    });

    setupTestConnectionForm();
    fireEvent.click(screen.getByTestId("test-conn-btn"));

    expect(screen.getByTestId("test-conn-btn")).toBeDisabled();

    await act(async () => {
      testDeferred.resolve({ ok: true, category: null, raw_error: null });
      await testDeferred.promise;
    });
  });

  it("测试中字段变更后旧请求结果不会复活连接成功", async () => {
    const testDeferred = deferred<ConnectionTestResult>();
    invokeMock.mockImplementation((cmd: string) => {
      if (cmd === "test_agent_connection") {
        return testDeferred.promise;
      }
      if (cmd === "fetch_agent_models") {
        return Promise.resolve([]);
      }
      return Promise.resolve();
    });

    setupTestConnectionForm();
    fireEvent.click(screen.getByTestId("test-conn-btn"));
    expect(await screen.findByTestId("test-state")).toHaveTextContent(/测试中/);

    fireEvent.change(screen.getByLabelText("API Key"), {
      target: { value: "sk-y" },
    });
    expect(screen.queryByTestId("test-state")).not.toBeInTheDocument();

    await act(async () => {
      testDeferred.resolve({ ok: true, category: null, raw_error: null });
      await testDeferred.promise;
    });

    expect(screen.queryByText(/连接成功/)).not.toBeInTheDocument();
  });

  it("自定义主模型态切换预设后恢复下拉", () => {
    render(<AgentForm agent={null} onCancel={vi.fn()} onSaved={vi.fn()} />);

    clickBorrowPreset("DeepSeek");
    openMoreOptions();
    fireEvent.click(screen.getByRole("button", { name: /deepseek-v4-pro/ }));
    fireEvent.click(screen.getByRole("menuitemradio", { name: /自定义/ }));

    expect(screen.getByLabelText("主模型")).toBeInstanceOf(HTMLInputElement);

    clickBorrowPreset("Kimi");

    expect(
      screen.getByRole("button", { name: /kimi-k2\.5/ }),
    ).toBeInTheDocument();
    expect(screen.queryByLabelText("主模型")).not.toBeInTheDocument();
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

  it("测试连接成功后从实时模型自动推导 borrow 模型映射并标注自动", async () => {
    const models = ["glm-4.7", "glm-5", "glm-5-air", "glm-4.5-air"];
    const expected = deriveModelMapping(models);
    expect(expected).not.toBeNull();
    invokeMock.mockImplementation((cmd: string) => {
      if (cmd === "test_agent_connection") return Promise.resolve({ ok: true });
      if (cmd === "fetch_agent_models") return Promise.resolve(models);
      return Promise.resolve();
    });

    render(<AgentForm onCancel={vi.fn()} onSaved={vi.fn()} />);

    clickBorrowPreset("DeepSeek");
    fireEvent.change(screen.getByLabelText("API Key"), {
      target: { value: "sk-glm" },
    });
    await passConnectionTest();
    openMoreOptions();

    await waitFor(() =>
      expect(screen.getByLabelText("opus")).toHaveValue(expected!.opus),
    );
    expect(screen.getByLabelText("sonnet")).toHaveValue(expected!.sonnet);
    expect(screen.getByLabelText("haiku")).toHaveValue(expected!.haiku);
    expect(screen.getByLabelText("subagent")).toHaveValue(expected!.subagent);
    expect(
      screen.getByRole("button", { name: new RegExp(expected!.primary!) }),
    ).toBeInTheDocument();
    expectAutoMark("opus");
    expectAutoMark("sonnet");
    expectAutoMark("haiku");
    expectAutoMark("subagent");
    expect(screen.getAllByText("· 自动")).toHaveLength(4);
  });

  it("手改过的映射字段不再被自动推导覆盖，未手改字段继续跟随", async () => {
    const firstModels = ["glm-4.7", "glm-5", "glm-5-air", "glm-4.5-air"];
    const secondModels = [
      "glm-4.7",
      "glm-5",
      "glm-5-air",
      "glm-6",
      "glm-6-air",
    ];
    const firstExpected = deriveModelMapping(firstModels);
    const secondExpected = deriveModelMapping(secondModels);
    expect(firstExpected).not.toBeNull();
    expect(secondExpected).not.toBeNull();
    invokeMock.mockImplementation((cmd: string) => {
      if (cmd === "test_agent_connection") return Promise.resolve({ ok: true });
      if (cmd === "fetch_agent_models") {
        return Promise.resolve(
          invokeMock.mock.calls.filter(
            ([name]) => name === "fetch_agent_models",
          ).length === 1
            ? firstModels
            : secondModels,
        );
      }
      return Promise.resolve();
    });

    render(<AgentForm onCancel={vi.fn()} onSaved={vi.fn()} />);

    clickBorrowPreset("DeepSeek");
    fireEvent.change(screen.getByLabelText("API Key"), {
      target: { value: "sk-glm" },
    });
    await passConnectionTest();
    openMoreOptions();
    await waitFor(() =>
      expect(screen.getByLabelText("opus")).toHaveValue(firstExpected!.opus),
    );

    fireEvent.change(screen.getByLabelText("opus"), {
      target: { value: "user-opus" },
    });
    expect(screen.getByLabelText("opus")).toHaveValue("user-opus");
    expectNoAutoMark("opus");

    fireEvent.click(screen.getByTestId("test-conn-btn"));
    await screen.findByText(/已拉取 5 个模型/);

    expect(screen.getByLabelText("opus")).toHaveValue("user-opus");
    expectNoAutoMark("opus");
    expect(screen.getByLabelText("sonnet")).toHaveValue(secondExpected!.sonnet);
    expect(screen.getByLabelText("haiku")).toHaveValue(secondExpected!.haiku);
    expect(screen.getByLabelText("subagent")).toHaveValue(
      secondExpected!.subagent,
    );
    expectAutoMark("sonnet");
    expectAutoMark("haiku");
    expectAutoMark("subagent");
  });

  it("编辑已有 borrow agent 测试连接成功后不覆盖已存模型映射", async () => {
    const models = ["deepseek-v5", "deepseek-v5-flash"];
    expect(deriveModelMapping(models)).not.toBeNull();
    invokeMock.mockImplementation((cmd: string) => {
      if (cmd === "test_agent_connection") return Promise.resolve({ ok: true });
      if (cmd === "fetch_agent_models") return Promise.resolve(models);
      return Promise.resolve();
    });

    render(
      <AgentForm
        agent={agent({
          id: "x",
          provider: "deepseek",
          primary_model: "deepseek-v4-pro",
          model_opus: "custom-opus",
          model_sonnet: "custom-sonnet",
          model_haiku: "custom-haiku",
          model_subagent: "custom-subagent",
          has_key: true,
        })}
        onCancel={vi.fn()}
        onSaved={vi.fn()}
      />,
    );

    await passConnectionTest();
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith(
        "fetch_agent_models",
        expect.anything(),
      ),
    );
    openMoreOptions();

    expect(screen.getByLabelText("opus")).toHaveValue("custom-opus");
    expect(screen.getByLabelText("sonnet")).toHaveValue("custom-sonnet");
    expect(screen.getByLabelText("haiku")).toHaveValue("custom-haiku");
    expect(screen.getByLabelText("subagent")).toHaveValue("custom-subagent");
    expect(screen.queryAllByText("· 自动")).toHaveLength(0);
  });

  it("自动映射推导为空时保留预设静态映射且不显示自动标注", async () => {
    const models = ["deepseek-chat", "deepseek-reasoner"];
    expect(deriveModelMapping(models)).toBeNull();
    invokeMock.mockImplementation((cmd: string) => {
      if (cmd === "test_agent_connection") return Promise.resolve({ ok: true });
      if (cmd === "fetch_agent_models") return Promise.resolve(models);
      return Promise.resolve();
    });

    render(<AgentForm onCancel={vi.fn()} onSaved={vi.fn()} />);

    clickBorrowPreset("DeepSeek");
    fireEvent.change(screen.getByLabelText("API Key"), {
      target: { value: "sk-deepseek" },
    });
    await passConnectionTest();
    openMoreOptions();

    expect(screen.getByLabelText("opus")).toHaveValue("deepseek-v4-pro");
    expect(screen.getByLabelText("sonnet")).toHaveValue("deepseek-v4-pro");
    expect(screen.getByLabelText("haiku")).toHaveValue("deepseek-v4-flash");
    expect(screen.getByLabelText("subagent")).toHaveValue("deepseek-v4-flash");
    expectNoAutoMark("opus");
    expectNoAutoMark("sonnet");
    expectNoAutoMark("haiku");
    expectNoAutoMark("subagent");
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

  it("harness 有缓存模型列表时模型字段渲染下拉并包含默认项", async () => {
    writeHarnessDeepSeekModelCache();

    render(<AgentForm onCancel={vi.fn()} onSaved={vi.fn()} />);

    clickHarnessPreset("DeepSeek");
    openMoreOptions();

    const trigger = await screen.findByRole("button", {
      name: /myagent 默认/,
    });
    expect(screen.queryByLabelText("模型")).not.toBeInTheDocument();
    fireEvent.click(trigger);

    const menu = screen.getByRole("menu");
    expect(
      within(menu).getByRole("menuitemradio", {
        name: /myagent 默认/,
      }),
    ).toBeInTheDocument();
    expect(
      within(menu).getByRole("menuitemradio", { name: /model-a/ }),
    ).toBeInTheDocument();
    expect(
      within(menu).getByRole("menuitemradio", { name: /model-b/ }),
    ).toBeInTheDocument();
  });

  it("harness 缓存模型可从下拉选择具体模型", async () => {
    writeHarnessDeepSeekModelCache();

    render(<AgentForm onCancel={vi.fn()} onSaved={vi.fn()} />);

    clickHarnessPreset("DeepSeek");
    openMoreOptions();
    fireEvent.click(
      await screen.findByRole("button", { name: /myagent 默认/ }),
    );
    fireEvent.click(screen.getByRole("menuitemradio", { name: /model-a/ }));

    expect(
      screen.getByRole("button", { name: /^model-a/ }),
    ).toBeInTheDocument();
  });

  it("harness 缓存模型选择 myagent 默认项后保存 primary_model 为空", async () => {
    vi.spyOn(Date, "now").mockReturnValue(1_700_000_000_000);
    writeHarnessDeepSeekModelCache();
    invokeMock.mockImplementation((cmd: string) => {
      if (cmd === "detect_runtime") return Promise.resolve(detectReady());
      if (cmd === "fetch_agent_models")
        return Promise.resolve(["model-a", "model-b"]);
      return Promise.resolve();
    });
    const onSaved = vi.fn();

    render(
      <AgentForm onCancel={vi.fn()} onSaved={onSaved} nextSortOrder={4} />,
    );

    clickHarnessPreset("DeepSeek");
    openMoreOptions();
    fireEvent.click(
      await screen.findByRole("button", { name: /myagent 默认/ }),
    );
    fireEvent.click(screen.getByRole("menuitemradio", { name: /model-a/ }));
    expect(
      screen.getByRole("button", { name: /^model-a/ }),
    ).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: /^model-a/ }));
    fireEvent.click(
      screen.getByRole("menuitemradio", { name: /myagent 默认/ }),
    );
    expect(
      screen.getByRole("button", { name: /myagent 默认/ }),
    ).toBeInTheDocument();

    fireEvent.change(screen.getByLabelText("API Key"), {
      target: { value: "sk-harness" },
    });
    await passConnectionTest();
    fireEvent.click(screen.getByRole("button", { name: "添加" }));

    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("upsert_agent", {
        profile: expect.objectContaining({
          id: "deepseek-myagent",
          access: "harness",
          primary_model: null,
        }),
      }),
    );
    expect(onSaved).toHaveBeenCalledTimes(1);
  });

  it("harness 无模型列表时仍是纯文本输入（现状回归）", () => {
    render(<AgentForm onCancel={vi.fn()} onSaved={vi.fn()} />);

    clickHarnessPreset("DeepSeek");
    openMoreOptions();

    const modelInput = screen.getByLabelText("模型");
    expect(modelInput).toBeInstanceOf(HTMLInputElement);
    expect(modelInput).toHaveAttribute("placeholder", "留空 = myagent 默认");
    expect(screen.queryByRole("button", { name: "↩ 从列表选择" })).toBeNull();
  });

  it("harness 测试成功且模型为空时自动选中列表末位模型（防默认模型陷阱）", async () => {
    invokeMock.mockImplementation((cmd: string) => {
      if (cmd === "detect_runtime") return Promise.resolve(detectReady());
      if (cmd === "fetch_agent_models")
        return Promise.resolve(["glm-4.5", "glm-4.6", "glm-4.7"]);
      return Promise.resolve();
    });
    const onSaved = vi.fn();

    render(<AgentForm onCancel={vi.fn()} onSaved={onSaved} />);

    clickHarnessPreset("DeepSeek");
    fireEvent.change(screen.getByLabelText("API Key"), {
      target: { value: "sk-harness" },
    });
    await passConnectionTest();
    openMoreOptions();

    expect(
      screen.getByRole("button", { name: /^glm-4\.7/ }),
    ).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "添加" }));

    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("upsert_agent", {
        profile: expect.objectContaining({
          primary_model: "glm-4.7",
        }),
      }),
    );
    expect(onSaved).toHaveBeenCalledTimes(1);
  });

  it("harness 已手选模型后测试成功不会被自动选中的末位覆盖", async () => {
    writeHarnessDeepSeekModelCache();
    invokeMock.mockImplementation((cmd: string) => {
      if (cmd === "detect_runtime") return Promise.resolve(detectReady());
      if (cmd === "fetch_agent_models")
        return Promise.resolve(["model-a", "model-b", "model-c"]);
      return Promise.resolve();
    });

    render(<AgentForm onCancel={vi.fn()} onSaved={vi.fn()} />);

    clickHarnessPreset("DeepSeek");
    openMoreOptions();
    fireEvent.click(
      await screen.findByRole("button", { name: /myagent 默认/ }),
    );
    fireEvent.click(screen.getByRole("menuitemradio", { name: /model-b/ }));
    expect(
      screen.getByRole("button", { name: /^model-b/ }),
    ).toBeInTheDocument();

    fireEvent.change(screen.getByLabelText("API Key"), {
      target: { value: "sk-harness" },
    });
    await passConnectionTest();

    expect(
      screen.getByRole("button", { name: /^model-b/ }),
    ).toBeInTheDocument();
  });

  it("harness 测试成功后改选另一个模型仍可直接保存（不再触发重测门禁）", async () => {
    invokeMock.mockImplementation((cmd: string) => {
      if (cmd === "detect_runtime") return Promise.resolve(detectReady());
      if (cmd === "fetch_agent_models")
        return Promise.resolve(["glm-4.5", "glm-4.6", "glm-4.7"]);
      return Promise.resolve();
    });
    const onSaved = vi.fn();

    render(<AgentForm onCancel={vi.fn()} onSaved={onSaved} />);

    clickHarnessPreset("DeepSeek");
    fireEvent.change(screen.getByLabelText("API Key"), {
      target: { value: "sk-harness" },
    });
    await passConnectionTest();
    openMoreOptions();

    // 测试成功已自动选中末位 glm-4.7，这里改选另一个模型
    fireEvent.click(screen.getByRole("button", { name: /^glm-4\.7/ }));
    fireEvent.click(screen.getByRole("menuitemradio", { name: /glm-4\.5/ }));

    expect(
      screen.getByRole("button", { name: /^glm-4\.5/ }),
    ).toBeInTheDocument();
    expect(screen.getByText(/连接成功/)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "添加" })).not.toBeDisabled();

    fireEvent.click(screen.getByRole("button", { name: "添加" }));

    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("upsert_agent", {
        profile: expect.objectContaining({
          primary_model: "glm-4.5",
        }),
      }),
    );
    expect(onSaved).toHaveBeenCalledTimes(1);
  });

  it("borrow 编辑态改主模型仍需重测才能保存（防 E2 波及 borrow）", async () => {
    invokeMock.mockImplementation((cmd: string) => {
      if (cmd === "test_agent_connection") return Promise.resolve({ ok: true });
      if (cmd === "fetch_agent_models") return Promise.resolve([]);
      return Promise.resolve();
    });

    render(
      <AgentForm
        agent={agent({ has_key: true })}
        onCancel={vi.fn()}
        onSaved={vi.fn()}
      />,
    );

    expect(screen.getByRole("button", { name: "保存" })).not.toBeDisabled();

    openMoreOptions();
    fireEvent.click(screen.getByRole("button", { name: /deepseek-v4-pro/ }));
    fireEvent.click(screen.getByRole("menuitemradio", { name: /自定义/ }));
    fireEvent.change(screen.getByLabelText("主模型"), {
      target: { value: "deepseek-v5" },
    });

    expect(screen.getByRole("button", { name: "保存" })).toBeDisabled();
    expect(screen.getByText("测试未通过，暂不能保存")).toBeInTheDocument();

    await passConnectionTest();

    expect(screen.getByRole("button", { name: "保存" })).not.toBeDisabled();
  });

  it("harness 默认模型文案不含「推荐」残留（zh/en）", () => {
    const { unmount } = render(
      <AgentForm onCancel={vi.fn()} onSaved={vi.fn()} />,
    );
    clickHarnessPreset("DeepSeek");
    openMoreOptions();

    expect(screen.getByLabelText("模型")).toHaveAttribute(
      "placeholder",
      "留空 = myagent 默认",
    );
    expect(screen.queryByText(/推荐/)).toBeNull();
    unmount();

    render(
      <I18nProvider initialLocale="en">
        <AgentForm onCancel={vi.fn()} onSaved={vi.fn()} />
      </I18nProvider>,
    );
    fireEvent.click(
      within(screen.getByLabelText("Engine")).getByRole("button", {
        name: "myagent",
      }),
    );
    fireEvent.click(
      within(screen.getByLabelText("LLM Provider")).getByRole("button", {
        name: /DeepSeek/,
      }),
    );
    fireEvent.click(screen.getByRole("button", { name: /More options/ }));

    expect(screen.getByLabelText("Model")).toHaveAttribute(
      "placeholder",
      "Blank = myagent default",
    );
    expect(screen.queryByText(/recommended/i)).toBeNull();
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

  it("harness 手改 endpoint 会更新输入框并清掉测试成功态", async () => {
    invokeMock.mockImplementation((cmd: string) => {
      if (cmd === "detect_runtime") return Promise.resolve(detectReady());
      if (cmd === "fetch_agent_models") return Promise.resolve(["deepseek-x"]);
      return Promise.resolve();
    });

    render(<AgentForm onCancel={vi.fn()} onSaved={vi.fn()} />);

    clickHarnessPreset("DeepSeek");
    fireEvent.change(screen.getByLabelText("API Key"), {
      target: { value: "sk-harness" },
    });
    await passConnectionTest();
    openMoreOptions();

    fireEvent.change(screen.getByLabelText("Endpoint"), {
      target: { value: "https://proxy.example/v1" },
    });

    expect(screen.getByLabelText("Endpoint")).toHaveValue(
      "https://proxy.example/v1",
    );
    expect(screen.queryByText(/连接成功/)).toBeNull();
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

  it("save_calls_upsert_then_set_key", async () => {
    vi.spyOn(Date, "now").mockReturnValue(1_700_000_000_000);
    invokeMock.mockImplementation(invokeWithConnectionOk);
    const onSaved = vi.fn();

    render(
      <AgentForm onCancel={vi.fn()} onSaved={onSaved} nextSortOrder={3} />,
    );

    clickBorrowPreset("DeepSeek");
    fireEvent.change(screen.getByLabelText("名称"), {
      target: { value: "DeepSeek Main" },
    });
    fireEvent.change(screen.getByLabelText("API Key"), {
      target: { value: "sk-local-only" },
    });
    await passConnectionTest();
    fireEvent.click(screen.getByRole("button", { name: "添加" }));

    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("upsert_agent", {
        profile: expect.objectContaining({
          id: "deepseek-main",
          name: "DeepSeek Main",
          access: "borrow",
          provider: "deepseek",
          primary_model: "deepseek-v4-pro",
          endpoint: "https://api.deepseek.com/anthropic",
          auth_mode: "bearer",
          reasoning_default: "medium",
          cap_reasoning: "low,medium,high",
          compat_proxy: "thinking_passback",
          compat_disable_nonessential: true,
          has_key: false,
          is_builtin: false,
          enabled: true,
          sort_order: 3,
          created_at: 1_700_000_000_000,
          updated_at: 1_700_000_000_000,
        }),
      }),
    );
    expect(invokeMock).toHaveBeenCalledWith("set_agent_key", {
      id: "deepseek-main",
      key: "sk-local-only",
    });
    const upsertIndex = invokeMock.mock.calls.findIndex(
      ([cmd]) => cmd === "upsert_agent",
    );
    const setKeyIndex = invokeMock.mock.calls.findIndex(
      ([cmd]) => cmd === "set_agent_key",
    );
    expect(upsertIndex).toBeGreaterThan(-1);
    expect(setKeyIndex).toBeGreaterThan(upsertIndex);
    expect(onSaved).toHaveBeenCalledTimes(1);
  });

  it("新增 myagent harness DeepSeek 测试通过后可保存且不暴露借壳专属字段", async () => {
    vi.spyOn(Date, "now").mockReturnValue(1_700_000_000_000);
    const calls: any[] = [];
    invokeMock.mockImplementation((cmd: string, args?: any) => {
      calls.push([cmd, args]);
      if (cmd === "detect_runtime") return Promise.resolve(detectReady());
      if (cmd === "fetch_agent_models") return Promise.resolve(["deepseek-x"]);
      return Promise.resolve();
    });
    const onSaved = vi.fn();

    render(
      <AgentForm onCancel={vi.fn()} onSaved={onSaved} nextSortOrder={3} />,
    );

    clickHarnessPreset("DeepSeek");

    expect(
      screen.getByText(/myagent 直连该 provider（OpenAI 兼容）/),
    ).toBeInTheDocument();
    openMoreOptions();
    const modelInput = screen.getByLabelText("模型");
    expect(modelInput).toBeInstanceOf(HTMLInputElement);
    expect(modelInput).toHaveAttribute("placeholder", "留空 = myagent 默认");
    expect(modelInput).not.toBeRequired();
    expect(screen.queryByRole("button", { name: "↩ 从列表选择" })).toBeNull();
    expect(screen.getByTestId("test-conn-btn")).toBeInTheDocument();
    expect(screen.queryByLabelText("opus")).toBeNull();
    expect(screen.queryByLabelText("鉴权方式")).toBeNull();

    fireEvent.change(screen.getByLabelText("API Key"), {
      target: { value: "sk-harness" },
    });
    expect(screen.getByRole("button", { name: "添加" })).toBeDisabled();
    await passConnectionTest();
    expect(calls.some((call) => call[0] === "test_agent_connection")).toBe(
      false,
    );
    expect(calls.find((call) => call[0] === "fetch_agent_models")?.[1]).toEqual(
      expect.objectContaining({
        modelsEndpoint: "https://api.deepseek.com/v1/models",
        authMode: null,
        apiKey: "sk-harness",
      }),
    );
    expect(screen.getByRole("button", { name: "添加" })).not.toBeDisabled();
    fireEvent.click(screen.getByRole("button", { name: "添加" }));

    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("upsert_agent", {
        profile: expect.objectContaining({
          id: "deepseek-myagent",
          name: "DeepSeek（myagent）",
          access: "harness",
          provider: "deepseek",
          // 测试成功后模型字段一直未被手动改过 → E1 自动选中列表末位
          // （这里只有一个模型 deepseek-x），不再落回空值。
          primary_model: "deepseek-x",
          endpoint: "https://api.deepseek.com/v1",
          auth_mode: null,
          model_opus: null,
          model_sonnet: null,
          model_haiku: null,
          model_subagent: null,
          api_timeout_ms: 600000,
          sort_order: 3,
        }),
      }),
    );
    expect(invokeMock).toHaveBeenCalledWith("set_agent_key", {
      id: "deepseek-myagent",
      key: "sk-harness",
    });
    expect(onSaved).toHaveBeenCalledTimes(1);
  });

  it("harness 测试连接 401 展示 auth 分类且添加保持 disabled", async () => {
    invokeMock.mockImplementation((cmd: string) => {
      if (cmd === "detect_runtime") return Promise.resolve(detectReady());
      if (cmd === "fetch_agent_models") return Promise.reject("HTTP 401");
      return Promise.resolve();
    });

    render(<AgentForm onCancel={vi.fn()} onSaved={vi.fn()} />);

    clickHarnessPreset("DeepSeek");
    fireEvent.change(screen.getByLabelText("API Key"), {
      target: { value: "sk-bad" },
    });
    fireEvent.click(screen.getByTestId("test-conn-btn"));

    expect(await screen.findByText(/key 无效或无权限/)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "添加" })).toBeDisabled();
  });

  it("harness 测试连接 endpoint 为空时直接提示且不拉模型", async () => {
    const calls: unknown[][] = [];
    invokeMock.mockImplementation((cmd: string, args?: unknown) => {
      calls.push([cmd, args]);
      if (cmd === "detect_runtime") return Promise.resolve(detectReady());
      if (cmd === "fetch_agent_models") return Promise.reject("reqwest error");
      return Promise.resolve();
    });

    render(<AgentForm onCancel={vi.fn()} onSaved={vi.fn()} />);

    clickHarnessPreset("DeepSeek");
    openMoreOptions();
    fireEvent.change(screen.getByLabelText("Endpoint"), {
      target: { value: "" },
    });
    fireEvent.click(screen.getByTestId("test-conn-btn"));

    expect(await screen.findByText(/请先填写 endpoint/)).toBeInTheDocument();
    expect(calls.find((call) => call[0] === "fetch_agent_models")).toBe(
      undefined,
    );
  });

  it("harness 测试连接成功显示已拉到模型数并灌入模型下拉", async () => {
    invokeMock.mockImplementation((cmd: string) => {
      if (cmd === "detect_runtime") return Promise.resolve(detectReady());
      if (cmd === "fetch_agent_models")
        return Promise.resolve(["deepseek-x", "deepseek-y"]);
      return Promise.resolve();
    });

    render(<AgentForm onCancel={vi.fn()} onSaved={vi.fn()} />);

    clickHarnessPreset("DeepSeek");
    fireEvent.change(screen.getByLabelText("API Key"), {
      target: { value: "sk-harness" },
    });
    fireEvent.click(screen.getByTestId("test-conn-btn"));

    expect(await screen.findByText(/已拉到 2 个模型/)).toBeInTheDocument();
    openMoreOptions();
    // 模型字段一直未被手动改过 → E1 测试成功自动选中列表末位 deepseek-y
    expect(
      screen.getByRole("button", { name: /^deepseek-y/ }),
    ).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: /^deepseek-y/ }));
    expect(
      screen.getByRole("menuitemradio", { name: /deepseek-x/ }),
    ).toBeInTheDocument();
  });

  it("harness 测试连接成功且模型列表为空时显示 0 个模型", async () => {
    invokeMock.mockImplementation((cmd: string) => {
      if (cmd === "detect_runtime") return Promise.resolve(detectReady());
      if (cmd === "fetch_agent_models") return Promise.resolve([]);
      return Promise.resolve();
    });

    render(<AgentForm onCancel={vi.fn()} onSaved={vi.fn()} />);

    clickHarnessPreset("DeepSeek");
    fireEvent.change(screen.getByLabelText("API Key"), {
      target: { value: "sk-harness" },
    });
    fireEvent.click(screen.getByTestId("test-conn-btn"));

    expect(await screen.findByText(/连接成功/)).toBeInTheDocument();
    expect(screen.getByText(/已拉到 0 个模型/)).toBeInTheDocument();
  });

  it("borrow 和 harness 新建未测试通过时保存按钮 disabled", () => {
    const { unmount } = render(
      <AgentForm onCancel={vi.fn()} onSaved={vi.fn()} />,
    );

    clickBorrowPreset("DeepSeek");
    expect(screen.getByRole("button", { name: "添加" })).toBeDisabled();

    fireEvent.change(screen.getByLabelText("API Key"), {
      target: { value: "sk-borrow" },
    });

    expect(screen.getByRole("button", { name: "添加" })).toBeDisabled();
    expect(screen.getByText("测试未通过，暂不能保存")).toBeInTheDocument();
    unmount();

    render(<AgentForm onCancel={vi.fn()} onSaved={vi.fn()} />);

    clickHarnessPreset("DeepSeek");
    expect(screen.getByRole("button", { name: "添加" })).toBeDisabled();

    fireEvent.change(screen.getByLabelText("API Key"), {
      target: { value: "sk-harness" },
    });

    expect(screen.getByRole("button", { name: "添加" })).toBeDisabled();
    expect(screen.getByText("测试未通过，暂不能保存")).toBeInTheDocument();
  });

  it("编辑已有 borrow agent 改连接参数后必须重新测试才能保存", async () => {
    invokeMock.mockImplementation((cmd: string) => {
      if (cmd === "test_agent_connection") return Promise.resolve({ ok: true });
      return Promise.resolve();
    });

    render(
      <AgentForm
        agent={agent({ has_key: true })}
        onCancel={vi.fn()}
        onSaved={vi.fn()}
      />,
    );

    expect(screen.getByRole("button", { name: "保存" })).not.toBeDisabled();

    openMoreOptions();
    fireEvent.change(screen.getByLabelText("Endpoint"), {
      target: { value: "https://alt.example/anthropic" },
    });

    expect(screen.getByRole("button", { name: "保存" })).toBeDisabled();
    expect(screen.getByText("测试未通过，暂不能保存")).toBeInTheDocument();

    await passConnectionTest();

    expect(screen.getByRole("button", { name: "保存" })).not.toBeDisabled();
  });

  it("编辑态改鉴权方式或改 API Key 后保存置灰须重测", () => {
    invokeMock.mockImplementation(() => Promise.resolve());

    const { unmount } = render(
      <AgentForm
        agent={agent({ has_key: true })}
        onCancel={vi.fn()}
        onSaved={vi.fn()}
      />,
    );
    openMoreOptions();
    fireEvent.click(screen.getByRole("button", { name: /x-api-key/ }));
    expect(screen.getByRole("button", { name: "保存" })).toBeDisabled();
    expect(screen.getByText("测试未通过，暂不能保存")).toBeInTheDocument();
    unmount();

    render(
      <AgentForm
        agent={agent({ has_key: true })}
        onCancel={vi.fn()}
        onSaved={vi.fn()}
      />,
    );
    fireEvent.change(screen.getByLabelText("API Key"), {
      target: { value: "sk-new-key" },
    });
    expect(screen.getByRole("button", { name: "保存" })).toBeDisabled();
  });

  it("编辑态只改名不动连接参数可直接保存", async () => {
    const calls: Array<[string, any]> = [];
    invokeMock.mockImplementation((cmd: string, args?: any) => {
      calls.push([cmd, args]);
      return Promise.resolve();
    });

    render(
      <AgentForm
        agent={agent({ has_key: true })}
        onCancel={vi.fn()}
        onSaved={vi.fn()}
      />,
    );
    fireEvent.change(screen.getByLabelText("名称"), {
      target: { value: "新名字" },
    });
    const save = screen.getByRole("button", { name: "保存" });
    expect(save).not.toBeDisabled();
    fireEvent.click(save);
    await waitFor(() =>
      expect(
        calls.some(
          (c) => c[0] === "upsert_agent" && c[1]?.profile?.name === "新名字",
        ),
      ).toBe(true),
    );
  });

  it("保存被门禁挡时 form submit 事件也不触发 upsert（兜底）", () => {
    const calls: string[] = [];
    invokeMock.mockImplementation((cmd: string) => {
      calls.push(cmd);
      return Promise.resolve();
    });

    const { container } = render(
      <AgentForm
        agent={agent({ has_key: true })}
        onCancel={vi.fn()}
        onSaved={vi.fn()}
      />,
    );
    openMoreOptions();
    fireEvent.change(screen.getByLabelText("Endpoint"), {
      target: { value: "https://alt.example/anthropic" },
    });
    fireEvent.submit(container.querySelector("form")!);
    expect(calls).not.toContain("upsert_agent");
  });

  it("native 检测未安装会禁用保存，已安装但未探测登录凭据仍可保存", async () => {
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

    render(<AgentForm onCancel={vi.fn()} onSaved={vi.fn()} />);

    fireEvent.click(providerChip("Anthropic 账号"));

    expect(await screen.findByText(/未检测到 claude CLI/)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "添加" })).toBeDisabled();

    clickEngine("Codex CLI");

    expect(await screen.findByText(/未探测到登录凭据/)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "添加" })).not.toBeDisabled();
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

  it("旧 z.ai agent 编辑保存 → provider canonical 为 zhipu", async () => {
    const saved: AgentProfile[] = [];
    invokeMock.mockImplementation((cmd: string, args?: any) => {
      if (cmd === "upsert_agent") saved.push(args.profile);
      return Promise.resolve();
    });

    render(
      <AgentForm
        agent={agent({
          id: "old-glm",
          name: "GLM",
          provider: "z.ai",
          endpoint: "https://api.z.ai/api/anthropic",
          primary_model: "glm-4.7",
          access: "borrow",
          has_key: true,
        })}
        onCancel={vi.fn()}
        onSaved={vi.fn()}
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: "保存" }));

    await waitFor(() => expect(saved[0]?.provider).toBe("zhipu"));
  });

  it("borrow_requires_endpoint", async () => {
    invokeMock.mockImplementation(invokeWithConnectionOk);

    render(<AgentForm onCancel={vi.fn()} onSaved={vi.fn()} />);

    clickBorrowPreset("DeepSeek");
    fireEvent.change(screen.getByLabelText("名称"), {
      target: { value: "DeepSeek Agent" },
    });
    openMoreOptions();
    fireEvent.change(screen.getByLabelText("Endpoint"), {
      target: { value: "" },
    });
    fireEvent.change(screen.getByLabelText("API Key"), {
      target: { value: "sk-local" },
    });
    await passConnectionTest();
    fireEvent.click(screen.getByRole("button", { name: "添加" }));

    expect(
      invokeMock.mock.calls.some(([command]) => command === "upsert_agent"),
    ).toBe(false);
    expect(screen.getByText("该 agent 需要填写 Endpoint")).toBeInTheDocument();
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

  it("unknown model warning is non-blocking and localized in English", async () => {
    invokeMock.mockImplementation(invokeWithConnectionOk);
    const onSaved = vi.fn();

    render(
      <I18nProvider initialLocale="en">
        <AgentForm
          agent={agent({
            id: "claude",
            name: "Claude CLI",
            access: "native",
            provider: "claude",
            primary_model: "fable5",
            endpoint: null,
          })}
          onCancel={vi.fn()}
          onSaved={onSaved}
        />
      </I18nProvider>,
    );

    expect(
      screen.getByText(
        "Unrecognized model id — double-check the spelling (e.g. claude-fable-5). Saving is allowed, but the agent may fail to start.",
      ),
    ).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Save" })).not.toBeDisabled();
    fireEvent.click(screen.getByRole("button", { name: "Save" }));

    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("upsert_agent", {
        profile: expect.objectContaining({
          primary_model: "fable5",
        }),
      }),
    );
    expect(onSaved).toHaveBeenCalledTimes(1);
  });

  it("自由文本态的已知模型和空值不显示未识别警示", () => {
    render(
      <AgentForm
        agent={agent({
          id: "claude",
          access: "native",
          provider: "claude",
          primary_model: "fable",
          endpoint: null,
        })}
        onCancel={vi.fn()}
        onSaved={vi.fn()}
      />,
    );

    fireEvent.click(screen.getByRole("button", { name: /^fable/ }));
    fireEvent.click(screen.getByRole("menuitemradio", { name: /自定义/ }));

    const modelInput = screen.getByLabelText("模型");
    expect(modelInput).toHaveValue("fable");
    expect(screen.queryByText(/未识别的模型 id/)).toBeNull();

    fireEvent.change(modelInput, { target: { value: "" } });
    expect(screen.queryByText(/未识别的模型 id/)).toBeNull();
  });

  it("custom 新建无 endpoint 报错不提交", async () => {
    invokeMock.mockImplementation(invokeWithConnectionOk);
    render(<AgentForm agent={null} onCancel={vi.fn()} onSaved={vi.fn()} />);

    fireEvent.click(providerChip("自定义"));
    fireEvent.change(screen.getByLabelText("名称"), {
      target: { value: "Custom Agent" },
    });
    openMoreOptions();
    fireEvent.change(screen.getByLabelText("主模型"), {
      target: { value: "remote-model" },
    });
    fireEvent.change(screen.getByLabelText("API Key"), {
      target: { value: "sk-custom" },
    });
    await passConnectionTest();
    fireEvent.click(screen.getByRole("button", { name: "添加" }));

    expect(screen.getByText("该 agent 需要填写 Endpoint")).toBeInTheDocument();
    expect(
      invokeMock.mock.calls.some(([command]) => command === "upsert_agent"),
    ).toBe(false);
  });

  it("borrow 选 x-api-key 提交 auth_mode=x_api_key", async () => {
    invokeMock.mockImplementation(invokeWithConnectionOk);

    render(<AgentForm agent={null} onCancel={vi.fn()} onSaved={vi.fn()} />);

    clickBorrowPreset("DeepSeek");
    fireEvent.change(screen.getByLabelText("名称"), {
      target: { value: "DeepSeek Main" },
    });
    fireEvent.change(screen.getByLabelText("API Key"), {
      target: { value: "sk-local" },
    });
    openMoreOptions();
    fireEvent.click(screen.getByRole("button", { name: /x-api-key/ }));
    await passConnectionTest();
    fireEvent.click(screen.getByRole("button", { name: "添加" }));

    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("upsert_agent", {
        profile: expect.objectContaining({ auth_mode: "x_api_key" }),
      }),
    );
  });

  it("api_key_field_renders_existing_key_status_without_value", () => {
    render(
      <AgentForm
        agent={agent({ has_key: true })}
        onCancel={vi.fn()}
        onSaved={vi.fn()}
      />,
    );

    expect(screen.getByLabelText("API Key")).toHaveValue("");
    expect(screen.getByLabelText("API Key")).toHaveAttribute(
      "placeholder",
      "已配置 · 留空保留原 key",
    );
    expect(screen.getByText("已配 ✓")).toBeInTheDocument();
  });

  it("API Key 可显示或隐藏明文输入", () => {
    render(<AgentForm agent={null} onCancel={vi.fn()} onSaved={vi.fn()} />);

    clickBorrowPreset("DeepSeek");

    expect(screen.getByLabelText("显示 API Key")).toBeInTheDocument();
    expect(screen.getByLabelText("API Key")).toHaveAttribute(
      "type",
      "password",
    );

    fireEvent.click(screen.getByLabelText("显示 API Key"));

    expect(screen.getByLabelText("隐藏 API Key")).toBeInTheDocument();
    expect(screen.getByLabelText("API Key")).toHaveAttribute("type", "text");

    fireEvent.click(screen.getByLabelText("隐藏 API Key"));

    expect(screen.getByLabelText("显示 API Key")).toBeInTheDocument();
    expect(screen.getByLabelText("API Key")).toHaveAttribute(
      "type",
      "password",
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

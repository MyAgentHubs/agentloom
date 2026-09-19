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
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import { openUrl } from "@tauri-apps/plugin-opener";
import type { AgentProfile, ConnectionTestResult } from "../../types/agent";
import { AgentForm } from "./AgentForm";
import { deriveModelMapping, writeModelCache } from "./agentFormHelpers";
import {
  agent,
  deferred,
  detectReady,
  invokeWithConnectionOk,
  providerChip,
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

  it("连接测试未通过时仍会禁用保存", async () => {
    invokeMock.mockImplementation(invokeWithConnectionOk);

    render(
      <AgentForm
        agent={agent({ has_key: true })}
        onCancel={vi.fn()}
        onSaved={vi.fn()}
      />,
    );
    await screen.findAllByText("✓ 已安装 · 已登录");
    openMoreOptions();
    fireEvent.change(screen.getByLabelText("Endpoint"), {
      target: { value: "https://unverified.example/anthropic" },
    });

    expect(screen.getByText("测试未通过，暂不能保存")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "保存" })).toBeDisabled();
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
});

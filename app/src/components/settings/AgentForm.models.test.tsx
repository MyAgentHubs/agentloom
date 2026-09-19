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
import { deriveModelMapping, writeModelCache } from "./agentFormHelpers";
import {
  agent,
  detectReady,
  invokeWithConnectionOk,
  clickBorrowPreset,
  clickHarnessPreset,
  passConnectionTest,
  openMoreOptions,
  writeHarnessDeepSeekModelCache,
  expectAutoMark,
  expectNoAutoMark,
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
});

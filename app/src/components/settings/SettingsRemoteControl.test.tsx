import {
  act,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { invoke } from "@tauri-apps/api/core";
import QRCode from "qrcode";
import {
  SettingsRemoteControl,
  base64UrlEncode,
  buildPairingQrUrl,
  type RemotePairingPayload,
} from "./SettingsRemoteControl";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

const repoOne = {
  id: "repo-1",
  source: "local",
  owner: null,
  name: "Repo One",
  path: "/tmp/repo-1",
  status: "active",
  added_at: 1_700_000_000,
  last_used_at: null,
  namespace_id: "local",
};

const repoTwo = {
  id: "repo-2",
  source: "local",
  owner: null,
  name: "Repo Two",
  path: "/tmp/repo-2",
  status: "active",
  added_at: 1_700_000_100,
  last_used_at: null,
  namespace_id: "local",
};

describe("SettingsRemoteControl", () => {
  const invokeMock = vi.mocked(invoke);
  let deviceListCalls = 0;
  let gatewayStatus: {
    running: boolean;
    stopped_reason: string | null;
    last_error?: string | null;
    counters?: Record<string, number | string>;
  };

  const activeDevice = {
    device_id: "phone-1",
    name: "Alice 的手机",
    created_at: 1_750_000_000,
    access_expires_at: 1_760_000_000_000,
    revoked_at: null,
  };

  beforeEach(() => {
    deviceListCalls = 0;
    gatewayStatus = { running: true, stopped_reason: null };
    invokeMock.mockReset();
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "remote_control_get_settings") {
        return {
          enabled: true,
          relay_url: "wss://relay.example.com",
          default_relay_url: "wss://agentloom.myagenthubs.com",
          active_repo_id: "repo-1",
        };
      }
      if (cmd === "list_repos") {
        return [repoOne, repoTwo];
      }
      if (cmd === "remote_devices_list") {
        deviceListCalls += 1;
        return [activeDevice];
      }
      if (cmd === "remote_pairing_begin") {
        return {
          v: 1,
          relay_url: "wss://relay.example.com",
          room: "room-1",
          pairing_token: "token-1",
          desktop_pub: "desktop-pub-1",
        };
      }
      if (cmd === "remote_pairing_status") {
        return {
          state: "WaitingForHello",
          expires_at: 1_750_000_300,
        };
      }
      if (cmd === "remote_gateway_status") {
        return gatewayStatus;
      }
      return undefined;
    });
  });

  afterEach(() => {
    vi.useRealTimers();
    vi.restoreAllMocks();
  });

  it("挂载读取 enabled 与 relay_url 并渲染", async () => {
    render(<SettingsRemoteControl />);

    expect(
      await screen.findByDisplayValue("wss://relay.example.com"),
    ).toBeInTheDocument();
    expect(
      screen.getByRole("switch", { name: "允许手机远程控制" }),
    ).toHaveAttribute("aria-checked", "true");
    expect(invokeMock).toHaveBeenCalledWith("remote_control_get_settings");
    expect(invokeMock).toHaveBeenCalledWith("remote_devices_list");
  });

  it("内置公共中继单：relay 输入框留空时 placeholder 展示后端返回的官方公共中继地址", async () => {
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "remote_control_get_settings") {
        return {
          enabled: true,
          relay_url: "",
          default_relay_url: "wss://agentloom.myagenthubs.com",
          active_repo_id: "repo-1",
        };
      }
      if (cmd === "list_repos") return [repoOne, repoTwo];
      if (cmd === "remote_devices_list") return [activeDevice];
      if (cmd === "remote_gateway_status") return gatewayStatus;
      return undefined;
    });

    render(<SettingsRemoteControl />);

    const input =
      await screen.findByLabelText<HTMLInputElement>("Relay 服务器地址");
    await waitFor(() =>
      expect(input).toHaveAttribute(
        "placeholder",
        "wss://agentloom.myagenthubs.com",
      ),
    );
    expect(input).toHaveValue("");
  });

  it("诊断区默认折叠", async () => {
    render(<SettingsRemoteControl />);

    const summary = await screen.findByText("诊断");
    expect(summary.closest("details")).not.toHaveAttribute("open");
  });

  it("展开诊断区后渲染 last_error 与 counters 键值", async () => {
    gatewayStatus = {
      running: true,
      stopped_reason: null,
      last_error: "socket closed",
      counters: { frames_seen: 12, upstream_repo_filtered: 3 },
    };
    render(<SettingsRemoteControl />);

    fireEvent.click(await screen.findByText("诊断"));

    expect(await screen.findByText("socket closed")).toBeInTheDocument();
    expect(screen.getByText("frames_seen")).toBeInTheDocument();
    expect(screen.getByText("12")).toBeInTheDocument();
    expect(screen.getByText("upstream_repo_filtered")).toBeInTheDocument();
    expect(screen.getByText("3")).toBeInTheDocument();
  });

  it("诊断区渲染 gateway 断连计数器与最近断连原因", async () => {
    gatewayStatus = {
      running: true,
      stopped_reason: null,
      counters: {
        keepalive_pings_sent: 19,
        disconnect_config_stale: 20,
        disconnect_closed_by_peer: 21,
        disconnect_error: 22,
        last_disconnect_reason: "closed_by_peer",
      },
    };
    render(<SettingsRemoteControl />);

    fireEvent.click(await screen.findByText("诊断"));

    expect(await screen.findByText("keepalive_pings_sent")).toBeInTheDocument();
    expect(screen.getByText("19")).toBeInTheDocument();
    expect(screen.getByText("disconnect_config_stale")).toBeInTheDocument();
    expect(screen.getByText("20")).toBeInTheDocument();
    expect(screen.getByText("disconnect_closed_by_peer")).toBeInTheDocument();
    expect(screen.getByText("21")).toBeInTheDocument();
    expect(screen.getByText("disconnect_error")).toBeInTheDocument();
    expect(screen.getByText("22")).toBeInTheDocument();
    expect(
      await screen.findByText("last_disconnect_reason"),
    ).toBeInTheDocument();
    expect(screen.getByText("closed_by_peer")).toBeInTheDocument();
  });

  it("最近断连原因为空字符串时显示占位符", async () => {
    gatewayStatus = {
      running: true,
      stopped_reason: null,
      counters: { last_disconnect_reason: "" },
    };
    render(<SettingsRemoteControl />);

    fireEvent.click(await screen.findByText("诊断"));

    expect(
      await screen.findByText("last_disconnect_reason"),
    ).toBeInTheDocument();
    expect(screen.getByText("—")).toBeInTheDocument();
  });

  it("旧版 IPC 缺少 counters 字段时诊断区仍可展开", async () => {
    gatewayStatus = { running: true, stopped_reason: null };
    render(<SettingsRemoteControl />);

    const summary = await screen.findByText("诊断");
    fireEvent.click(summary);

    expect(summary.closest("details")).toHaveAttribute("open");
    expect(screen.getByText("暂无诊断数据")).toBeInTheDocument();
  });

  it("挂载后活跃项目选择器展示可选项目与当前活跃值", async () => {
    render(<SettingsRemoteControl />);

    const select = await screen.findByLabelText("活跃项目");
    await waitFor(() => expect(select).toHaveValue("repo-1"));
    expect(
      screen.getByRole("option", { name: "Repo One" }),
    ).toBeInTheDocument();
    expect(
      screen.getByRole("option", { name: "Repo Two" }),
    ).toBeInTheDocument();
    expect(invokeMock).toHaveBeenCalledWith("list_repos");
  });

  it("未设置活跃项目时禁用开始配对并展示引导文案", async () => {
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "remote_control_get_settings") {
        return {
          enabled: true,
          relay_url: "wss://relay.example.com",
          active_repo_id: null,
        };
      }
      if (cmd === "list_repos") return [repoOne, repoTwo];
      if (cmd === "remote_devices_list") return [activeDevice];
      if (cmd === "remote_gateway_status") return gatewayStatus;
      return undefined;
    });

    render(<SettingsRemoteControl />);

    expect(
      await screen.findByText(
        "配对与设备都归属当前活跃项目的房间，请先选择一个项目再开始配对。",
      ),
    ).toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: "生成配对二维码" }),
    ).toBeDisabled();
  });

  it("M24D-DEVLIST：未设置活跃项目时设备区展示引导语，而不是空态/设备行文案", async () => {
    // 即使 remote_devices_list 的 IPC mock 仍返回一台设备（模拟后端过滤前的旧行为/时序竞
    // 争），前端在未设活跃项目时也必须优先展示引导语——不该把这台设备渲染出来，也不该落进
    // 「还没有已配对的设备」这条容易误导用户的空态文案。
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "remote_control_get_settings") {
        return {
          enabled: true,
          relay_url: "wss://relay.example.com",
          active_repo_id: null,
        };
      }
      if (cmd === "list_repos") return [repoOne, repoTwo];
      if (cmd === "remote_devices_list") return [activeDevice];
      if (cmd === "remote_gateway_status") return gatewayStatus;
      return undefined;
    });

    render(<SettingsRemoteControl />);

    expect(
      await screen.findByText("选择活跃项目后，才能看到该项目配对的设备。"),
    ).toBeInTheDocument();
    expect(screen.queryByText("Alice 的手机")).not.toBeInTheDocument();
    expect(screen.queryByText("还没有已配对的设备")).not.toBeInTheDocument();
  });

  it("切换活跃项目后调用 remote_set_active_project 并刷新 settings 与网关状态", async () => {
    let activeRepoIdOnBackend: string | null = "repo-1";
    let getSettingsCalls = 0;
    let gatewayStatusCalls = 0;
    invokeMock.mockImplementation(async (cmd: string, args?: any) => {
      if (cmd === "remote_control_get_settings") {
        getSettingsCalls += 1;
        return {
          enabled: true,
          relay_url: "wss://relay.example.com",
          active_repo_id: activeRepoIdOnBackend,
        };
      }
      if (cmd === "list_repos") return [repoOne, repoTwo];
      if (cmd === "remote_devices_list") return [activeDevice];
      if (cmd === "remote_gateway_status") {
        gatewayStatusCalls += 1;
        return gatewayStatus;
      }
      if (cmd === "remote_set_active_project") {
        activeRepoIdOnBackend = (args?.repoId as string | null) ?? null;
        return undefined;
      }
      return undefined;
    });

    render(<SettingsRemoteControl />);
    const select = await screen.findByLabelText("活跃项目");
    await waitFor(() => expect(select).toHaveValue("repo-1"));

    const settingsCallsBefore = getSettingsCalls;
    const gatewayCallsBefore = gatewayStatusCalls;
    fireEvent.change(select, { target: { value: "repo-2" } });

    expect(invokeMock).toHaveBeenCalledWith("remote_set_active_project", {
      repoId: "repo-2",
    });
    await waitFor(() => expect(select).toHaveValue("repo-2"));
    await waitFor(() =>
      expect(getSettingsCalls).toBeGreaterThan(settingsCallsBefore),
    );
    await waitFor(() =>
      expect(gatewayStatusCalls).toBeGreaterThan(gatewayCallsBefore),
    );
  });

  it("DEVLIST 返工·项 2：切换活跃项目成功后重新拉取设备列表，旧房设备行不再展示", async () => {
    const deviceRoomOne = {
      device_id: "phone-room-one",
      name: "Room One 手机",
      created_at: 1_750_000_000,
      access_expires_at: 1_760_000_000_000,
      revoked_at: null,
    };
    const deviceRoomTwo = {
      device_id: "phone-room-two",
      name: "Room Two 手机",
      created_at: 1_750_000_100,
      access_expires_at: 1_760_000_100_000,
      revoked_at: null,
    };
    let activeRepoIdOnBackend: string | null = "repo-1";
    let deviceListCallCount = 0;
    invokeMock.mockImplementation(async (cmd: string, args?: any) => {
      if (cmd === "remote_control_get_settings") {
        return {
          enabled: true,
          relay_url: "wss://relay.example.com",
          active_repo_id: activeRepoIdOnBackend,
        };
      }
      if (cmd === "list_repos") return [repoOne, repoTwo];
      if (cmd === "remote_devices_list") {
        deviceListCallCount += 1;
        // 第一次挂载时的设备列表是 repo-1 房的；切到 repo-2 后必须重拉——第二次起返回 repo-2
        // 房的设备集合，旧集合不该继续挂在 UI 上（本单要修的接缝：切项目后不重拉，旧房设备
        // 行残留可点，撤销会打进新房、relay CAS 恒拒、端到端断裂）。
        return deviceListCallCount === 1 ? [deviceRoomOne] : [deviceRoomTwo];
      }
      if (cmd === "remote_gateway_status") return gatewayStatus;
      if (cmd === "remote_set_active_project") {
        activeRepoIdOnBackend = (args?.repoId as string | null) ?? null;
        return undefined;
      }
      return undefined;
    });

    render(<SettingsRemoteControl />);
    const select = await screen.findByLabelText("活跃项目");
    await waitFor(() => expect(select).toHaveValue("repo-1"));
    expect(await screen.findByText("Room One 手机")).toBeInTheDocument();

    fireEvent.change(select, { target: { value: "repo-2" } });

    await waitFor(() => expect(select).toHaveValue("repo-2"));
    await waitFor(() => expect(deviceListCallCount).toBeGreaterThanOrEqual(2));
    expect(await screen.findByText("Room Two 手机")).toBeInTheDocument();
    expect(screen.queryByText("Room One 手机")).not.toBeInTheDocument();
  });

  it("切换活跃项目失败时回退选择并展示后端错误文案", async () => {
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "remote_control_get_settings") {
        return {
          enabled: true,
          relay_url: "wss://relay.example.com",
          active_repo_id: "repo-1",
        };
      }
      if (cmd === "list_repos") return [repoOne, repoTwo];
      if (cmd === "remote_devices_list") return [activeDevice];
      if (cmd === "remote_gateway_status") return gatewayStatus;
      if (cmd === "remote_set_active_project") {
        throw 'AL_ERR:remoteControl.activeProjectMissing:{"repoId":"repo-2"}';
      }
      return undefined;
    });

    render(<SettingsRemoteControl />);
    const select = await screen.findByLabelText("活跃项目");
    await waitFor(() => expect(select).toHaveValue("repo-1"));

    fireEvent.change(select, { target: { value: "repo-2" } });

    await waitFor(() => expect(select).toHaveValue("repo-1"));
    expect(
      await screen.findByText("找不到该项目（id：repo-2），无法设为活跃项目"),
    ).toBeInTheDocument();
  });

  it("gateway 因当前活跃项目房间归属冲突停机时展示项目维度提示", async () => {
    gatewayStatus = {
      running: false,
      stopped_reason: "room_claim_conflict_project",
    };

    render(<SettingsRemoteControl />);

    expect(
      await screen.findByText(
        "当前活跃项目的房间被另一台桌面占用·per-project 房间不支持自动换房，本机已让位停止，请到设置重新配对该项目",
      ),
    ).toBeInTheDocument();
  });

  it("gateway 因房间归属冲突停机时展示保护已配对设备的原因", async () => {
    gatewayStatus = {
      running: false,
      stopped_reason: "room_claim_conflict",
    };

    render(<SettingsRemoteControl />);

    expect(
      await screen.findByText(
        "房间归属被其它桌面占用·为保已配对设备未自动换房",
      ),
    ).toBeInTheDocument();
  });

  it("gateway 因房间终结停机时展示重新配对提示", async () => {
    gatewayStatus = {
      running: false,
      stopped_reason: "room_tombstoned",
    };

    render(<SettingsRemoteControl />);

    expect(
      await screen.findByText("房间已在服务端终结（410）·需重新配对"),
    ).toBeInTheDocument();
  });

  it.each([
    [
      "room_device_status_unavailable",
      "无法确认配对设备状态·为保护既有设备已停机",
    ],
  ])("gateway 停机码 %s 展示明确提示", async (stoppedReason, message) => {
    gatewayStatus = {
      running: false,
      stopped_reason: stoppedReason,
    };

    render(<SettingsRemoteControl />);

    expect(await screen.findByText(message)).toBeInTheDocument();
  });

  it.each(["room_regeneration_limit", "room_regeneration_failed"])(
    "M24D-DEVLIST：换房死分支已删——停机码 %s 落回通用 unknown 兜底文案（不再有专属渲染分支）",
    async (stoppedReason) => {
      gatewayStatus = {
        running: false,
        stopped_reason: stoppedReason,
      };

      render(<SettingsRemoteControl />);

      expect(
        await screen.findByText(
          `远程控制已停止（代码：${stoppedReason}）·修改设置或重新配对可恢复`,
        ),
      ).toBeInTheDocument();
    },
  );

  it("gateway 因 registry rebase 达到上限时展示恢复提示", async () => {
    gatewayStatus = {
      running: false,
      stopped_reason: "registry_rebase_limit",
    };

    render(<SettingsRemoteControl />);

    expect(
      await screen.findByText(
        "Relay 注册表高水位连续抬升·远程控制已停止，请修改设置或重新配对",
      ),
    ).toBeInTheDocument();
  });

  it("gateway 返回未知停机码时展示带代码的通用恢复提示", async () => {
    gatewayStatus = {
      running: false,
      stopped_reason: "future_stop_reason",
    };

    render(<SettingsRemoteControl />);

    expect(
      await screen.findByText(
        "远程控制已停止（代码：future_stop_reason）·修改设置或重新配对可恢复",
      ),
    ).toBeInTheDocument();
  });

  it("生成配对二维码后渲染 SVG", async () => {
    const { container } = render(<SettingsRemoteControl />);

    const generateButton = screen.getByRole("button", {
      name: "生成配对二维码",
    });
    await waitFor(() => expect(generateButton).toBeEnabled());
    fireEvent.click(generateButton);

    await waitFor(() => {
      expect(container.querySelector("svg")).not.toBeNull();
    });
    expect(invokeMock).toHaveBeenCalledWith("remote_pairing_begin", {
      relayUrl: "wss://relay.example.com",
    });
    expect(screen.getByText("5 分钟内有效")).toBeInTheDocument();
    expect(screen.getByText("等待手机扫码…")).toBeInTheDocument();
  });

  it("P0-a1：出码用 URL 形态且把钉死的 ECC/margin/width 选项传给 QRCode 库", async () => {
    const toStringSpy = vi.spyOn(QRCode, "toString");
    render(<SettingsRemoteControl />);

    const generateButton = screen.getByRole("button", {
      name: "生成配对二维码",
    });
    await waitFor(() => expect(generateButton).toBeEnabled());
    fireEvent.click(generateButton);

    await waitFor(() => expect(toStringSpy).toHaveBeenCalled());
    const [calledUrl, calledOptions] = toStringSpy.mock.calls[0]!;
    expect(calledUrl).toMatch(
      /^https:\/\/relay\.example\.com\/#p=[A-Za-z0-9_-]+$/,
    );
    // qrcode 库的 Utils.getOptions 会原地 mutate 传入的 options 对象（补 `color: {}` 等内部默认
    // 值），所以这里只用 toMatchObject 断言我们钉死传入的字段，别断言整个对象相等。
    expect(calledOptions).toMatchObject({
      type: "svg",
      errorCorrectionLevel: "M",
      margin: 4,
      width: 224,
    });
    // 真机 bug 回归断言：spy 没打 mockImplementation，走的是真库真跑——直接查它的（异步）
    // 返回值里有没有显式 width/height 属性。没有 width 选项时 qrcode 库只输出 viewBox，
    // 这个 svg 在 WKWebView 的 flex 容器里会塌成 0×0；这里钉死输出里必须有这两个属性，
    // 才是真正防「svg 无显式尺寸」这个真机 bug 回归的断言（不是只查传参）。
    const markup = await toStringSpy.mock.results[0]!.value;
    expect(markup).toContain('width="224"');
    expect(markup).toContain('height="224"');
  });

  it("生成配对二维码后可复制裸 JSON 配对串兜底", async () => {
    Object.defineProperty(navigator, "clipboard", {
      value: { writeText: vi.fn().mockResolvedValue(undefined) },
      writable: true,
      configurable: true,
    });

    render(<SettingsRemoteControl />);
    const generateButton = screen.getByRole("button", {
      name: "生成配对二维码",
    });
    await waitFor(() => expect(generateButton).toBeEnabled());
    fireEvent.click(generateButton);
    await screen.findByText("5 分钟内有效");

    fireEvent.click(screen.getByRole("button", { name: "复制配对串" }));

    await waitFor(() => {
      expect(navigator.clipboard.writeText).toHaveBeenCalledWith(
        JSON.stringify({
          v: 1,
          relay_url: "wss://relay.example.com",
          room: "room-1",
          pairing_token: "token-1",
          desktop_pub: "desktop-pub-1",
        }),
      );
    });
    expect(await screen.findByText("已复制")).toBeInTheDocument();
  });

  it("配对信息编码后超过 1024 字节时展示配对错误且不出二维码", async () => {
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "remote_control_get_settings") {
        return {
          enabled: true,
          relay_url: "wss://relay.example.com",
          active_repo_id: "repo-1",
        };
      }
      if (cmd === "list_repos") return [repoOne, repoTwo];
      if (cmd === "remote_devices_list") return [activeDevice];
      if (cmd === "remote_gateway_status") return gatewayStatus;
      if (cmd === "remote_pairing_begin") {
        return {
          v: 1,
          relay_url: "wss://relay.example.com",
          room: "room-1",
          pairing_token: "t".repeat(2000),
          desktop_pub: "desktop-pub-1",
        };
      }
      return undefined;
    });

    const { container } = render(<SettingsRemoteControl />);
    const generateButton = screen.getByRole("button", {
      name: "生成配对二维码",
    });
    await waitFor(() => expect(generateButton).toBeEnabled());
    fireEvent.click(generateButton);

    expect(
      await screen.findByText(
        "生成配对二维码失败：配对信息过长或 Relay 地址不合法，请检查设置后重试",
      ),
    ).toBeInTheDocument();
    expect(container.querySelector("svg")).toBeNull();
    // M24DF 项 5：出码失败前 remote_pairing_begin 已经把后端配对槽落到 Waiting、token 已注册
    // ——出码失败分支必须 best-effort 取消这个悬空槽位，不能让它靠 5 分钟自然过期。
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("remote_pairing_cancel"),
    );
  });

  it("M24DF 项 1：QR origin 跟随后端回抄的 payload.relay_url，不跟随桌面本地保存的 relayUrl", async () => {
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "remote_control_get_settings") {
        return {
          enabled: true,
          relay_url: "wss://relay.example.com",
          active_repo_id: "repo-1",
        };
      }
      if (cmd === "list_repos") return [repoOne, repoTwo];
      if (cmd === "remote_devices_list") return [activeDevice];
      if (cmd === "remote_gateway_status") return gatewayStatus;
      if (cmd === "remote_pairing_begin") {
        // 后端回抄的 relay_url 与桌面本地保存的 "wss://relay.example.com" 不同——今天两者
        // 恰好同源只是构造巧合，这里故意构造不同值来钉死"origin 只认 payload"这条契约。
        return {
          v: 1,
          relay_url: "wss://relay-echo.example.com",
          room: "room-1",
          pairing_token: "token-1",
          desktop_pub: "desktop-pub-1",
        };
      }
      return undefined;
    });
    const toStringSpy = vi.spyOn(QRCode, "toString");

    render(<SettingsRemoteControl />);
    const generateButton = screen.getByRole("button", {
      name: "生成配对二维码",
    });
    await waitFor(() => expect(generateButton).toBeEnabled());
    fireEvent.click(generateButton);

    await waitFor(() => expect(toStringSpy).toHaveBeenCalled());
    const [calledUrl] = toStringSpy.mock.calls[0]!;
    expect(calledUrl).toMatch(/^https:\/\/relay-echo\.example\.com\/#p=/);
  });

  it("beginPairing 内部对 `!activeRepoId` 的守卫独立生效（绕过按钮 disabled 属性直接验证逻辑层，不是靠 UI 视觉态）", async () => {
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "remote_control_get_settings") {
        return {
          enabled: true,
          relay_url: "wss://relay.example.com",
          active_repo_id: null,
        };
      }
      if (cmd === "list_repos") return [repoOne, repoTwo];
      if (cmd === "remote_devices_list") return [activeDevice];
      if (cmd === "remote_gateway_status") return gatewayStatus;
      return undefined;
    });

    render(<SettingsRemoteControl />);
    const generateButton = await screen.findByRole("button", {
      name: "生成配对二维码",
    });
    await waitFor(() => expect(generateButton).toBeDisabled());

    // 变异自证锚点：删掉 beginPairing() 内部 `!activeRepoId` 早退守卫应该让本测试变红。
    // 直接在真实 DOM 上清掉 disabled 属性再触发原生 click——测的是函数内部的守卫本身，
    // 不是按钮的 disabled 视觉态（后者删掉守卫也不会变红，因为按钮已经被禁用挡住了点击）。
    (generateButton as HTMLButtonElement).disabled = false;
    generateButton.removeAttribute("disabled");
    fireEvent.click(generateButton);

    await new Promise((resolve) => setTimeout(resolve, 0));
    expect(invokeMock).not.toHaveBeenCalledWith(
      "remote_pairing_begin",
      expect.anything(),
    );
  });

  it("select 选「未设置」时以 repoId: null 调用 remote_set_active_project", async () => {
    invokeMock.mockImplementation(async (cmd: string, args?: any) => {
      if (cmd === "remote_control_get_settings") {
        return {
          enabled: true,
          relay_url: "wss://relay.example.com",
          active_repo_id: "repo-1",
        };
      }
      if (cmd === "list_repos") return [repoOne, repoTwo];
      if (cmd === "remote_devices_list") return [activeDevice];
      if (cmd === "remote_gateway_status") return gatewayStatus;
      if (cmd === "remote_set_active_project") {
        expect(args?.repoId ?? null).toBeNull();
        return undefined;
      }
      return undefined;
    });

    render(<SettingsRemoteControl />);
    const select = await screen.findByLabelText("活跃项目");
    await waitFor(() => expect(select).toHaveValue("repo-1"));

    fireEvent.change(select, { target: { value: "" } });

    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("remote_set_active_project", {
        repoId: null,
      }),
    );
  });

  it("list_repos 失败时 UI 不炸且错误可见", async () => {
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "remote_control_get_settings") {
        return {
          enabled: true,
          relay_url: "wss://relay.example.com",
          active_repo_id: "repo-1",
        };
      }
      if (cmd === "list_repos") throw new Error("list_repos failed");
      if (cmd === "remote_devices_list") return [activeDevice];
      if (cmd === "remote_gateway_status") return gatewayStatus;
      return undefined;
    });

    render(<SettingsRemoteControl />);

    expect(
      await screen.findByText("Error: list_repos failed"),
    ).toBeInTheDocument();
    // UI 未崩溃——设置区其余部分仍照常渲染。
    expect(await screen.findByLabelText("活跃项目")).toBeInTheDocument();
    expect(
      screen.getByRole("switch", { name: "允许手机远程控制" }),
    ).toBeInTheDocument();
  });

  it("Done 继续轮询到 Idle 且设备列表只在进入 Done 时刷新一次", async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    const statuses = [
      { state: "Done", device_id: "phone-2" },
      { state: "Done", device_id: "phone-2" },
      { state: "Idle" },
    ];
    let pairingStatusCalls = 0;
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "remote_control_get_settings") {
        return {
          enabled: true,
          relay_url: "wss://relay.example.com",
          active_repo_id: "repo-1",
        };
      }
      if (cmd === "list_repos") return [repoOne, repoTwo];
      if (cmd === "remote_devices_list") {
        deviceListCalls += 1;
        return [activeDevice];
      }
      if (cmd === "remote_pairing_begin") {
        return {
          v: 1,
          relay_url: "wss://relay.example.com",
          room: "room-1",
          pairing_token: "token-1",
          desktop_pub: "desktop-pub-1",
        };
      }
      if (cmd === "remote_pairing_status") {
        const status =
          statuses[pairingStatusCalls] ?? statuses[statuses.length - 1];
        pairingStatusCalls += 1;
        return status;
      }
      if (cmd === "remote_gateway_status") return gatewayStatus;
      return undefined;
    });

    render(<SettingsRemoteControl />);
    const generateButton = screen.getByRole("button", {
      name: "生成配对二维码",
    });
    await waitFor(() => expect(generateButton).toBeEnabled());
    fireEvent.click(generateButton);

    expect(await screen.findByText("已配对设备 phone-2")).toBeInTheDocument();
    await waitFor(() => expect(deviceListCalls).toBe(2));
    expect(pairingStatusCalls).toBe(1);

    await act(async () => vi.advanceTimersByTimeAsync(3000));
    await waitFor(() => expect(pairingStatusCalls).toBe(2));
    expect(screen.getByText("已配对设备 phone-2")).toBeInTheDocument();
    expect(deviceListCalls).toBe(2);

    await act(async () => vi.advanceTimersByTimeAsync(3000));
    await waitFor(() => expect(pairingStatusCalls).toBe(3));
    expect(screen.queryByText("已配对设备 phone-2")).not.toBeInTheDocument();
    expect(deviceListCalls).toBe(2);

    await act(async () => vi.advanceTimersByTimeAsync(3000));
    expect(pairingStatusCalls).toBe(3);
  });

  it("配对状态查询错误后停止配对轮询", async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    let pairingStatusCalls = 0;
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "remote_control_get_settings") {
        return {
          enabled: true,
          relay_url: "wss://relay.example.com",
          active_repo_id: "repo-1",
        };
      }
      if (cmd === "list_repos") return [repoOne, repoTwo];
      if (cmd === "remote_devices_list") return [activeDevice];
      if (cmd === "remote_pairing_begin") {
        return {
          v: 1,
          relay_url: "wss://relay.example.com",
          room: "room-1",
          pairing_token: "token-1",
          desktop_pub: "desktop-pub-1",
        };
      }
      if (cmd === "remote_pairing_status") {
        pairingStatusCalls += 1;
        throw new Error("pairing status failed");
      }
      if (cmd === "remote_gateway_status") return gatewayStatus;
      return undefined;
    });

    render(<SettingsRemoteControl />);
    const generateButton = screen.getByRole("button", {
      name: "生成配对二维码",
    });
    await waitFor(() => expect(generateButton).toBeEnabled());
    fireEvent.click(generateButton);

    expect(
      await screen.findByText("Error: pairing status failed"),
    ).toBeInTheDocument();
    const callsAfterError = pairingStatusCalls;
    expect(callsAfterError).toBeGreaterThan(0);

    await act(async () => vi.advanceTimersByTimeAsync(6000));
    expect(pairingStatusCalls).toBe(callsAfterError);
  });

  it("渲染设备列表并在确认后吊销设备", async () => {
    render(<SettingsRemoteControl />);

    expect(await screen.findByText("Alice 的手机")).toBeInTheDocument();
    expect(
      screen.getByText(
        `访问权限到期：${new Date(activeDevice.access_expires_at).toLocaleString("zh-CN")}`,
      ),
    ).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "吊销" }));

    const dialog = screen.getByRole("dialog", {
      name: "吊销设备访问权限？",
    });
    expect(dialog).toHaveTextContent(
      "吊销「Alice 的手机」后，这台设备将无法继续远程控制 AgentLoom。",
    );
    expect(dialog).toHaveTextContent(
      "解除后该手机会立即断开，需要重新扫码才能再次连接。",
    );
    fireEvent.click(screen.getByRole("button", { name: "确认吊销" }));

    await waitFor(() => {
      expect(invokeMock).toHaveBeenCalledWith("remote_device_revoke", {
        deviceId: "phone-1",
      });
      expect(deviceListCalls).toBe(2);
    });
  });
});

// P0-a1：QR 出码纯函数——独立于组件渲染直接测，覆盖 spec 钉死的形状/派生/校验/上限规则。
describe("buildPairingQrUrl", () => {
  const payload: RemotePairingPayload = {
    v: 1,
    relay_url: "wss://relay.example.com",
    room: "room-1",
    pairing_token: "token-1",
    desktop_pub: "desktop-pub-1",
  };

  function decodeBase64Url(value: string): string {
    const padded = value.replace(/-/g, "+").replace(/_/g, "/");
    const padLength = (4 - (padded.length % 4)) % 4;
    const base64 = padded + "=".repeat(padLength);
    const binary = atob(base64);
    const bytes = Uint8Array.from(binary, (char) => char.charCodeAt(0));
    return new TextDecoder().decode(bytes);
  }

  it("URL 形状正确且 base64url 可逆解回原 payload", () => {
    const url = buildPairingQrUrl(payload);
    expect(url).not.toBeNull();

    const match = url?.match(
      /^https:\/\/relay\.example\.com\/#p=([A-Za-z0-9_-]+)$/,
    );
    expect(match).not.toBeNull();
    const decoded: unknown = JSON.parse(decodeBase64Url(match![1]!));
    expect(decoded).toEqual(payload);
  });

  it("wss host（含端口）派生为对应的 https origin——变异自证锚点：把这条 origin 派生拼错会让本条断言变红", () => {
    const relayUrl = "wss://relay.example.com:8443";
    const portedPayload: RemotePairingPayload = {
      ...payload,
      relay_url: relayUrl,
    };

    const url = buildPairingQrUrl(portedPayload);

    expect(url).not.toBeNull();
    expect(url?.startsWith("https://relay.example.com:8443/#p=")).toBe(true);
  });

  it.each([
    ["http:// 前缀", "http://relay.example.com"],
    ["带非根 path", "wss://relay.example.com/room"],
    ["带 query", "wss://relay.example.com?x=1"],
    ["带 userinfo", "wss://user:pass@relay.example.com"],
    ["无法 parse", "not a url"],
    // M24DF 项 8b：canonical 等值检查关死的 WHATWG 归一化尾巴——这几种输入的逐属性检查
    // 单独看都"合法"（username/search/hash 解析后都是空字符串），必须靠 canonical href
    // 比对才能拦住。
    ["显式 hash", "wss://relay.example.com#x"],
    ["空 hash（归一化会悄悄吃掉 #）", "wss://relay.example.com#"],
    ["空 userinfo（归一化会悄悄吃掉 @）", "wss://@relay.example.com"],
    ["空 query（归一化会悄悄吃掉 ?）", "wss://relay.example.com?"],
    // M24DF 微返工第 3 轮：上面两条空 ?/# 用例都没带斜杠，WHATWG 会补一个 `/` 导致 href
    // 与原始输入不等、被 canonical 检查拦住——但斜杠已经在原始输入里时，href 逐字符相等，
    // canonical 检查测不出来，这两条才是真正的绕过口子。
    ["斜杠后空 query（canonical 等值检查测不出）", "wss://relay.example.com/?"],
    ["斜杠后空 hash（canonical 等值检查测不出）", "wss://relay.example.com/#"],
  ])("payload.relay_url 形态非法（%s）时拒绝出码", (_label, badRelayUrl) => {
    expect(
      buildPairingQrUrl({ ...payload, relay_url: badRelayUrl }),
    ).toBeNull();
  });

  it("总 URL 长度超过 1024 字节时拒绝出码（不出降级码）", () => {
    const oversizedPayload: RemotePairingPayload = {
      ...payload,
      pairing_token: "t".repeat(2000),
    };

    expect(buildPairingQrUrl(oversizedPayload)).toBeNull();
  });

  it("M24DF 项 8a：总 URL 长度落在 1025~2048 字节之间时仍拒绝出码（变异自证锚点：把上限从 1024 误改成 2048 会让本条断言变红，上面那条 2843 字节的用例不会——它超过 2048 依然被拒）", () => {
    const midOversizedPayload: RemotePairingPayload = {
      ...payload,
      pairing_token: "t".repeat(700),
    };
    const encoded = base64UrlEncode(JSON.stringify(midOversizedPayload));
    const byteLength = new TextEncoder().encode(
      `https://relay.example.com/#p=${encoded}`,
    ).length;
    expect(byteLength).toBeGreaterThan(1024);
    expect(byteLength).toBeLessThanOrEqual(2048);

    expect(buildPairingQrUrl(midOversizedPayload)).toBeNull();
  });

  it("M24DF 项 8a：总 URL 长度恰好等于 1024 字节上限时通过出码", () => {
    const boundaryPayload: RemotePairingPayload = {
      ...payload,
      pairing_token: "t".repeat(636),
    };
    const url = buildPairingQrUrl(boundaryPayload);
    expect(url).not.toBeNull();
    expect(new TextEncoder().encode(url!).length).toBe(1024);
  });

  it("base64UrlEncode 输出不含 +、/、= 字符", () => {
    const encoded = base64UrlEncode(JSON.stringify(payload));
    expect(encoded).not.toMatch(/[+/=]/);
  });

  it("base64UrlEncode 对含中文的 payload 仍可逆（不假设全 ASCII）", () => {
    const zhPayload: RemotePairingPayload = {
      ...payload,
      room: "房间-1",
    };
    const encoded = base64UrlEncode(JSON.stringify(zhPayload));
    expect(JSON.parse(decodeBase64Url(encoded))).toEqual(zhPayload);
  });
});

// Composer.test.tsx — TDD 覆盖 src/ui/composer/Composer.tsx（T6f3 · 发送/答卡的发送框半边；U4
// 差量：Stop 按钮 + 二次确认 + stopBadge 已迁到 `ui/stream/SessionStreamScreen.tsx` 的 header，
// 相关用例一并迁到 `SessionStreamScreen.test.tsx`，本文件不再覆盖它们。）

import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import { Composer, type ComposerSendBadge } from "./Composer.tsx";

afterEach(() => {
  cleanup();
  vi.useRealTimers();
});

describe("Composer: 输入框 + 发送按钮", () => {
  it("空输入时发送按钮禁用；输入内容后可点，点击后调用 onSend 并清空输入框", async () => {
    const user = userEvent.setup();
    const onSend = vi.fn();
    render(<Composer locale="zh" onSend={onSend} />);

    const sendButton = screen.getByTestId("composer-send") as HTMLButtonElement;
    expect(sendButton.disabled).toBe(true);

    const input = screen.getByTestId("composer-input") as HTMLTextAreaElement;
    await user.type(input, "hello desktop");
    expect(sendButton.disabled).toBe(false);

    await user.click(sendButton);
    expect(onSend).toHaveBeenCalledWith("hello desktop");
    expect(input.value).toBe("");
  });

  it("纯空白输入不触发 onSend（trim 后为空）", async () => {
    const user = userEvent.setup();
    const onSend = vi.fn();
    render(<Composer locale="zh" onSend={onSend} />);
    const input = screen.getByTestId("composer-input") as HTMLTextAreaElement;
    await user.type(input, "   ");
    const sendButton = screen.getByTestId("composer-send") as HTMLButtonElement;
    expect(sendButton.disabled).toBe(true);
  });

  it("Enter（无 shift）提交；Shift+Enter 不提交", () => {
    const onSend = vi.fn();
    render(<Composer locale="zh" onSend={onSend} />);
    const input = screen.getByTestId("composer-input") as HTMLTextAreaElement;

    fireEvent.change(input, { target: { value: "line one" } });
    fireEvent.keyDown(input, { key: "Enter", shiftKey: true });
    expect(onSend).not.toHaveBeenCalled();

    fireEvent.keyDown(input, { key: "Enter", shiftKey: false });
    expect(onSend).toHaveBeenCalledWith("line one");
  });

  it("U4 图标化：发送按钮不再显字，靠 aria-label（既有 composer.send i18n key）保留可及性", () => {
    render(<Composer locale="zh" onSend={() => {}} />);
    const sendButton = screen.getByTestId("composer-send") as HTMLButtonElement;
    expect(sendButton.getAttribute("aria-label")).toBe("发送");
    expect(sendButton.textContent?.trim()).toBe("");
    expect(sendButton.querySelector("svg")).toBeTruthy();
  });
});

describe("Composer: sendBadge——发送中/排队/失败/过期/放弃的展示与重试", () => {
  it("status=sending：显示发送中；30 秒（阈值）内不升级文案", () => {
    const now = () => 1_000_000;
    const badge: ComposerSendBadge = { commandId: "c1", status: "sending", sentAtMs: now() - 10_000 };
    render(<Composer locale="zh" onSend={() => {}} sendBadge={badge} now={now} />);
    expect(screen.getByTestId("composer-send-badge").textContent).toContain("发送中");
  });

  it("status=sending：超过 30 秒未 ack，文案升级为「投递中，桌面可能离线」（一次性定时器精确触发）", () => {
    vi.useFakeTimers();
    const base = 1_000_000;
    let nowValue = base;
    const now = () => nowValue;
    const badge: ComposerSendBadge = { commandId: "c1", status: "sending", sentAtMs: base };
    render(<Composer locale="zh" onSend={() => {}} sendBadge={badge} now={now} />);
    expect(screen.getByTestId("composer-send-badge").textContent).toContain("发送中");

    nowValue = base + 30_000;
    act(() => {
      vi.advanceTimersByTime(30_000);
    });
    expect(screen.getByTestId("composer-send-badge").textContent).toContain("投递中");
  });

  it("status=queued：弱担保措辞「桌面已接收，正在排队」，不出现「已执行」这类强断言字样", () => {
    const badge: ComposerSendBadge = { commandId: "c1", status: "queued", sentAtMs: 0 };
    render(<Composer locale="zh" onSend={() => {}} sendBadge={badge} />);
    const text = screen.getByTestId("composer-send-badge").textContent ?? "";
    expect(text).toContain("已接收");
    expect(text).not.toContain("已执行");
  });

  it("status=relay_queued：弱担保措辞「已排队，等电脑回来」，不出现重试按钮（消息没丢）", () => {
    const badge: ComposerSendBadge = { commandId: "c1", status: "relay_queued", sentAtMs: 0 };
    render(<Composer locale="zh" onSend={() => {}} sendBadge={badge} />);
    const el = screen.getByTestId("composer-send-badge");
    expect(el.textContent).toContain("已排队");
    expect(el.getAttribute("data-status")).toBe("relay_queued");
    expect(screen.queryByTestId("composer-send-retry")).toBeNull();
  });

  it("status=delivering_uncertain：显示投递结果未知 + 重试按钮，点击调用 onRetry", async () => {
    const user = userEvent.setup();
    const onRetry = vi.fn();
    const badge: ComposerSendBadge = { commandId: "c1", status: "delivering_uncertain", sentAtMs: 0, onRetry };
    render(<Composer locale="zh" onSend={() => {}} sendBadge={badge} />);
    expect(screen.getByTestId("composer-send-badge").textContent).toContain("投递结果未知");
    await user.click(screen.getByTestId("composer-send-retry"));
    expect(onRetry).toHaveBeenCalledTimes(1);
  });

  it("status=failed：显示失败 + 重试按钮，点击调用 onRetry", async () => {
    const user = userEvent.setup();
    const onRetry = vi.fn();
    const badge: ComposerSendBadge = { commandId: "c1", status: "failed", sentAtMs: 0, onRetry };
    render(<Composer locale="zh" onSend={() => {}} sendBadge={badge} />);
    expect(screen.getByTestId("composer-send-badge").getAttribute("data-status")).toBe("failed");
    await user.click(screen.getByTestId("composer-send-retry"));
    expect(onRetry).toHaveBeenCalledTimes(1);
  });

  it("status=failed 且 reason=no_agent：显示未选 agent 的专用提示", () => {
    const badge: ComposerSendBadge = {
      commandId: "c1",
      status: "failed",
      sentAtMs: 0,
      onRetry: () => {},
      reason: "no_agent",
    };
    render(<Composer locale="zh" onSend={() => {}} sendBadge={badge} />);
    expect(screen.getByTestId("composer-send-badge").textContent).toContain(
      "这个会话还没选 agent，请先在桌面上打开它选一个",
    );
  });

  it("status=failed 且无 reason：仍显示通用发送失败", () => {
    const badge: ComposerSendBadge = { commandId: "c1", status: "failed", sentAtMs: 0 };
    render(<Composer locale="zh" onSend={() => {}} sendBadge={badge} />);
    expect(screen.getByTestId("composer-send-badge").textContent).toContain("发送失败");
  });

  it("status=failed 且 reason 未知：仍显示通用发送失败", () => {
    const badge: ComposerSendBadge = {
      commandId: "c1",
      status: "failed",
      sentAtMs: 0,
      reason: "some_other_reason",
    };
    render(<Composer locale="zh" onSend={() => {}} sendBadge={badge} />);
    expect(screen.getByTestId("composer-send-badge").textContent).toContain("发送失败");
  });

  it("status=expired：显示过期提示 + 重试按钮", () => {
    const badge: ComposerSendBadge = { commandId: "c1", status: "expired", sentAtMs: 0, onRetry: () => {} };
    render(<Composer locale="zh" onSend={() => {}} sendBadge={badge} />);
    expect(screen.getByTestId("composer-send-badge").textContent).toContain("已过期");
  });

  it("status=give_up：显示连接不稳定提示 + 重试按钮", () => {
    const badge: ComposerSendBadge = { commandId: "c1", status: "give_up", sentAtMs: 0, onRetry: () => {} };
    render(<Composer locale="zh" onSend={() => {}} sendBadge={badge} />);
    expect(screen.getByTestId("composer-send-badge").textContent).toContain("连接不稳定");
  });

  it("sendBadge 未传（或 null）——不渲染徽标", () => {
    render(<Composer locale="zh" onSend={() => {}} sendBadge={null} />);
    expect(screen.queryByTestId("composer-send-badge")).toBeNull();
  });
});

describe("Composer: 输入框自动长高（搬桌面 InputArea.tsx autosize 手法）——jsdom scrollHeight 恒 0，必须桩", () => {
  function stubScrollHeight(el: HTMLTextAreaElement, value: number): void {
    Object.defineProperty(el, "scrollHeight", { get: () => value, configurable: true });
  }

  it("多行文本、scrollHeight=80（未超上限）：拉高到 80px 且不显示滚动条", () => {
    render(<Composer locale="zh" onSend={() => {}} />);
    const input = screen.getByTestId("composer-input") as HTMLTextAreaElement;
    stubScrollHeight(input, 80);

    fireEvent.change(input, { target: { value: "line one\nline two\nline three" } });

    expect(input.style.height).toBe("80px");
    expect(input.style.overflowY).toBe("hidden");
  });

  it("scrollHeight=300（超上限 120）：高度钳到 120px 且出现滚动条", () => {
    render(<Composer locale="zh" onSend={() => {}} />);
    const input = screen.getByTestId("composer-input") as HTMLTextAreaElement;
    stubScrollHeight(input, 300);

    fireEvent.change(input, { target: { value: "very long text ".repeat(50) } });

    expect(input.style.height).toBe("120px");
    expect(input.style.overflowY).toBe("auto");
  });

  it("发送成功后：清空文本触发重新测量（按新内容——空文本——的 scrollHeight 收窄，滚动条隐藏）", async () => {
    // msgfix2 U6a：测量搬进 `useLayoutEffect`（依赖 `text`）后，提交清空文本本身就是一次
    // `text` 变化，会照常重新 autosize——不再是提交路径手写死值 "auto" 的旧写法（那手写值
    // 必被随后触发的 effect 覆盖，是竞态死代码，已删）。这里用「随 el.value 变化」的桩模拟
    // 真实浏览器行为：清空后 scrollHeight 应显著变小。
    const user = userEvent.setup();
    const onSend = vi.fn();
    render(<Composer locale="zh" onSend={onSend} />);
    const input = screen.getByTestId("composer-input") as HTMLTextAreaElement;
    Object.defineProperty(input, "scrollHeight", {
      configurable: true,
      get: () => (input.value.length === 0 ? 24 : 80),
    });
    fireEvent.change(input, { target: { value: "hello" } });
    expect(input.style.height).toBe("80px");

    const sendButton = screen.getByTestId("composer-send") as HTMLButtonElement;
    await user.click(sendButton);

    expect(onSend).toHaveBeenCalledWith("hello");
    expect(input.value).toBe("");
    expect(input.style.height).toBe("24px");
    expect(input.style.overflowY).toBe("hidden");
  });

  it("scrollHeight 只读一次（消除第二次强制布局——msgfix2 U6a 核心修复的直接回归测试）", () => {
    render(<Composer locale="zh" onSend={() => {}} />);
    const input = screen.getByTestId("composer-input") as HTMLTextAreaElement;
    let reads = 0;
    Object.defineProperty(input, "scrollHeight", {
      configurable: true,
      get() {
        reads += 1;
        return 80;
      },
    });

    fireEvent.change(input, { target: { value: "line one\nline two" } });

    expect(reads).toBe(1);
  });

  it("超长文本（>AUTOSIZE_MAX_CHARS=20000 字符）：不测量、直接锁最大高度 120px + 内部滚动（搬桌面 InputArea.tsx 守卫）", () => {
    render(<Composer locale="zh" onSend={() => {}} />);
    const input = screen.getByTestId("composer-input") as HTMLTextAreaElement;
    let reads = 0;
    Object.defineProperty(input, "scrollHeight", {
      configurable: true,
      get() {
        reads += 1;
        return 9999; // 若守卫失效、误测量，会被钳到 120px——用 reads 计数确认真的没读它。
      },
    });

    fireEvent.change(input, { target: { value: "a".repeat(20_001) } });

    expect(input.style.height).toBe("120px");
    expect(input.style.overflowY).toBe("auto");
    expect(reads).toBe(0);
  });
});

describe("Composer: IME 组词期间 Enter 不触发发送（msgfix2 U6c·搬桌面 composingRef 守卫）", () => {
  it("compositionstart 之后按 Enter 不发送；compositionend 之后 Enter 正常发送", () => {
    const onSend = vi.fn();
    render(<Composer locale="zh" onSend={onSend} />);
    const input = screen.getByTestId("composer-input") as HTMLTextAreaElement;

    fireEvent.change(input, { target: { value: "你好" } });
    fireEvent.compositionStart(input);
    fireEvent.keyDown(input, { key: "Enter", shiftKey: false });
    expect(onSend).not.toHaveBeenCalled();

    fireEvent.compositionEnd(input);
    fireEvent.keyDown(input, { key: "Enter", shiftKey: false });
    expect(onSend).toHaveBeenCalledWith("你好");
  });

  it("keyCode===229（IME 兼容兜底，覆盖不派发 compositionstart 的浏览器实现）时 Enter 不发送", () => {
    const onSend = vi.fn();
    render(<Composer locale="zh" onSend={onSend} />);
    const input = screen.getByTestId("composer-input") as HTMLTextAreaElement;
    fireEvent.change(input, { target: { value: "test" } });

    fireEvent.keyDown(input, { key: "Enter", shiftKey: false, keyCode: 229 });
    expect(onSend).not.toHaveBeenCalled();
  });

  it("nativeEvent.isComposing===true 时 Enter 不发送", () => {
    const onSend = vi.fn();
    render(<Composer locale="zh" onSend={onSend} />);
    const input = screen.getByTestId("composer-input") as HTMLTextAreaElement;
    fireEvent.change(input, { target: { value: "test" } });

    fireEvent.keyDown(input, { key: "Enter", shiftKey: false, isComposing: true });
    expect(onSend).not.toHaveBeenCalled();
  });
});

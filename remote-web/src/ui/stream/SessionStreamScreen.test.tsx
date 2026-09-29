// SessionStreamScreen.test.tsx — TDD 覆盖 SessionStreamScreen.tsx，走真实 data-plane-v1.json
// 样张的完整流水线：parseFrame → MilestoneProjection/LiveBlockReducer（T6d1 内核）→
// deriveSessionStreamProps → SessionStreamScreen 渲染，断言 DOM 锚点（任务书 §3 acceptance）。
//
// 落在 `src/ui/stream/`（`.test.tsx`）——vitest.config.ts 的 "ui" project 覆盖
// `src/ui/**/*.test.tsx`，跑 jsdom 环境；`streamSource.test.ts`（同目录、`.test.ts` 后缀）落
// "logic" project 的 node 环境，两者分工不重叠。
//
// ============================================================================
// 覆盖表
// ============================================================================
// | 断言目标                              | 数据来源                                          |
// |----------------------------------------|---------------------------------------------------|
// | msg.completed 的 text/tool 块经桌面叶子 | data-plane-v1.json: msg_completed（真样张）        |
// | 组件渲染（MessageContent→MarkdownBody/  |                                                     |
// | ToolCard）                              |                                                     |
// | thinking 块折叠默认（ThinkingBlock 复用）| data-plane-v1.json: live_thinking_delta（真样张，   |
// |                                          | 归约进 LiveBlockReducer）                          |
// | live 打字态 + 状态条 running             | data-plane-v1.json: run_status_running +           |
// |                                          | snapshot_response_running_with_partial +            |
// |                                          | live_text_delta（真样张）                          |
// | 空态 / idle 状态条                       | 协议自造语料（同 parseFrame.test.ts 先例：fixture   |
// |                                          | 没有"零消息"这种样张，语义上也不需要）              |
// | user 行显示                             | 协议自造语料（fixture 的 msg_completed 唯一样张是   |
// |                                          | assistant；role="user" 走同一条 parseFrame 校验，   |
// |                                          | 不是 fork 出一条新解析路径）                        |
// | AttachmentPort null 降级显示路径         | 协议自造语料（markdown 内嵌本地图片路径）           |
// ============================================================================
//
// 变异自证（worker 报告 ⑤，方法论同 parseFrame.test.ts；差量返工新增 2 条，共 4 条）：
//   1. `SessionStreamScreen.tsx::SessionStreamContent` 里把 `liveBlocks !== null` 的判断改成
//      恒 `false`——"live 打字态"整组测试转红（live message row/typing indicator 断言全部找不到
//      对应 DOM）。
//   2. `MessageRow` 的 `isUser` 判断改成恒 `false`——"user 行显示"测试转红（`.stream-msg--user`
//      class 断言失败，user 消息的头像位置也会跟 assistant 一样）。
//   3.（差量返工）`hasRestrictedBlock` 改成恒 `false`——"Approval/ScopeChange 只读呈现"整组测试
//      转红（`stream-restricted-content`/`stream-restricted-hint` 断言全部找不到对应 DOM）。
//   4.（差量返工）`SessionStreamContent` 改成不读 `useAppI18n().locale`、直接固定传 `"en"` 给
//      `useI18n(...)`——"locale 跟随桌面 Provider 切换"测试转红（切换后壳层文案仍是英文）。
//   四处改完各自跑 `npx vitest run src/ui/stream/SessionStreamScreen.test.tsx` 确认转红，再改回来
//   复跑转绿；过程与结果见 worker 报告，代码已还原，不作为提交内容。

import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { useEffect } from "react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { I18nProvider, useI18n as useAppI18n } from "@app/i18n";
import { loadFixture } from "../../test-support/fixtures.ts";
import {
  parseFrame,
  type CardCreatedFrame,
  type CardResolvedFrame,
  type MsgCompletedFrame,
  type RunStatusFrame,
  type SnapshotResponseFrame,
} from "../../events/parseFrame.ts";
import { MilestoneProjection } from "../../events/milestoneProjection.ts";
import { LiveBlockReducer } from "../../events/liveReducer.ts";
import type { ReducedBlock } from "../../events/blocks.ts";
import { deriveSessionStreamProps, type SessionStreamMessage } from "./streamSource.ts";
import { SessionStreamScreen, SessionStreamContent, type StreamStopBadge } from "./SessionStreamScreen.tsx";
import type { MsgFetchState } from "../../events/msgFetch.ts";

afterEach(() => {
  cleanup();
  // U4 新增的 stopBadge 150s 阈值用例用了 vi.useFakeTimers()——不还原会让假定时器泄漏到后面的
  // describe 块（userEvent.setup() 默认吃真实定时器，泄漏后续测试全体超时）。同 Composer.test.tsx
  // 既有 afterEach 的取向。
  vi.useRealTimers();
});

interface DataPlaneCase {
  name: string;
  desc: string;
  frame: unknown;
  valid: boolean;
  consumers: string[];
}
interface DataPlaneFixture {
  version: number;
  cases: DataPlaneCase[];
}

function fixtureFrame(name: string): unknown {
  const fixture = loadFixture<DataPlaneFixture>("data-plane-v1.json");
  const found = fixture.cases.find((c) => c.name === name);
  if (!found) throw new Error(`data-plane-v1.json: case "${name}" not found`);
  expect(found.valid).toBe(true);
  return found.frame;
}

describe("SessionStreamScreen: fixture-driven pipeline (parseFrame → MilestoneProjection/LiveBlockReducer → screen)", () => {
  it("renders msg.completed's text + tool blocks via the reused desktop leaves, and shows the running status", async () => {
    const projection = new MilestoneProjection();

    const msgCompletedResult = parseFrame(fixtureFrame("msg_completed"));
    if (!msgCompletedResult.ok || msgCompletedResult.frame.t !== "msg.completed") {
      throw new Error("fixture parse failed: msg_completed");
    }
    projection.applyMsgCompleted(msgCompletedResult.frame as MsgCompletedFrame);

    const runStatusResult = parseFrame(fixtureFrame("run_status_running"));
    if (!runStatusResult.ok || runStatusResult.frame.t !== "run.status") {
      throw new Error("fixture parse failed: run_status_running");
    }
    projection.applyRunStatus(runStatusResult.frame as RunStatusFrame);
    const sessionId = (runStatusResult.frame as RunStatusFrame).session_id;

    const props = deriveSessionStreamProps(projection, sessionId);
    const { container } = render(<SessionStreamScreen {...props} initialLocale="zh" />);

    // 状态条：running=true → "运行中"。
    expect(screen.getByTestId("stream-status-label").textContent).toBe("运行中");

    // MA2：msg_completed 样张带 agent: "Claude"（MA1 后端补的字段）——完成消息头像渲染真实 agent
    // （命中 AgentAvatar 的 claude 配色分支），不是通用 "assistant" 占位。
    expect(container.querySelector(".agent-avatar--claude")).toBeTruthy();
    expect(container.querySelector(".agent-avatar--assistant")).toBeNull();

    // text 块——经 MessageContent → (MarkdownBody 加载完成前的兜底 div，或加载完成后的
    // markdown 渲染) 两条路径都会显示同样的纯文本，findByText 覆盖两种时序。
    await screen.findByText("Fixed the login bug and added a regression test.");

    // tool 块——经 ToolCard（compact 模式，MessageContent 固定传 compact，套在 ToolStepsFold
    // 的 <details> 折叠里，同 folded-by-default DNA）：工具名 "shell"（不在 toolLabel.ts 的映射表
    // 里，原样透传）+ summary "cargo test" + 完成态徽标（两处徽标——折叠头汇总一个、ToolCard 自己
    // 一个，用 getAllByText 而不是 getByText）。
    expect(screen.getByText("shell")).toBeTruthy();
    expect(screen.getByText("cargo test")).toBeTruthy();
    expect(screen.getAllByText("完成").length).toBeGreaterThanOrEqual(1);
  });

  it("live typing: seeds LiveBlockReducer from a snapshot's partial_msg, feeds live deltas, shows the typing indicator + live text + a newly-created thinking block folded by default", async () => {
    const user = userEvent.setup();
    const projection = new MilestoneProjection();

    const runStatusResult = parseFrame(fixtureFrame("run_status_running"));
    if (!runStatusResult.ok || runStatusResult.frame.t !== "run.status") throw new Error("parse failed");
    projection.applyRunStatus(runStatusResult.frame as RunStatusFrame);
    const sessionId = (runStatusResult.frame as RunStatusFrame).session_id;

    const snapshotResult = parseFrame(fixtureFrame("snapshot_response_running_with_partial"));
    if (!snapshotResult.ok || snapshotResult.frame.t !== "snapshot") throw new Error("parse failed");
    const snapshotFrame = snapshotResult.frame as SnapshotResponseFrame;
    const seedBlocks = (snapshotFrame.partial_msg?.blocks ?? []) as ReducedBlock[];
    const liveReducer = new LiveBlockReducer(seedBlocks);

    const textDeltaResult = parseFrame(fixtureFrame("live_text_delta"));
    if (!textDeltaResult.ok || textDeltaResult.frame.t !== "text_delta") throw new Error("parse failed");
    liveReducer.feed(textDeltaResult.frame);

    const thinkingDeltaResult = parseFrame(fixtureFrame("live_thinking_delta"));
    if (!thinkingDeltaResult.ok || thinkingDeltaResult.frame.t !== "thinking_delta") throw new Error("parse failed");
    liveReducer.feed(thinkingDeltaResult.frame);

    const props = deriveSessionStreamProps(projection, sessionId, liveReducer);
    render(<SessionStreamScreen {...props} initialLocale="zh" />);

    // live message row + typing indicator present.
    const liveRow = screen.getByTestId("stream-live-message");
    expect(within(liveRow).getByTestId("stream-typing-indicator").textContent).toBe("正在输入…");

    // partial_msg 的 text 块 "Working on the fix..." 续写 live_text_delta 的 "Hello, "
    // （末块同类续写规则，liveReducer.ts 头注）——拼接结果是同一个文本节点；markdown 渲染按
    // CommonMark 规则裁掉了段落末尾的单个空格（不是 hard-break 的两个空格），所以断言串不带
    // 尾随空格（实测确认：liveReducer 归约出的原始字符串确实带一个尾随空格，是渲染层裁的，不是
    // liveReducer 的 bug）。
    await within(liveRow).findByText("Working on the fix, running tests now...Hello,");

    // thinking_delta 在"末块是 text"时另起一块（不是续写）——ThinkingBlock 折叠默认：正文
    // 不在 DOM 里，只有"展开"切换按钮。
    expect(within(liveRow).queryByText("Let me check the tests...")).toBeNull();
    const thinkingToggle = within(liveRow).getByRole("button", { name: /展开/ });
    await user.click(thinkingToggle);
    expect(within(liveRow).getByText("Let me check the tests...")).toBeTruthy();
  });

  it("idle status + no live message + empty history shows the empty state, not a crash", () => {
    render(
      <SessionStreamScreen
        sessionId={null}
        messages={[]}
        running={false}
        liveBlocks={null}
        decisionCards={[]}
        initialLocale="zh"
      />,
    );
    expect(screen.getByTestId("stream-status-label").textContent).toBe("空闲");
    expect(screen.getByTestId("stream-empty").textContent).toBe("还没有消息。");
    expect(screen.queryByTestId("stream-live-message")).toBeNull();
  });

  it("a user-role msg.completed message renders on the user side (protocol-authored fixture: parseFrame doesn't discriminate by role)", () => {
    const projection = new MilestoneProjection();
    const userFrameResult = parseFrame({
      t: "msg.completed",
      message_id: 1,
      role: "user",
      blocks: [{ type: "text", text: "Please check the deploy logs." }],
    });
    if (!userFrameResult.ok || userFrameResult.frame.t !== "msg.completed") throw new Error("parse failed");
    projection.applyMsgCompleted(userFrameResult.frame as MsgCompletedFrame);

    const props = deriveSessionStreamProps(projection, null);
    const { container } = render(<SessionStreamScreen {...props} initialLocale="zh" />);

    const row = screen.getByTestId("stream-message");
    expect(row.getAttribute("data-role")).toBe("user");
    expect(container.querySelector(".stream-msg--user")).toBe(row);
  });

  it("AttachmentPort's default web implementation degrades a markdown-embedded local image to a path (no <img>)", async () => {
    const projection = new MilestoneProjection();
    const frameResult = parseFrame({
      t: "msg.completed",
      message_id: 1,
      role: "assistant",
      blocks: [{ type: "text", text: "See ![screenshot](./shot.png) for details." }],
    });
    if (!frameResult.ok || frameResult.frame.t !== "msg.completed") throw new Error("parse failed");
    projection.applyMsgCompleted(frameResult.frame as MsgCompletedFrame);

    const props = deriveSessionStreamProps(projection, null);
    // 不传 attachmentPort——用 SessionStreamScreen 默认的 createWebAttachmentPort()
    // （resolveImageSrc 恒 null）。
    render(<SessionStreamScreen {...props} initialLocale="zh" />);

    // MarkdownBody 是懒加载的（useMarkdown()），等它真的挂载、把 markdown img 解析成
    // LocalMarkdownImage → resolveImageSrc(null) → failed → PreviewablePath 之后再断言。
    await waitFor(() => {
      expect(screen.getByText("./shot.png")).toBeTruthy();
    });
    expect(screen.queryByRole("img")).toBeNull();
  });

  it("MA2 向后兼容：msg.completed 无 agent 键时头像回退现有 assistant 占位，不崩（协议自造语料——history/snapshot 帧本就不带 agent）", () => {
    const projection = new MilestoneProjection();
    const frameResult = parseFrame({
      t: "msg.completed",
      message_id: 1,
      role: "assistant",
      blocks: [{ type: "text", text: "No agent field on this frame." }],
    });
    if (!frameResult.ok || frameResult.frame.t !== "msg.completed") throw new Error("parse failed");
    expect((frameResult.frame as MsgCompletedFrame).agent).toBeUndefined();
    projection.applyMsgCompleted(frameResult.frame as MsgCompletedFrame);

    const props = deriveSessionStreamProps(projection, null);
    const { container } = render(<SessionStreamScreen {...props} initialLocale="zh" />);

    expect(container.querySelector(".agent-avatar--assistant")).toBeTruthy();
    expect(screen.queryByText("undefined")).toBeNull();
  });
});

// ============================================================================
// U4（手机端 composer 与壳层布局重排）：Stop 按钮 + 二次确认 + stopBadge 从
// `ui/composer/Composer.tsx` 底部迁到这里的 header 右侧——用例原样从 Composer.test.tsx 搬运
// （交互语义不变：仅 running 时出现 / 二次确认 / running 翻 false 自动收起 / stopBadge 150s 如实
// 措辞过渡），断言目标从 `composer-stop-*` 改成新的 `stream-stop-*` testid。
// ============================================================================
describe("SessionStreamScreen: header Stop 按钮——仅 running 时出现 + 二次确认", () => {
  it("running=false 时不渲染 Stop 按钮", () => {
    render(
      <SessionStreamContent sessionId={null} messages={[]} running={false} liveBlocks={null} decisionCards={[]} />,
    );
    expect(screen.queryByTestId("stream-stop-row")).toBeNull();
  });

  it("running=true：点击 Stop 先进入确认态，不直接调用 onStop；确认后才调用", async () => {
    const user = userEvent.setup();
    const onStop = vi.fn();
    render(
      <SessionStreamContent
        sessionId={null}
        messages={[]}
        running={true}
        liveBlocks={null}
        decisionCards={[]}
        onStop={onStop}
      />,
    );

    await user.click(screen.getByTestId("stream-stop-button"));
    expect(onStop).not.toHaveBeenCalled();
    expect(screen.getByTestId("stream-stop-confirm")).toBeTruthy();

    await user.click(screen.getByTestId("stream-stop-confirm-yes"));
    expect(onStop).toHaveBeenCalledTimes(1);
  });

  it("确认态点取消——收起确认，不调用 onStop", async () => {
    const user = userEvent.setup();
    const onStop = vi.fn();
    render(
      <SessionStreamContent
        sessionId={null}
        messages={[]}
        running={true}
        liveBlocks={null}
        decisionCards={[]}
        onStop={onStop}
      />,
    );
    await user.click(screen.getByTestId("stream-stop-button"));
    await user.click(screen.getByTestId("stream-stop-confirm-cancel"));
    expect(onStop).not.toHaveBeenCalled();
    expect(screen.queryByTestId("stream-stop-confirm")).toBeNull();
    expect(screen.getByTestId("stream-stop-button")).toBeTruthy();
  });

  it("running 从 true 翻 false（Stop 真正生效）——确认态自动收起，整行连带徽标一起消失", () => {
    const { rerender } = render(
      <SessionStreamContent sessionId={null} messages={[]} running={true} liveBlocks={null} decisionCards={[]} />,
    );
    fireEvent.click(screen.getByTestId("stream-stop-button"));
    expect(screen.getByTestId("stream-stop-confirm")).toBeTruthy();

    rerender(
      <SessionStreamContent sessionId={null} messages={[]} running={false} liveBlocks={null} decisionCards={[]} />,
    );
    expect(screen.queryByTestId("stream-stop-row")).toBeNull();
  });

  it("U4 图标化：Stop 按钮不再显字，靠 aria-label（既有 composer.stop i18n key）保留可及性", () => {
    render(
      <SessionStreamContent sessionId={null} messages={[]} running={true} liveBlocks={null} decisionCards={[]} />,
    );
    const stopButton = screen.getByTestId("stream-stop-button") as HTMLButtonElement;
    expect(stopButton.getAttribute("aria-label")).toBe("停止");
    expect(stopButton.textContent?.trim()).toBe("");
    expect(stopButton.querySelector("svg")).toBeTruthy();
  });
});

// F5：返回按钮从 `AppRuntime.tsx` 里独占一整行的裸元素迁进这个屏幕自己的 header 第一个子元素——
// 用例覆盖"不传 onBack 不渲染"（既有默认降级取向） + "传了 onBack 渲染在 header 首位、点击调用、
// aria-label 走 i18n（不再是硬编码英文字面量）"。
describe("SessionStreamScreen: header 返回按钮（F5：并入 header，不再独占一行）", () => {
  it("不传 onBack 时不渲染返回按钮", () => {
    render(
      <SessionStreamContent sessionId={null} messages={[]} running={false} liveBlocks={null} decisionCards={[]} />,
    );
    expect(screen.queryByTestId("app-runtime-back-to-sessions")).toBeNull();
  });

  it("传 onBack 时返回按钮是 header 第一个子元素，点击调用 onBack，aria-label 走 i18n", () => {
    const onBack = vi.fn();
    render(
      <SessionStreamContent
        sessionId={null}
        messages={[]}
        running={false}
        liveBlocks={null}
        decisionCards={[]}
        onBack={onBack}
      />,
    );
    const header = screen.getByTestId("session-stream-screen").querySelector(".stream-screen__head");
    const backButton = screen.getByTestId("app-runtime-back-to-sessions");
    expect(header?.firstElementChild).toBe(backButton);
    expect(backButton.getAttribute("aria-label")).toBe("返回会话列表");

    fireEvent.click(backButton);
    expect(onBack).toHaveBeenCalledTimes(1);
  });
});

describe("SessionStreamScreen: header stopBadge——发送中/排队/失败展示，150s 如实措辞过渡", () => {
  it("status=sending：150 秒内显示「发送停止指令…」", () => {
    const now = () => 1_000_000;
    const badge: StreamStopBadge = { commandId: "s1", status: "sending", sentAtMs: now() - 10_000 };
    render(
      <SessionStreamContent
        sessionId={null}
        messages={[]}
        running={true}
        liveBlocks={null}
        decisionCards={[]}
        stopBadge={badge}
        now={now}
      />,
    );
    expect(screen.getByTestId("stream-stop-badge").textContent).toContain("发送停止指令");
  });

  it("status=sending：超过 150 秒未 ack，措辞过渡为「未收到确认，可能仍在生效窗内」（不宣告已失效）", () => {
    vi.useFakeTimers();
    const base = 1_000_000;
    let nowValue = base;
    const now = () => nowValue;
    const badge: StreamStopBadge = { commandId: "s1", status: "sending", sentAtMs: base };
    render(
      <SessionStreamContent
        sessionId={null}
        messages={[]}
        running={true}
        liveBlocks={null}
        decisionCards={[]}
        stopBadge={badge}
        now={now}
      />,
    );

    nowValue = base + 150_000;
    act(() => {
      vi.advanceTimersByTime(150_000);
    });
    const text = screen.getByTestId("stream-stop-badge").textContent ?? "";
    expect(text).toContain("可能仍在生效窗内");
    expect(text).not.toMatch(/已失效|失效$/);
  });

  it("status=failed：显示失败 + 重试按钮", async () => {
    const user = userEvent.setup();
    const onRetry = vi.fn();
    const badge: StreamStopBadge = { commandId: "s1", status: "failed", sentAtMs: 0, onRetry };
    render(
      <SessionStreamContent
        sessionId={null}
        messages={[]}
        running={true}
        liveBlocks={null}
        decisionCards={[]}
        stopBadge={badge}
      />,
    );
    await user.click(screen.getByTestId("stream-stop-retry"));
    expect(onRetry).toHaveBeenCalledTimes(1);
  });

  it("running=false 时即便有 stopBadge 也不显示（会话已经不在跑，Stop 上下文已经过时）", () => {
    const badge: StreamStopBadge = { commandId: "s1", status: "sending", sentAtMs: 0 };
    render(
      <SessionStreamContent
        sessionId={null}
        messages={[]}
        running={false}
        liveBlocks={null}
        decisionCards={[]}
        stopBadge={badge}
      />,
    );
    expect(screen.queryByTestId("stream-stop-badge")).toBeNull();
  });
});

describe("SessionStreamScreen: history pagination", () => {
  it("shows load-earlier while history is not exhausted and invokes the callback", async () => {
    const projection = new MilestoneProjection();
    projection.historyCursor = 101;
    const onLoadEarlier = vi.fn();
    render(
      <SessionStreamScreen
        {...deriveSessionStreamProps(projection, "s-1")}
        historyLoading={false}
        onLoadEarlier={onLoadEarlier}
        initialLocale="en"
      />,
    );
    const button = screen.getByTestId("history-load-earlier");
    expect(button.textContent).toBe("Load earlier");
    fireEvent.click(button);
    expect(onLoadEarlier).toHaveBeenCalledTimes(1);
  });

  it("disables the button and shows loading copy while a request is in flight; hides it only when exhausted", () => {
    const projection = new MilestoneProjection();
    projection.historyCursor = 101;
    const { rerender } = render(
      <SessionStreamScreen
        {...deriveSessionStreamProps(projection, "s-1")}
        historyLoading
        onLoadEarlier={() => {}}
        initialLocale="zh"
      />,
    );
    const button = screen.getByTestId("history-load-earlier") as HTMLButtonElement;
    expect(button.disabled).toBe(true);
    expect(button.textContent).toBe("加载中…");

    projection.historyCursor = null;
    projection.historyExhausted = true;
    rerender(
      <SessionStreamScreen
        {...deriveSessionStreamProps(projection, "s-1")}
        historyLoading={false}
        onLoadEarlier={() => {}}
        initialLocale="zh"
      />,
    );
    expect(screen.queryByTestId("history-load-earlier")).toBeNull();
  });

  it("shows the first manual history entry when messages exist but no cursor has been requested yet", () => {
    const projection = new MilestoneProjection();
    projection.applyMsgCompleted({
      t: "msg.completed",
      message_id: 101,
      role: "assistant",
      blocks: [{ type: "text", text: "Known message" }],
    });

    render(
      <SessionStreamScreen
        {...deriveSessionStreamProps(projection, "s-1")}
        onLoadEarlier={() => {}}
        initialLocale="en"
      />,
    );

    expect(screen.getByTestId("history-load-earlier").textContent).toBe("Load earlier");
  });

  it("shows a retry button and localized lightweight error even when the first-page cursor is null", () => {
    const projection = new MilestoneProjection();
    render(
      <SessionStreamScreen
        {...deriveSessionStreamProps(projection, "s-1")}
        historyError="quota"
        onLoadEarlier={() => {}}
        initialLocale="zh"
      />,
    );

    const button = screen.getByTestId("history-load-earlier") as HTMLButtonElement;
    expect(button.disabled).toBe(false);
    expect(button.textContent).toBe("重试");
    expect(screen.getByTestId("history-load-error").textContent).toBe("本月额度已用完。");
    expect(screen.getByRole("alert")).toBe(screen.getByTestId("history-load-error"));
  });

  it("U3：全新/空会话——没有消息、没有游标、不在加载、没有错误——不渲染「加载更早」按钮", () => {
    const projection = new MilestoneProjection();
    render(
      <SessionStreamScreen
        {...deriveSessionStreamProps(projection, "s-empty")}
        onLoadEarlier={() => {}}
        initialLocale="en"
      />,
    );

    expect(screen.queryByTestId("history-load-earlier")).toBeNull();
    expect(screen.queryByTestId("history-load-error")).toBeNull();
  });

  it("U3：空会话一旦进入 loading（自动拉取已发出）仍显示按钮，不被空消息列表判据误藏", () => {
    const projection = new MilestoneProjection();
    render(
      <SessionStreamScreen
        {...deriveSessionStreamProps(projection, "s-empty-loading")}
        historyLoading
        onLoadEarlier={() => {}}
        initialLocale="en"
      />,
    );

    const button = screen.getByTestId("history-load-earlier") as HTMLButtonElement;
    expect(button.disabled).toBe(true);
    expect(button.textContent).toBe("Loading…");
  });
});

// ============================================================================
// 只读卡片呈现（差量返工 item 2·设计 §0.5 决策 6「看得到、点不动、知道去哪办」）
// ============================================================================

function decisionCardBlock(overrides: Partial<Record<string, unknown>> = {}): Record<string, unknown> {
  return {
    type: "decision_card",
    decision_id: "dec-1",
    kind: "ask",
    question: "继续执行下一步吗？",
    options: ["继续", "停止"],
    recommended: "继续",
    rationale: null,
    payload: null,
    source_run_id: "run-99",
    status: "pending",
    chosen_option: null,
    created_at: 1765430400123,
    ...overrides,
  };
}

describe("SessionStreamScreen: decision cards (DecisionCard / 交付确认) render via the desktop RunLeadTurn/DecisionCard chain, chooser disabled", () => {
  it("card visible (question + options) + option buttons disabled (no onChoose passed) + no crash without a matching msg.completed", () => {
    const projection = new MilestoneProjection();
    const created = parseFrame({ t: "card.created", block: decisionCardBlock() });
    if (!created.ok || created.frame.t !== "card.created") throw new Error("parse failed");
    projection.applyCardCreated(created.frame as CardCreatedFrame);

    const props = deriveSessionStreamProps(projection, null);
    expect(props.decisionCards).toHaveLength(1);
    render(<SessionStreamScreen {...props} initialLocale="zh" />);

    const section = screen.getByTestId("stream-decision-cards");
    // 看得到：问题文本 + 选项文本都在。
    expect(within(section).getByText("继续执行下一步吗？")).toBeTruthy();
    const optionButton = within(section).getByRole("button", { name: /继续/ });
    // 点不动：DecisionCard.tsx 的 `disabled = ... || !onChoose`——RunLeadTurn 没拿到
    // `onDecisionChoose`，这里传给 DecisionCard 的 onChoose 天然是 undefined，按钮真带
    // HTML `disabled` 属性（不是我们自己拼的 pointer-events 那套，是组件原生机制）。
    expect((optionButton as HTMLButtonElement).disabled).toBe(true);
    // 没有 lead 名字来源（source_run_id 分组、不经 buildLeadTurns）时 RunLeadTurn 落回
    // t("runLeadTurn.fallbackLeadName") 兜底文案，不是空白/崩溃。
    expect(within(section).getByText("队长")).toBeTruthy();
  });

  it("card.resolved merges status/chosen_option on top of card.created's block (latest wins, chosen renders the compact receipt line)", () => {
    const projection = new MilestoneProjection();
    const created = parseFrame({ t: "card.created", block: decisionCardBlock() });
    if (!created.ok || created.frame.t !== "card.created") throw new Error("parse failed");
    projection.applyCardCreated(created.frame as CardCreatedFrame);

    const resolved = parseFrame({ t: "card.resolved", decision_id: "dec-1", status: "chosen", chosen_option: "继续" });
    if (!resolved.ok || resolved.frame.t !== "card.resolved") throw new Error("parse failed");
    projection.applyCardResolved(resolved.frame as CardResolvedFrame);

    const props = deriveSessionStreamProps(projection, null);
    render(<SessionStreamScreen {...props} initialLocale="zh" />);

    const section = screen.getByTestId("stream-decision-cards");
    // DecisionCard.tsx: status==="chosen" → 紧凑回执行「已选：继续」，不再渲选项按钮列表。
    expect(within(section).getByText(/已选/)).toBeTruthy();
    expect(within(section).queryByRole("button", { name: /继续/ })).toBeNull();
  });

  it("multiple decision cards for the same source_run_id group into one RunLeadTurn", () => {
    const projection = new MilestoneProjection();
    const first = parseFrame({ t: "card.created", block: decisionCardBlock({ decision_id: "dec-a", question: "第一问" }) });
    const second = parseFrame({
      t: "card.created",
      block: decisionCardBlock({ decision_id: "dec-b", question: "第二问", source_run_id: "run-99" }),
    });
    if (!first.ok || first.frame.t !== "card.created" || !second.ok || second.frame.t !== "card.created") {
      throw new Error("parse failed");
    }
    projection.applyCardCreated(first.frame as CardCreatedFrame);
    projection.applyCardCreated(second.frame as CardCreatedFrame);

    const props = deriveSessionStreamProps(projection, null);
    render(<SessionStreamScreen {...props} initialLocale="zh" />);

    const section = screen.getByTestId("stream-decision-cards");
    expect(within(section).getByText("第一问")).toBeTruthy();
    expect(within(section).getByText("第二问")).toBeTruthy();
    // 同一个 source_run_id → 一个 RunLeadTurn（一个 "队长" author 行），不是两个。
    expect(within(section).getAllByText("队长")).toHaveLength(1);
  });

  it("T6f3: passing onDecisionChoose enables the option buttons, and clicking one calls it with (decisionId, option)", async () => {
    const user = userEvent.setup();
    const projection = new MilestoneProjection();
    const created = parseFrame({ t: "card.created", block: decisionCardBlock() });
    if (!created.ok || created.frame.t !== "card.created") throw new Error("parse failed");
    projection.applyCardCreated(created.frame as CardCreatedFrame);

    const props = deriveSessionStreamProps(projection, null);
    const onDecisionChoose = vi.fn();
    render(<SessionStreamScreen {...props} initialLocale="zh" onDecisionChoose={onDecisionChoose} />);

    const section = screen.getByTestId("stream-decision-cards");
    const optionButton = within(section).getByRole("button", { name: /继续/ });
    expect((optionButton as HTMLButtonElement).disabled).toBe(false);
    await user.click(optionButton);
    expect(onDecisionChoose).toHaveBeenCalledWith("dec-1", "继续");
  });

  it("T6f3: decisionAnswerOverrides('submitting') disables the option buttons even though onDecisionChoose is passed (still pending server-side)", () => {
    const projection = new MilestoneProjection();
    const created = parseFrame({ t: "card.created", block: decisionCardBlock() });
    if (!created.ok || created.frame.t !== "card.created") throw new Error("parse failed");
    projection.applyCardCreated(created.frame as CardCreatedFrame);

    const props = deriveSessionStreamProps(projection, null);
    render(
      <SessionStreamScreen
        {...props}
        initialLocale="zh"
        onDecisionChoose={() => {}}
        decisionAnswerOverrides={new Map([["dec-1", { status: "submitting", option: "继续" }]])}
      />,
    );

    const section = screen.getByTestId("stream-decision-cards");
    const optionButton = within(section).getByRole("button", { name: /继续/ });
    expect((optionButton as HTMLButtonElement).disabled).toBe(true); // block.status==="submitting" 的既有禁用规则。
  });

  it("T6f3: card.resolved arriving after a local override clears it (server truth wins, no local guessing of the winner)", () => {
    const projection = new MilestoneProjection();
    const created = parseFrame({ t: "card.created", block: decisionCardBlock() });
    if (!created.ok || created.frame.t !== "card.created") throw new Error("parse failed");
    projection.applyCardCreated(created.frame as CardCreatedFrame);
    const resolved = parseFrame({ t: "card.resolved", decision_id: "dec-1", status: "chosen", chosen_option: "停止" });
    if (!resolved.ok || resolved.frame.t !== "card.resolved") throw new Error("parse failed");
    projection.applyCardResolved(resolved.frame as CardResolvedFrame);

    const props = deriveSessionStreamProps(projection, null);
    // 本机自己曾经点过"继续"、本地覆盖仍是 submitting——但服务器最终判给了"停止"（另一台手机的
    // CAS 赢家）：展示必须服从服务器真相，不能停留在本机自己那次点击的 submitting 态。
    render(
      <SessionStreamScreen
        {...props}
        initialLocale="zh"
        onDecisionChoose={() => {}}
        decisionAnswerOverrides={new Map([["dec-1", { status: "submitting", option: "继续" }]])}
      />,
    );

    const section = screen.getByTestId("stream-decision-cards");
    expect(within(section).getByText("已选：停止")).toBeTruthy();
    expect(within(section).queryByRole("button", { name: /继续/ })).toBeNull();
  });

  it("a card.resolved with no matching card.created (block undefined) is skipped, not rendered as a broken card", () => {
    const projection = new MilestoneProjection();
    const resolved = parseFrame({ t: "card.resolved", decision_id: "dec-orphan", status: "chosen", chosen_option: "x" });
    if (!resolved.ok || resolved.frame.t !== "card.resolved") throw new Error("parse failed");
    projection.applyCardResolved(resolved.frame as CardResolvedFrame);

    const props = deriveSessionStreamProps(projection, null);
    expect(props.decisionCards).toHaveLength(1);
    expect(props.decisionCards[0].block).toBeUndefined();
    render(<SessionStreamScreen {...props} initialLocale="zh" />);

    expect(screen.queryByTestId("stream-decision-cards")).toBeNull();
  });
});

function approvalMessageFrame(): Record<string, unknown> {
  return {
    t: "msg.completed",
    message_id: 1,
    role: "assistant",
    blocks: [
      {
        type: "approval",
        approval_id: "appr-1",
        run_id: "run-1",
        tool: "shell",
        command: "rm -rf /tmp/scratch",
        summary: "rm -rf /tmp/scratch",
        cwd: "/tmp",
        request_kind: null,
        status: "pending",
      },
    ],
  };
}

function scopeChangeMessageFrame(): Record<string, unknown> {
  return {
    t: "msg.completed",
    message_id: 2,
    role: "assistant",
    blocks: [
      {
        type: "scope_change",
        changes: [
          {
            proposal_id: "prop-1",
            kind: "scope",
            detail_text: "把范围扩大到整个 lib/ 目录",
            detail_summary: null,
          },
        ],
      },
    ],
  };
}

describe("SessionStreamScreen: Approval / ScopeChange blocks get a genuinely-disabled read-only presentation (no native disabled prop, so pointer-events + hint)", () => {
  it("approval card: visible + pointer-events:none on the wrapper + '请回桌面处理' hint present", () => {
    const projection = new MilestoneProjection();
    const frame = parseFrame(approvalMessageFrame());
    if (!frame.ok || frame.frame.t !== "msg.completed") throw new Error("parse failed");
    projection.applyMsgCompleted(frame.frame as MsgCompletedFrame);

    const props = deriveSessionStreamProps(projection, null);
    render(<SessionStreamScreen {...props} initialLocale="zh" />);

    // 看得到：命令文本仍然渲染在 DOM 里。
    expect(screen.getByText(/rm -rf \/tmp\/scratch/)).toBeTruthy();
    // 点不动：包裹层内联 style 真的是 pointer-events:none（不是外部 CSS class——jsdom 对外部
    // 样式表的解析不可靠，内联 style 直接可读可测，见 SessionStreamScreen.tsx 里的注释）。
    const wrapper = screen.getByTestId("stream-restricted-content");
    expect(wrapper.style.pointerEvents).toBe("none");
    // 知道去哪办：提示文案在。
    expect(screen.getByTestId("stream-restricted-hint").textContent).toBe("请回桌面处理");
  });

  it("scope_change card: visible + pointer-events:none on the wrapper + hint present", () => {
    const projection = new MilestoneProjection();
    const frame = parseFrame(scopeChangeMessageFrame());
    if (!frame.ok || frame.frame.t !== "msg.completed") throw new Error("parse failed");
    projection.applyMsgCompleted(frame.frame as MsgCompletedFrame);

    const props = deriveSessionStreamProps(projection, null);
    render(<SessionStreamScreen {...props} initialLocale="zh" />);

    expect(screen.getByText("把范围扩大到整个 lib/ 目录")).toBeTruthy();
    const wrapper = screen.getByTestId("stream-restricted-content");
    expect(wrapper.style.pointerEvents).toBe("none");
    expect(screen.getByTestId("stream-restricted-hint").textContent).toBe("请回桌面处理");
  });

  it("msgfix2 F2 S5⑤：decision_card block also gets the restricted wrapper (对齐三端三元组：approval/scope_change/decision_card 同一套只读呈现)", () => {
    const projection = new MilestoneProjection();
    const frame = parseFrame({
      t: "msg.completed",
      message_id: 3,
      role: "assistant",
      blocks: [
        {
          type: "decision_card",
          decision_id: "dec-restricted-1",
          kind: "ask",
          question: "要不要继续？",
          options: ["继续", "停止"],
          recommended: null,
          rationale: null,
          payload: null,
          source_run_id: "run-1",
        },
      ],
    });
    if (!frame.ok || frame.frame.t !== "msg.completed") throw new Error("parse failed");
    projection.applyMsgCompleted(frame.frame as MsgCompletedFrame);

    const props = deriveSessionStreamProps(projection, null);
    render(<SessionStreamScreen {...props} initialLocale="zh" />);

    const wrapper = screen.getByTestId("stream-restricted-content");
    expect(wrapper.style.pointerEvents).toBe("none");
    expect(screen.getByTestId("stream-restricted-hint").textContent).toBe("请回桌面处理");
  });

  it("an ordinary text-only message does NOT get the restricted wrapper (no false positives)", async () => {
    const projection = new MilestoneProjection();
    const frame = parseFrame({
      t: "msg.completed",
      message_id: 1,
      role: "assistant",
      blocks: [{ type: "text", text: "Just a normal reply, nothing to restrict here." }],
    });
    if (!frame.ok || frame.frame.t !== "msg.completed") throw new Error("parse failed");
    projection.applyMsgCompleted(frame.frame as MsgCompletedFrame);

    const props = deriveSessionStreamProps(projection, null);
    render(<SessionStreamScreen {...props} initialLocale="zh" />);

    await screen.findByText("Just a normal reply, nothing to restrict here.");
    expect(screen.queryByTestId("stream-restricted-content")).toBeNull();
    expect(screen.queryByTestId("stream-restricted-hint")).toBeNull();
  });
});

// ============================================================================
// locale 同源（差量返工 item 3）——壳层文案不独立探测，跟随挂载中的桌面 I18nProvider。
// ============================================================================

/** 测试探针：拿到 @app/i18n 的 setLocale，通过 onReady 回调交给外层测试代码调用。 */
function LocaleSwitchProbe({ onReady }: { onReady: (setLocale: (locale: "zh" | "en") => void) => void }) {
  const { setLocale } = useAppI18n();
  useEffect(() => {
    onReady(setLocale);
  }, [setLocale, onReady]);
  return null;
}

describe("SessionStreamScreen: shell (stream.*) text follows the desktop I18nProvider's live locale", () => {
  it("switching locale on the same mounted I18nProvider updates the shell's status label without remounting", () => {
    let currentSetLocale: (locale: "zh" | "en") => void = () => {
      throw new Error("setLocale probe not ready");
    };

    render(
      <I18nProvider initialLocale="en">
        <LocaleSwitchProbe
          onReady={(setLocale) => {
            currentSetLocale = setLocale;
          }}
        />
        <SessionStreamContent sessionId={null} messages={[]} running={false} liveBlocks={null} decisionCards={[]} />
      </I18nProvider>,
    );

    // 挂载态：I18nProvider 传的 initialLocale="en" → remote-web 自己的 stream.* 文案也该是英文
    // （不是恒定中文、也不是独立探测出的另一个值）。
    expect(screen.getByTestId("stream-status-label").textContent).toBe("Idle");
    expect(screen.getByTestId("stream-empty").textContent).toBe("No messages yet.");

    // setLocale 触发的 state 更新发生在 React 事件处理之外（直接从测试代码调用）——包 act()
    // 让更新在下面的断言之前真正 flush 到 DOM。
    act(() => {
      currentSetLocale("zh");
    });

    // 同一棵树、同一个 I18nProvider 实例内切 locale——壳层文案要跟着变，证明读的是"当前值"而不是
    // 挂载时拍扁的快照。
    expect(screen.getByTestId("stream-status-label").textContent).toBe("空闲");
    expect(screen.getByTestId("stream-empty").textContent).toBe("还没有消息。");
  });
});

// ============================================================================
// Preview card tests ensure content_ref messages render preview blocks and a load-full-text button.
// 协议自造语料（直接手搭 `SessionStreamMessage[]`，不经 fixture/MilestoneProjection 管线；这层
// 只关心"给定 contentRef/fetchState 该渲染成什么 DOM"，数据来源已经在 milestoneProjection.test.ts/
// streamSource.test.ts 各自验过）。
// ============================================================================
describe("SessionStreamScreen: msgfix1 T6 preview card (content_ref → 'load full text' footer)", () => {
  const CONTENT_REF = { message_id: 101, revision: 1, content_sha256: "a".repeat(64), total_bytes: 2048 };

  function renderWithMessage(
    message: Partial<SessionStreamMessage> & { messageId: number },
    extra?: {
      msgFetchStates?: ReadonlyMap<number, MsgFetchState>;
      onLoadFullText?: (messageId: number) => void;
    },
  ) {
    return render(
      <I18nProvider initialLocale="en">
        <SessionStreamContent
          sessionId="s-1"
          messages={[
            {
              messageId: message.messageId,
              role: message.role ?? "assistant",
              blocks: message.blocks ?? [{ type: "text", text: "preview text" }],
              contentRef: message.contentRef,
              hasFullText: message.hasFullText ?? false,
            },
          ]}
          running={false}
          liveBlocks={null}
          decisionCards={[]}
          msgFetchStates={extra?.msgFetchStates}
          onLoadFullText={extra?.onLoadFullText}
        />
      </I18nProvider>,
    );
  }

  it("a message without content_ref never shows a fetch footer, even if a fetch state exists for its id (旧行为不更坏：无 ref 的消息渲染路径完全不受本刀影响)", () => {
    renderWithMessage({ messageId: 101 });
    expect(screen.queryByTestId("msg-fetch-load")).toBeNull();
    expect(screen.queryByTestId("msg-fetch-loading")).toBeNull();
    expect(screen.queryByTestId("msg-fetch-error")).toBeNull();
    screen.getByText("preview text"); // preview blocks 照常渲染（前向兼容硬约束）。
  });

  it("a message with content_ref and no fetch state (idle) shows a 'load full text (N KB)' button", () => {
    renderWithMessage({ messageId: 101, contentRef: CONTENT_REF });
    const button = screen.getByTestId("msg-fetch-load");
    expect(button.textContent).toBe("Load full text (2 KB)");
  });

  it("clicking the load button calls onLoadFullText with the message id", async () => {
    const user = userEvent.setup();
    const onLoadFullText = vi.fn();
    renderWithMessage({ messageId: 101, contentRef: CONTENT_REF }, { onLoadFullText });
    await user.click(screen.getByTestId("msg-fetch-load"));
    expect(onLoadFullText).toHaveBeenCalledWith(101);
  });

  it("status: loading shows a loading footer, not the load button", () => {
    renderWithMessage(
      { messageId: 101, contentRef: CONTENT_REF },
      { msgFetchStates: new Map([[101, { status: "loading" }]]) },
    );
    expect(screen.queryByTestId("msg-fetch-load")).toBeNull();
    screen.getByTestId("msg-fetch-loading");
  });

  it("a retryable error (busy) shows an unavailable-ish label with a retry button that calls onLoadFullText", async () => {
    const user = userEvent.setup();
    const onLoadFullText = vi.fn();
    renderWithMessage(
      { messageId: 101, contentRef: CONTENT_REF },
      { msgFetchStates: new Map([[101, { status: "error", errorReason: "busy" }]]), onLoadFullText },
    );
    const errorFooter = screen.getByTestId("msg-fetch-error");
    expect(errorFooter.getAttribute("data-error-reason")).toBe("busy");
    const retry = screen.getByTestId("msg-fetch-retry");
    await user.click(retry);
    expect(onLoadFullText).toHaveBeenCalledWith(101);
  });

  it("timeout is retryable (任务书 §3: 'busy/超时可重试')", () => {
    renderWithMessage(
      { messageId: 101, contentRef: CONTENT_REF },
      { msgFetchStates: new Map([[101, { status: "error", errorReason: "timeout" }]]) },
    );
    screen.getByTestId("msg-fetch-retry");
  });

  it("a terminal error (forbidden) shows 'unavailable' with no retry button (任务书 §3: 'soft_deleted/forbidden 显示不可用')", () => {
    renderWithMessage(
      { messageId: 101, contentRef: CONTENT_REF },
      { msgFetchStates: new Map([[101, { status: "error", errorReason: "forbidden" }]]) },
    );
    const errorFooter = screen.getByTestId("msg-fetch-error");
    expect(errorFooter.textContent).toBe("Full text unavailable");
    expect(screen.queryByTestId("msg-fetch-retry")).toBeNull();
  });

  it("soft_deleted is also terminal (no retry)", () => {
    renderWithMessage(
      { messageId: 101, contentRef: CONTENT_REF },
      { msgFetchStates: new Map([[101, { status: "error", errorReason: "soft_deleted" }]]) },
    );
    expect(screen.queryByTestId("msg-fetch-retry")).toBeNull();
  });

  it("stale_revision is retryable and shows the 'content changed' label", () => {
    renderWithMessage(
      { messageId: 101, contentRef: CONTENT_REF },
      { msgFetchStates: new Map([[101, { status: "error", errorReason: "stale_revision" }]]) },
    );
    const errorFooter = screen.getByTestId("msg-fetch-error");
    expect(errorFooter.textContent).toContain("Content changed");
    screen.getByTestId("msg-fetch-retry");
  });

  it("a message that already has full text never shows the footer, regardless of fetch state (footer disappears once fullBlocks lands — task brief §3: preview replaced by full text)", () => {
    renderWithMessage(
      { messageId: 101, contentRef: CONTENT_REF, hasFullText: true, blocks: [{ type: "text", text: "the full report" }] },
      { msgFetchStates: new Map([[101, { status: "loading" }]]) },
    );
    expect(screen.queryByTestId("msg-fetch-load")).toBeNull();
    expect(screen.queryByTestId("msg-fetch-loading")).toBeNull();
    screen.getByText("the full report");
  });
});

// ============================================================================
// msgfix2 U3（M0 §10.11 / 设计稿 v4.1 §4.1）：L1 活动摘要 chip——真实 fixture 样张走完整流水线
// （parseFrame → MilestoneProjection → deriveSessionStreamProps → SessionStreamScreen），断言折叠/
// 展开两态 DOM 与 revision 更新后的原位刷新；L0 保护复用文件里既有的 approvalMessageFrame() 反例。
// ============================================================================
describe("SessionStreamScreen: msgfix2 U3 活动摘要 chip（verbose 折叠/展开分级呈现）", () => {
  function applyFixtureCase(projection: MilestoneProjection, name: string): void {
    const frame = parseFrame(fixtureFrame(name));
    if (!frame.ok || frame.frame.t !== "msg.completed") throw new Error(`fixture parse failed: ${name}`);
    projection.applyMsgCompleted(frame.frame as MsgCompletedFrame);
  }

  it("verbose 关（默认省略）：折叠成一行 chip（工具调用次数 + 失败次数 + 状态），不经 MessageContent 渲染原始块", () => {
    const projection = new MilestoneProjection();
    applyFixtureCase(projection, "activity_summary_first_send");
    const props = deriveSessionStreamProps(projection, null);
    render(<SessionStreamScreen {...props} initialLocale="zh" />);

    const chip = screen.getByTestId("activity-summary-chip");
    expect(chip.getAttribute("data-state")).toBe("running");
    expect(screen.getByTestId("activity-summary-collapsed").textContent).toBe("活动 · 3 次工具调用 · 0 次失败");
    expect(screen.getByTestId("activity-summary-state").textContent).toBe("进行中");
    // 展开态明细不应该出现——折叠态是唯一 DOM。
    expect(screen.queryByTestId("activity-summary-detail")).toBeNull();
  });

  it("verbose 开：展开成按类别的计数明细（工具调用/MCP 调用/权限请求/失败）", () => {
    const projection = new MilestoneProjection();
    applyFixtureCase(projection, "activity_summary_revision_update"); // tool_calls:5 failed:1 mcp_calls:1 permission_prompts:1 state:done
    const props = deriveSessionStreamProps(projection, null);
    render(<SessionStreamScreen {...props} initialLocale="zh" verboseEnabled />);

    const chip = screen.getByTestId("activity-summary-chip");
    expect(chip.getAttribute("data-state")).toBe("done");
    expect(screen.getByTestId("activity-summary-state").textContent).toBe("已完成");
    expect(screen.getByTestId("activity-summary-detail-tools").textContent).toBe("工具调用 5 次");
    expect(screen.getByTestId("activity-summary-detail-mcp").textContent).toBe("MCP 调用 1 次");
    expect(screen.getByTestId("activity-summary-detail-permission").textContent).toBe("权限请求 1 次");
    expect(screen.getByTestId("activity-summary-detail-failed").textContent).toBe("失败 1 次");
    // 折叠态的单行文案不应该同时出现。
    expect(screen.queryByTestId("activity-summary-collapsed")).toBeNull();
  });

  it("revision 更新到达时 chip 原位刷新（同 message_id，同一个 DOM 位置，计数/状态换成新值）", () => {
    const projection = new MilestoneProjection();
    applyFixtureCase(projection, "activity_summary_first_send"); // tool_calls:3 failed:0 state:running
    const initialProps = deriveSessionStreamProps(projection, null);
    const { rerender } = render(<SessionStreamScreen {...initialProps} initialLocale="zh" />);
    expect(screen.getByTestId("activity-summary-collapsed").textContent).toBe("活动 · 3 次工具调用 · 0 次失败");

    applyFixtureCase(projection, "activity_summary_revision_update"); // 同 message_id 501，revision 1→2
    const updatedProps = deriveSessionStreamProps(projection, null);
    rerender(<SessionStreamScreen {...updatedProps} initialLocale="zh" />);

    // 只有一个 chip（原位刷新，不是追加了第二条消息）。
    expect(screen.getAllByTestId("activity-summary-chip")).toHaveLength(1);
    expect(screen.getByTestId("activity-summary-collapsed").textContent).toBe("活动 · 5 次工具调用 · 1 次失败");
    // activity_summary_revision_update 样张的 state 字段是 "done"（见 data-plane-v1.json）。
    expect(screen.getByTestId("activity-summary-state").textContent).toBe("已完成");
  });

  it("L0 保护：approval 卡永不折叠成 chip（复用文件既有的 approvalMessageFrame() 反例，同一条流水线）", () => {
    const projection = new MilestoneProjection();
    const frame = parseFrame(approvalMessageFrame());
    if (!frame.ok || frame.frame.t !== "msg.completed") throw new Error("parse failed");
    projection.applyMsgCompleted(frame.frame as MsgCompletedFrame);

    const props = deriveSessionStreamProps(projection, null);
    render(<SessionStreamScreen {...props} initialLocale="zh" verboseEnabled />);

    expect(screen.queryByTestId("activity-summary-chip")).toBeNull();
    // 既有只读呈现路径完全不受影响（回归确认：本单改动没有意外破坏 restricted 渲染分支）。
    expect(screen.getByTestId("stream-restricted-content")).toBeTruthy();
    expect(screen.getByTestId("stream-restricted-hint")).toBeTruthy();
  });
});

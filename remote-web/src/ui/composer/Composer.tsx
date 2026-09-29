// Composer.tsx — T6f3 · C1 composer：发送框 + 发送按钮（移动端·克制风）。
//
// The input box and send button map acknowledgements to delivery status without promising execution and describe expired commands explicitly.
// 发送"）+ §4d（发指令 ack 映射/弱担保措辞/expired 语义）。
//
// **U4 差量（手机端 composer 与壳层布局重排）**：Stop 按钮 + 二次确认 + `stopBadge` 展示已迁到
// `ui/stream/SessionStreamScreen.tsx` 的顶部 header 右侧（真机症状：输入框右侧 Send、下一行右侧
// Stop 两层右对齐堆叠观感错乱；规格原文 §4c "顶部会话行右侧 = Stop 按钮" 本就该在流屏 header，不
// 在 composer 底部）——本文件不再持有 `running`/`onStop`/`stopBadge` 这些 Stop 相关 props，只管
// 发送半边。
//
// **纯展示组件**——不知道 K_room/socket/command_id 这些协议细节,只吃 `app/commandChannel.ts` 折算
// 好的 `ComposerSendBadge`（`app/AppRuntime.tsx` 是唯一装配点,同 `ui/stream/SessionStreamScreen.tsx`
// 的既有"纯 props 组件"边界）。发送成功（ack outcome=ok）后本组件不显示徽标——真消息会经
// `msg.completed` 里程碑正常出现在会话流里（P0-c 已收的 user 回显),composer 自己的徽标只服务
// "还没确定成不成"这段过渡期。
//
// **计时展示（如实,不是精确协议时钟)**：`sendBadge.sentAtMs` 用于纯展示判断——发送 30 秒仍未 ack
// 时文案从"发送中"升级为"投递中(桌面可能离线)"（M2 C1 spec §4d 的 30 秒探询节奏,本组件不真的发
// 探询帧,只是文案随时间推移更如实)。用一次性 `setTimeout` 精确排到阈值那一刻触发一次重渲染,不用
// 持续 `setInterval`轮询——阈值前后各恰好一次状态翻转,足够、且测试可用假定时器精确断言。

import { useEffect, useLayoutEffect, useReducer, useRef, useState } from "react";
import { useI18n, type I18nHookValue, type Locale } from "../i18n.ts";
import { ACK_WATCHDOG_MS } from "../../app/commandChannel.ts";
import "./composer.css";

export type ComposerSendStatus =
  | "sending"
  | "not_connected"
  | "queued"
  /** C1-RQ（dogfood 修障第二批）：relay `input.relay_queued`——消息已安全排队等桌面回来，不是
   *  失败,不带重试按钮（见 `SendBadgeView` 的渲染分支）。 */
  | "relay_queued"
  /** C1：`status:"sent"` 满 `ACK_WATCHDOG_MS` 仍未见 ack/relay_queued——投递结果未知,可重试。 */
  | "delivering_uncertain"
  | "failed"
  | "expired"
  | "rate_limited"
  | "give_up";

export interface ComposerSendBadge {
  commandId: string;
  status: ComposerSendStatus;
  sentAtMs: number;
  /** 仅 failed/expired/rate_limited/give_up 有意义——点击用**新** command_id 重新发送同一段文本
   *  （M0 §3："failed 终态，重试按钮生成新 command_id 重新提交"；rate_limited 同族语义——见
   *  `app/commandChannel.ts::handleRateLimited` 注释）。 */
  onRetry?: () => void;
  /** Only set for status=failed when the desktop supplies a reason; currently only no_agent is known. */
  reason?: string;
}

export interface ComposerProps {
  onSend: (text: string) => void;
  sendBadge?: ComposerSendBadge | null;
  /** 测试注入的时钟——省略时用真 `Date.now()`。 */
  now?: () => number;
  /** 省略 = 走 `useI18n()` 的系统语言自动探测（同 `SessionStreamScreen` 的既有取向）；
   *  `app/AppRuntime.tsx` 目前也不强制传（保持与 `SessionStreamScreen` 一致的默认行为）。 */
  locale?: Locale;
}

/** C1（dogfood 修障第二批）：与 `app/commandChannel.ts` 的 ack 看门狗共用同一个 30 秒阈值来源，
 *  不再自己另开一份字面量 `30_000`（见 `ACK_WATCHDOG_MS` 定义处注释）。 */
const SLOW_DELIVERY_THRESHOLD_MS = ACK_WATCHDOG_MS;
/** 单行→多行自动长高的高度上限——对齐 composer.css `.composer__input` 现有 `max-height:120px`
 *  （两处保持同值）。搬桌面 `app/src/components/InputArea.tsx` `MAX_H`（该文件桌面值 160，手机端
 *  按既有 CSS 上限收窄到 120）。 */
const MAX_H = 120;
/** msgfix2 U6：超长文本每键读 scrollHeight 会触发同步强制布局，耗时随全文长度线性增长——
 *  超过此阈值时跳过测量、直接锁最大高度+内部滚动，避免打字卡死。搬桌面
 *  `app/src/components/InputArea.tsx` 同名常量（commit `e7ad2798`）。 */
const AUTOSIZE_MAX_CHARS = 20000;

/** 到阈值那一刻精确触发一次重渲染（一次性 timer,不持续轮询）——见文件头注"计时展示"一节。 */
function useThresholdTick(sentAtMs: number | undefined, thresholdMs: number, active: boolean, now: () => number): void {
  const [, forceTick] = useReducer((n: number) => n + 1, 0);
  useEffect(() => {
    if (!active || sentAtMs === undefined) return;
    const remaining = sentAtMs + thresholdMs - now();
    if (remaining <= 0) return; // 已经过了阈值——渲染时直接算出来即可，不需要再排一次。
    const timer = setTimeout(forceTick, remaining);
    return () => clearTimeout(timer);
  }, [sentAtMs, thresholdMs, active]);
}

export function Composer({ onSend, sendBadge, now = Date.now, locale }: ComposerProps) {
  const { t } = useI18n(locale);
  const [text, setText] = useState("");
  const taRef = useRef<HTMLTextAreaElement>(null);
  /** msgfix2 U6c：IME 组词期间守卫——照搬桌面 `InputArea.tsx:261`。 */
  const composingRef = useRef(false);

  useThresholdTick(sendBadge?.sentAtMs, SLOW_DELIVERY_THRESHOLD_MS, sendBadge?.status === "sending", now);

  /** 照抄桌面 `InputArea.tsx:271-282` 的 autosize 逻辑（含超长文本守卫）——真机症状：输入框恒
   *  一行高，多行文本只能框内滚动读不全，根因是手机端此前完全没有这半套高度重算。
   *  msgfix2 U6a：① 超长文本（>AUTOSIZE_MAX_CHARS）直接锁最大高度+内部滚动、不测量；
   *  ② scrollHeight 只读一次存局部变量，overflowY 判断复用同一读数（消除第二次强制布局）。 */
  function autosize(el: HTMLTextAreaElement | null = taRef.current): void {
    if (!el) return;
    if (el.value.length > AUTOSIZE_MAX_CHARS) {
      el.style.height = `${MAX_H}px`;
      el.style.overflowY = "auto";
      return;
    }
    el.style.height = "auto";
    const scrollHeight = el.scrollHeight;
    el.style.height = `${Math.min(scrollHeight, MAX_H)}px`;
    el.style.overflowY = scrollHeight > MAX_H ? "auto" : "hidden";
  }

  /** msgfix2 U6a：把测量从 onChange 同步路径挪进 `useLayoutEffect`（依赖 `text`）——同一渲染
   *  提交内的多次状态更新只触发一次布局测量，且仍在浏览器绘制前完成（视觉上与同步测量无差异）。
   *  比手动 rAF 更贴当前组件结构（本组件已是 `text` 驱动受控 textarea，不需要额外管理
   *  pending 句柄/卸载清理）。 */
  useLayoutEffect(() => {
    autosize();
  }, [text]);

  function handleSubmit(): void {
    const trimmed = text.trim();
    if (!trimmed) return;
    onSend(trimmed);
    // msgfix2 U6a：清空后不再手动摆样式——`text` 变 "" 会让上面的 `useLayoutEffect` 重新
    // measure 一次（此前手动 `el.style.height="auto"` 写在这里必被随后触发的 effect 覆盖，
    // 是竞态死代码，删掉）。
    setText("");
  }

  /** msgfix2 U6c：IME 组词期间（拼音/日文/韩文等）按 Enter 是在确认候选字，不是要发送——照搬
   *  桌面 `InputArea.tsx:456-464` 的三重守卫（`composingRef` 本地态 + `isComposing` + 兜底
   *  `keyCode===229`，覆盖不同浏览器对 composition 事件时序的不一致实现）。 */
  function handleKeyDown(event: React.KeyboardEvent<HTMLTextAreaElement>): void {
    if (event.key !== "Enter") return;
    const composing = composingRef.current || event.nativeEvent.isComposing || event.keyCode === 229;
    if (event.shiftKey || composing) return;
    event.preventDefault();
    handleSubmit();
  }

  return (
    <div className="composer" data-testid="composer">
      {sendBadge && (
        <SendBadgeView badge={sendBadge} now={now} t={t} />
      )}
      <div className="composer__row">
        <textarea
          ref={taRef}
          className="composer__input"
          data-testid="composer-input"
          rows={1}
          value={text}
          placeholder={t("composer.placeholder")}
          onChange={(event) => {
            setText(event.target.value);
          }}
          onKeyDown={handleKeyDown}
          onCompositionStart={() => {
            composingRef.current = true;
          }}
          onCompositionEnd={() => {
            composingRef.current = false;
          }}
        />
        <button
          type="button"
          className="composer__send"
          data-testid="composer-send"
          aria-label={t("composer.send")}
          // 返工③第③点："发送中禁用发送按钮"——`sendBadge.status==="sending"` 覆盖 seal() 进行中
          // 到 ack/expired/rate_limited/give_up 落地之前的整段在飞窗口（`app/commandChannel.ts::
          // deriveSendBadge` 把 in-flight 的 "sending"/"sent" 两个内部状态统一折成这一个展示
          // 状态），防止连点/连打字回车在同一条还没有结果的指令上再叠加一条，跟本地滑动窗
          // （`CommandChannel::takeLocalRateSlot`）互为纵深防御——UI 层先拦一道，本地窗再兜底。
          disabled={
            text.trim().length === 0 || sendBadge?.status === "sending" || sendBadge?.status === "not_connected"
          }
          onClick={handleSubmit}
        >
          <SendIcon />
        </button>
      </div>
    </div>
  );
}

/** 纸飞机线性图标——U4 图标化（发送按钮不再显字，靠 `aria-label` 保留可及性）。显式 width/height
 *  属性（本仓血泪教训：无尺寸 SVG 在 WKWebView 会撑爆布局，见 tokens.css 相邻文件同批教训记录）。 */
function SendIcon() {
  return (
    <svg
      width="20"
      height="20"
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="2"
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden="true"
    >
      <line x1="22" y1="2" x2="11" y2="13" />
      <polygon points="22 2 15 22 11 13 2 9 22 2" />
    </svg>
  );
}

function SendBadgeView({ badge, now, t }: { badge: ComposerSendBadge; now: () => number; t: I18nHookValue["t"] }) {
  const elapsed = now() - badge.sentAtMs;
  let label: string;
  let variant: "neutral" | "error" = "neutral";
  switch (badge.status) {
    case "sending":
      label = elapsed >= SLOW_DELIVERY_THRESHOLD_MS ? t("composer.deliveringMaybeOffline") : t("composer.sending");
      break;
    case "not_connected":
      label = t("connection.messageNotSent");
      variant = "error";
      break;
    case "queued":
      // M2 C1 spec §4d："ACK ...queued→「桌面忙，已排队」标签"——弱担保措辞（M0 §3：ack 未认证，
      // 不说"已执行"）。
      label = t("composer.queued");
      break;
    case "relay_queued":
      // C1-RQ（dogfood 修障第二批）：relay 已经确认收下并暂存（桌面离线），不是失败——neutral，
      // 不带重试按钮（消息没丢，见 `ComposerSendBadge.onRetry` 注释）。
      label = t("composer.relayQueued");
      break;
    case "delivering_uncertain":
      // C1：ack 看门狗超时——投递结果未知，可重试（同 failed/expired/rate_limited/give_up 一族）。
      label = t("composer.deliveringUncertain");
      variant = "error";
      break;
    case "failed":
      label = badge.reason === "no_agent" ? t("composer.failedNoAgent") : t("composer.failed");
      variant = "error";
      break;
    case "expired":
      label = t("composer.expired");
      variant = "error";
      break;
    case "rate_limited":
      label = t("composer.rateLimited");
      variant = "error";
      break;
    case "give_up":
      label = t("composer.giveUp");
      variant = "error";
      break;
  }
  return (
    <div className={`composer__badge composer__badge--${variant}`} data-testid="composer-send-badge" data-status={badge.status}>
      <span>{label}</span>
      {badge.onRetry && (
        <button type="button" data-testid="composer-send-retry" onClick={badge.onRetry}>
          {t("composer.retry")}
        </button>
      )}
    </div>
  );
}

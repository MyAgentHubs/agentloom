// SessionStreamScreen.tsx — read-only mobile session stream screen.
//
// Contract: reuses the desktop rendering leaves as-is — blocks reduced from `msg.completed`/live
// are fed unmodified via the `@app` alias into desktop's `MessageContent` (its entire internal
// leaf-routing table of 20+ components — ThinkingBlock/ToolCard/RunCard/GateCard/DecisionCard/
// BackgroundTaskStack/... reused as-is, none forked). Only this "mobile layout shell" is new.
//
// **只读**：本单不接线任何命令（发消息/答卡/Stop 是 T6f3 的事）——`MessageContent` 拿到的全部
// 交互回调（`onGateAction`/`onConfirmVerify`/`onContinueScope`/...）都不传，天然是 undefined；
// 另外**总是**给 `readonlyReason` 传一个非 null 哨兵值，让 `MessageContent`/`GateCard` 内部按
// "readonly" 语义禁用可操作的按钮（`onTakeOver`/`onCleanRedispatch`/`onConfirmVerify`/`onShelve`/
// `onRetryVerify` 都在 readonly 时被 MessageContent 强制置 undefined，见该文件里 `readonly ? ... :
// ...` 那批分支）。
//
// **只读卡片呈现（差量返工 item 2·设计 §0.5 决策 6「看得到、点不动、知道去哪办」）**：
//   - DecisionCard / 交付确认（M0 协议里两者共用同一对 card.created/card.resolved 帧，`kind` 字段
//     区分——不是两条独立 wire 路径）：`MessageContent` 自己对 `decision_card` block 固定
//     `return null`（"决策卡经 lead-turn 路径渲·不走 raw block 循环"）——单靠喂 blocks 给
//     `MessageContent` 永远看不到。改走 `decisionCardView.ts::groupDecisionCardsIntoTurns` 把
//     `MilestoneProjection.decisionCards`（按 id 合并 card.created + card.resolved 后的结果）折成
//     退化形态的 `LeadTurnView[]`，喂给桌面的 `RunLeadTurn`（内部再路由到 `DecisionCard`）；不传
//     `onDecisionChoose` = `DecisionCard.tsx` 自己的 `disabled = ... || !onChoose` 天然生效，不需要
//     额外发明遮罩。
//   - `ApprovalCard`（`resolve_approval` 直接 `invoke`，不经任何 port，桌面自己的后端 IPC 都不
//     存在——见 CLAUDE.md 项目记忆「dogfood 首 bug」系列）与 `ScopeChangeCard`（`onContinue` 未传时
//     退到 no-op，不是禁用态）都没有内建的 disabled 机制、也没有可挂钩的 className（`ApprovalCard`
//     全用内联 `style`，零 className）——没法像 DecisionCard 那样"传空 chooser"了事。改在
//     `MessageRow` 扫描 `msg.blocks` 有没有 `approval`/`scope_change` 类型块，命中就给
//     `MessageContent` 的渲染结果包一层 `pointer-events:none` + 视觉变暗（内联 style，不依赖外部
//     CSS 文件在 jsdom 下的解析，直接可测）+ 一行「请回桌面处理」提示（`stream.restrictedHint`，
//     `../i18n.ts` 自己的 `stream.*` 命名空间，不是 `@app` messages）。这层包裹是整条消息气泡级别
//     （不是单块级别）——`MessageContent` 不对外暴露"这个 block 对应哪段 DOM"的锚点，没法只包住
//     那一张卡不连累同一条消息里的其它内容；实践中 approval/scope_change 出现时通常是消息的唯一
//     内容，这个粒度收紧是有意的取舍，不是漏做。
//   - 可应答化（真正让这些卡片能点、把选择发回桌面）归 T6f3，本单只做只读呈现。
//
// **T6f3 差量：答卡激活**——`onDecisionChoose` 由调用方（`app/AppRuntime.tsx`）传入真正发送
// `input.answer` 的回调时，`RunLeadTurn`/`DecisionCard.tsx` 的既有 `disabled = ... || !onChoose`
// 天然从"禁用"翻成"可点"（本文件不新增任何禁用/启用判断逻辑——这条开关是桌面组件自带的，见文件
// 上方"chooser 传空 = 天然禁用"一节的既有说明，此处只是把它从"永远不传"改成"调用方决定传不传"）；
// 不传时行为与 T6f2 完全一致（默认禁用，向后兼容）。`decisionAnswerOverrides` 透传给
// `decisionCardView.ts::groupDecisionCardsIntoTurns` 的第二参——本机刚发出、尚未被服务器
// `card.resolved` 取代的回答在本地展示为 submitting/failed（见该函数注释："不本地臆断"）。
//
// **locale 同源（差量返工 item 3）**：壳层自己的 `stream.*` 文案不再独立探测 locale——在挂了
// `@app/i18n` 的 `I18nProvider` 的子树里，读它"当前"的 `useI18n().locale` 传给 remote-web 自己的
// `useI18n()`，两套 i18n 系统命名空间不同但共享同一个 locale 决定，且是响应式的（`I18nProvider`
// 的 locale 变化——不管出于什么原因，比如未来加了语言切换 UI——壳层文案会跟着重渲染，不是挂载时
// 拍扁的快照值）。
//
// **数据入口**：`SessionStreamProps`（见 `streamSource.ts`）——已经是从 `MilestoneProjection` +
// `LiveBlockReducer` 折算好的纯 props，本组件不知道、也不关心底下是不是真在连 wss（真 WS 接线是
// 后续单的事）。

import { useMemo } from "react";
import { I18nProvider } from "@app/i18n";
import { AttachmentPortContext } from "@app/lib/attachmentPortContext";
import { createWebAttachmentPort } from "./webAttachmentPort.ts";
import { SessionStreamContent, type SessionStreamScreenProps } from "./SessionStreamContent.tsx";

/**
 * 顶层导出组件——负责挂桌面 `I18nProvider`（叶子组件的 `useI18n()` 必须真的能工作，不是 no-op
 * 兜底）与注入 `AttachmentPortContext.Provider`（web 版 port，见 `webAttachmentPort.ts`）。两层
 * provider 都在这里挂一次，调用方（`main.tsx` / 测试）不需要自己知道要包哪些 provider。
 */
export function SessionStreamScreen({ attachmentPort, initialLocale, ...source }: SessionStreamScreenProps) {
  const port = useMemo(() => attachmentPort ?? createWebAttachmentPort(), [attachmentPort]);
  return (
    <I18nProvider initialLocale={initialLocale}>
      <AttachmentPortContext.Provider value={port}>
        <SessionStreamContent {...source} />
      </AttachmentPortContext.Provider>
    </I18nProvider>
  );
}

export { SessionStreamContent } from "./SessionStreamContent.tsx";
export type { SessionStreamScreenProps, HistoryLoadError } from "./SessionStreamContent.tsx";
export type { StreamStopStatus, StreamStopBadge } from "./sessionStreamMessageRows.tsx";

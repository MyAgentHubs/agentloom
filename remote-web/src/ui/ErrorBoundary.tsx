// ErrorBoundary.tsx — msgfix2 F2 S1（Opus 整盘审）：remote-web 此前没有任何 React error boundary
// ——`ui/stream/SessionStreamScreen.tsx` 把 `msg.blocks` 原样喂给桌面的 `MessageContent`（`@app`
// alias 直接复用，见该文件头注），若某一条消息带着一个前端还不认识的新块类型（后端新增帧型走在
// 前端更新前面，或投影层产出的形状跟前端类型定义有出入），`MessageContent` 内部对未知块的兜底
// 守卫本身已经在同一单里补上（`app/src/components/MessageContent.tsx`），但纵深防御要求：万一
// 桌面那棵 20+ 叶子组件树里还有别的地方抛出未预料的异常（不只是这一个已知点），也不该让 React
// 把整棵组件树连根卸载、留一片白屏——React 的错误边界机制本身就是"一条消息渲染出错，只丢那一条
// 消息，其它消息与外层壳（header/composer/滚动容器）继续工作"。
//
// **消息级粒度（不是整页一个大边界）**：调用方在 `messages.map()` 循环里给每条消息各包一层——
// 一条消息渲染崩溃只影响它自己（降级成一行"该消息渲染失败"提示），兄弟消息、顶部状态条、底部
// 输入框全部不受影响；不是在应用根部包一个大边界（那样一条消息崩溃仍会让整个会话流唯一的滚动区
// 塌掉，用户什么都看不见,体验上跟没有边界差别不大）。
//
// React error boundary 目前只能用 class component 实现（没有等价的 hook）——`fallback` 走 props
// 传入已经译好的节点（调用方在函数组件里用 `useI18n()`/`t()` 拼好，不需要这个类组件自己碰 i18n）。

import { Component, type ErrorInfo, type ReactNode } from "react";

interface ErrorBoundaryProps {
  children: ReactNode;
  fallback: ReactNode;
}

interface ErrorBoundaryState {
  hasError: boolean;
}

export class ErrorBoundary extends Component<ErrorBoundaryProps, ErrorBoundaryState> {
  state: ErrorBoundaryState = { hasError: false };

  static getDerivedStateFromError(): ErrorBoundaryState {
    return { hasError: true };
  }

  componentDidCatch(error: unknown, info: ErrorInfo): void {
    // 可见信号——同 CLAUDE.md「静默 fail-open 最危险」的既有教训，不能让一条消息悄悄消失却什么
    // 都不留（用户看到的只是"这条消息渲染失败"，开发者/排障时还需要真正的错误与组件栈）。
    console.error("remote-web ErrorBoundary caught a render error", error, info.componentStack);
  }

  render(): ReactNode {
    if (this.state.hasError) return this.props.fallback;
    return this.props.children;
  }
}

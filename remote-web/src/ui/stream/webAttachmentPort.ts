// webAttachmentPort.ts — T6f2 · C1 Web 版 `AttachmentPort` 实现。
//
// The web port makes resolveImageSrc return null to hide images while keeping paths, makes openExternal a no-op, and uses window.open for openUrl.
// 「AttachmentPortContext.Provider 注入 web 版 port：resolveImageSrc 返 null=显示路径不显示图、
// openExternal no-op、openUrl 用 window.open」。C1 没有本地文件系统可读（附件读取要走 relay 的
// 独立通道——不在本单范围内，本单只做只读会话流的渲染骨架），MVP 阶段这三个方法全部走最保守的
// 降级路径：不假装能做到桌面能做的事。
//
// 消费方：`app/src/lib/attachmentPortContext.ts` 定义的 `AttachmentPort` 接口——经
// `AttachmentPortContext.Provider` 注入后，桌面叶子组件（MarkdownBody → localMarkdownImage /
// Lightbox）里对 `useAttachmentPort()` 的调用会落到这里，而不是桌面默认实现（那个默认实现直接
// `invoke("read_attachment", ...)`，在 C1 里那条路径必须被这份注入的实现整个替换掉，不能让它有
// 机会碰到 `tauri-runtime-stub.ts`——`{type:"image"}` 块自己的渲染路径 `MessageContent.tsx` 里的
// `useAttachmentImage` 目前并不经这个 port（直接 invoke，是 T6e 还没来得及转的既有耦合，见本单
// worker 报告 ⑥ 偏离说明），本 port 覆盖的是已经转好的三个消费方。

import type { AttachmentPort } from "@app/lib/remoteSessionPort";

/**
 * `resolveImageSrc` 恒返回 `null`——调用方（`LocalMarkdownImage`/`Lightbox`）在 `null` 时走
 * 「显示路径不显示图」的既有降级分支（`failed=true` → `PreviewablePath`），不是新发明的状态。
 */
async function resolveImageSrc(): Promise<string | null> {
  return null;
}

/** no-op——C1 没有「用系统默认程序打开本地文件」这回事，安静地什么都不做，不 reject 制造多余报错。 */
async function openExternal(): Promise<void> {
  // 有意留空：MVP 不支持外部程序打开本地附件（浏览器沙箱本就做不到），调用方的
  // `.catch(...)` 处理是为真实失败准备的，这里不该触发它。
}

/** 外链交给浏览器自己的新标签页，`noopener,noreferrer` 防止被打开的页面拿到 `window.opener`。 */
async function openUrl(url: string): Promise<void> {
  window.open(url, "_blank", "noopener,noreferrer");
}

export function createWebAttachmentPort(): AttachmentPort {
  return { resolveImageSrc, openExternal, openUrl };
}

// 纯类型文件：零运行时依赖、零 Tauri import。
//
// 背景：remote-web 是独立的远端 Web 客户端工程，目标是原样 import 复用桌面的
// 会话流叶子组件，但那批叶子里有 7 个耦合了「直接调 Tauri 本地能力」——这个文件把耦合点
// 收窄成两个接口，供组件经 context 注入，桌面默认实现零改动、远端 Web 客户端换一套注入
// 即可复用同一份组件源码（不 fork 文件）。

/**
 * RemoteSessionPort — C1 Web 远端客户端消费的窄接口。
 *
 * 语义来源：远端控制设计文档定义的「RemoteSessionPort 最小面」
 * + M0 协议 §3（指令通道语义）。逐字对齐该处列出的事件面/命令面成员，不许自创扩面。
 *
 * 桌面 App.tsx **不实现**此接口（本单不动 App.tsx 一行）；只有远端 Web 客户端
 * （`remote-web/`）会实现它，经 wss + K_room 与 relay 通信。本文件只钉类型面，
 * 供后续单与共享叶子组件对齐契约——本单不接线、不消费。
 */
export interface RemoteSessionPort {
  // 事件面：四类会话流相关事件的订阅，返回值 = 取消订阅函数。
  /** 会话流事件（msg.completed / live delta 等）。 */
  onEvent(handler: (event: unknown) => void): () => void;
  /** session.index 快照/增量。 */
  onSessionIndex(handler: (index: unknown) => void): () => void;
  /** presence（弱担保措辞·G10 修好前不带精确设备数）。 */
  onPresence(handler: (presence: unknown) => void): () => void;
  /** epoch 变更（G4 stale_epoch 重试的触发信号）。 */
  onEpochChange(handler: (epoch: unknown) => void): () => void;

  // 命令面：四类可操作命令，均带 command_id（幂等/CAS 用）。参数名逐字对齐 §6 第 109
  // 行与 M0 wire 命名（下划线原名，不转驼峰）。
  /** 发送用户输入。对应 wire `{t:"input.send", session, text, command_id}`。 */
  sendInput(session: string, text: string, command_id: string): Promise<void>;
  /** 答卡（DecisionCard / 交付确认）。 */
  answerCard(
    session: string,
    decision_id: string,
    option: string,
    command_id: string,
  ): Promise<void>;
  /** Stop 当前会话运行。 */
  stopSession(session: string, command_id: string): Promise<void>;
  /** 请求 control.snapshot（重连/首屏对齐用）。 */
  requestSnapshot(session: string): Promise<void>;
}

/**
 * AttachmentPort — 「本地路径 → 可显示 src + 外开动作」窄接口。
 *
 * 语义来源：远端控制设计文档定义的「本地路径 → base64/外开」+ 前端实勘中梳理出的
 * B 类耦合清单（桌面叶子组件里直接依赖本地文件系统访问的那批调用点）。
 *
 * MVP 降级语义：`resolveImageSrc` 解析失败或该 port 不支持本地读取时返回 `null`，
 * 调用方按「显示路径不显示图」呈现（现行桌面组件的 `failed` 分支已是这个形状，不需要
 * 新增状态）。`openExternal` / `openUrl` 语义上允许 rejects，调用方沿用现行的
 * `.catch(...)` 处理（吐 toast 或静默）。
 */
export interface AttachmentPort {
  /**
   * 把本地附件路径解析为可显示的 `src`（如 `data:` URI）。
   * 返回 `null` 表示无法本地解析——组件应降级显示路径文本，而不是抛异常掩盖降级路径。
   */
  resolveImageSrc(
    path: string,
    sessionId?: string | null,
  ): Promise<string | null>;

  /** 用系统默认程序打开本地附件（如 html 预览）。 */
  openExternal(path: string, sessionId?: string | null): Promise<void>;

  /** 打开外部 URL（如 https 链接）。 */
  openUrl(url: string): Promise<void>;
}

// port.ts — 事件存储端口（brief §2 项 2/3：replay/去重层 + 水位事务层）。
//
// 只服务 client_msg_id 幂等 + 房间级 envelope.seq 水位——重连 `?last_seq=`（M0 §6）用的水位，
// 跟 `events/runWatermark.ts` 的"run 内 seq"水位是两个独立概念，见该文件顶注。
//
// **三样东西必须同一事务落储（审查返工·2026-08 校准）**：去重回执（client_msg_id）、房间级水位、
// 以及**事件本体（帧明文）**。早前版本只落回执+水位、不落事件本体——重载后高水位 + 去重回执会
// 让同一条帧永久拒绝重投（回执说"已应用过"），但本体从未真正持久化过，等于内容永久丢失、
// "applied:true" 这个回执在撒谎。`MilestoneProjection`（events/milestoneProjection.ts）本身可以
// 继续留在内存里、进程重启即空——但它必须能从这个持久事件日志重放重建，重建的前提是日志里
// 真有内容,不能只有"这条我见过"的回执。
//
// **`session` 字段（INT1c 审查返工·随信封同一事务落盘）**：外层信封的 `session` 字段（M0 §1）——
// `session.index` 恒 null，其余五种里程碑必须非 null——是重放/归约时唯一能确定一条里程碑属于哪个
// 会话的真相源（`msg.completed`/`card.created`/`card.resolved`/`tool.completed` 四种明文自己都不
// 带会话归属字段，见 `app/appRuntimeCore.ts` 头注）。写入时必填，读出时可能是 `undefined`——见
// `StoredEvent.session` 注释。

export interface ApplyEventInput {
  /** kind=event 帧的顶层 client_msg_id（去重手柄，M0 §1 v1.7.4·不入 AAD·仅 kind=event 携带）。 */
  clientMsgId: string;
  /** relay 盖的房间级 `envelope.seq`（仅 kind=event 有；relay 分配从 1 起的单调递增号）。 */
  seq: number;
  /** 外层信封的 `session` 字段——写入必填（调用方总是能从解密出的信封拿到它，`session.index`
   *  传 `null`，其余传实际会话 id）。 */
  session: string | null;
  /**
   * 解密后的帧明文（`parseFrame()` 的输入原始 JSON，不是已解析的 `ParsedFrame`）——存原始值，
   * 不烘焙进某个特定的 parse 版本，未来解析规则变化时旧日志仍可用新规则重新解析。
   */
  frame: unknown;
}

export interface StoredEvent {
  clientMsgId: string;
  seq: number;
  /** `undefined` = 这一行写于本字段引入之前的旧库（迁移边界，不是这一行的异常）——消费方按
   *  "无法确定归属，只能安全重建 `session.index` 类型的帧"降级处理，不当崩溃/异常处理。 */
  session: string | null | undefined;
  frame: unknown;
}

export interface ApplyEventResult {
  /**
   * true = 本次是真正的新记录（首次见到这个 client_msg_id）；
   * false = client_msg_id 命中已记录过的行——at-least-once 重投的重复到达，存储层状态未变化
   * （幂等：调用方不应该把这次当"新事件"再喂给业务层归约）。
   */
  applied: boolean;
  /** 事务提交后的房间级水位——不论 `applied` 是 true 还是 false 都如实反映当前最新值。 */
  watermark: number;
}

export interface EventStorePort {
  /**
   * 单一入口——"查 client_msg_id 是否已应用 → 若否则同一事务写入回执 + 事件本体 + 推进水位"
   * 整体在一个事务里完成。同一个 clientMsgId 调两次绝不会第二次也返回 `applied:true`；水位
   * 绝不因为乱序/重投而倒退或丢失（内部取 `max()`，不是覆盖写）；事件本体与回执/水位同生共死
   * ——三者要么一起提交，要么（事务失败/被 abort）一起不落，不存在"回执落了本体没落"的半成品。
   */
  applyEventIfNew(input: ApplyEventInput): Promise<ApplyEventResult>;

  /** 当前房间级水位；从未应用过任何事件时为 0（relay 分配的 seq 从 1 起，0 = "没有"）。 */
  getWatermark(): Promise<number>;

  hasAppliedClientMsgId(clientMsgId: string): Promise<boolean>;

  /**
   * 持久事件日志的完整读出，按 `seq` 升序返回——重载后由此重建内存态 `MilestoneProjection`
   * （对每条 `frame` 依次跑 `parseFrame()` + projection 的 `apply*`）。量级是单房间客户端侧的
   * 里程碑流，不是海量数据，这里用"整表读出再按 seq 排序"而不是建游标索引分页——足够、简单。
   */
  listEvents(): Promise<StoredEvent[]>;

  /**
   * msgfix2 U4（收拢 P1-3）：同步关闭已建立的长期连接——供 `store/cacheManager.ts::purgeRoomData()`
   * 在 `deleteDatabase()` 之前调用，避免本标签页自己的活跃连接把删库请求卡在 `onblocked`。可选：
   * `InMemoryEventStore` 没有持久连接，实现为 no-op；调用方一律用 `?.()` 调用，不强制所有实现都
   * 提供有意义的行为。
   */
  close?(): void;
}

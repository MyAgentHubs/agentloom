// SessionListScreen.tsx — T6d2 · C1 会话列表屏（移动端·纯 props 驱动）。
//
// **数据入口（同 stream/streamSource.ts 的边界取向）**：`sessions` 直接是 T6d1 事件内核
// （`MilestoneProjection.sessions`，一个 `Map<id, SessionIndexRow>`）折成数组后的形状——本组件
// 不知道、也不关心这份数组是不是真的经 `session.index` 帧（full/created/renamed/archived|
// unarchived/deleted 五种变体）折算出来的；调用方（本单测试 / 后续 INT1 真接线单）负责把
// `MilestoneProjection` 应用好，本组件只读已经归约完的"当前态"快照。`SessionIndexRow` 类型直接
// 从 `../../events/parseFrame.ts` 引入（只读引用，未改动该文件）——避免另开一份形状重复的本地
// 类型、两边字段漂移。
//
// **接线不在本单**：main.tsx 怎么挂这个屏、真实 WS 连接怎么喂 `MilestoneProjection`，归后续 INT1
// 单——本文件（连同整个 `src/ui/sessions/` 目录）是纯组件，不引用任何数据源/store/连接层。
//
// **视觉**：对齐 `stream/SessionStreamScreen.css` 的克制风与 `tokens.css` 变量——暖米底、暖橙
// accent、无 emoji；运行状态用色点（`.session-status-dot--running`/`--idle`，同 `stream-status-dot`
// 的双状态配色）表达，不在每行堆"运行中"/"空闲"这类文字（brief §「running 状态用色点不用文字堆
// 砌」）——色点上仍挂一个 `aria-label`（走 `sessions.statusRunning`/`sessions.statusIdle`），只服务
// 屏幕阅读器，不在视觉上占位置。
//
// **排序**：最近活跃在前——两个时间字段先统一为毫秒，再按
// `max(updated_at, last_activity_at ?? 0)` 降序。纯函数式排序，不改动入参数组（`.slice()` 后再
// `.sort()`），调用方传入的 `sessions` 数组本身不被本组件变异。
//
// **最近更新时间格式化**：用 UTC 分量手拼 `YYYY-MM-DD HH:mm`（不用 `toLocaleString`/
// `Intl.DateTimeFormat` 的本地时区——测试跑在 CI/开发机时区不定，用本地时区格式化会让同一个
// `updated_at` 数值在不同机器上渲染出不同字符串，测试断言会跟着抖动；UTC 分量在任何机器上都是
// 确定值）。

import type { SessionIndexRow } from "../../events/parseFrame.ts";
import { useI18n } from "../i18n.ts";
import "./SessionListScreen.css";

export interface SessionListScreenProps {
  /** T6d1 内核 `MilestoneProjection.sessions`（`Map<id, SessionIndexRow>`）折成数组后的形状——
   *  调用方负责折算与传入，本组件只读、不派生。 */
  sessions: SessionIndexRow[];
  /** 当前选中的会话 id；`null`/`undefined`/不在 `sessions` 里的 id 都等价于"无选中"。 */
  selectedId?: string | null;
  onSelect: (sessionId: string) => void;
  /** M2-4x：全量快照顶层"当前被远程的项目"名字（`MilestoneProjection.activeRepo?.name`）——
   *  `null`/`undefined` 时不渲染顶部项目名（旧桌面没带这个字段，或 active repo 恰好没有名字，
   *  两种都不是错误，安静地不显示，不是显示一段空文案）。 */
  activeRepoName?: string | null;
  /** msgfix2 U3：设置屏入口——省略时不渲染设置按钮（同 `SessionStreamScreen.tsx::onBack` 的既有
   *  降级取向；`AppRuntime.tsx` 是唯一真正接线它的调用方）。 */
  onOpenSettings?: () => void;
}

export function SessionListScreen({ sessions, selectedId, onSelect, activeRepoName, onOpenSettings }: SessionListScreenProps) {
  const { t } = useI18n();
  // `.slice()` 先拷贝——不变异调用方传入的数组（调用方可能把同一个数组引用挂在别处，如 store 里
  // 折算出的快照）。
  const sorted = sessions
    .slice()
    .sort(
      (a, b) =>
        Math.max(normalizeTimestampMs(b.updated_at), normalizeTimestampMs(b.last_activity_at ?? 0)) -
        Math.max(normalizeTimestampMs(a.updated_at), normalizeTimestampMs(a.last_activity_at ?? 0)),
    );

  return (
    <div className="session-list-screen" data-testid="session-list-screen">
      {/* msgfix2 U3：设置入口——独立于 activeRepoName 是否存在（那个条目本来就可能省略），不跟它
          共用一个容器,避免把已经过既有测试断言过 textContent 的 `session-list-active-repo` 元素
          的子节点结构改掉。 */}
      {onOpenSettings && (
        <button
          type="button"
          className="session-list__settings-button"
          data-testid="session-list-settings-button"
          aria-label={t("sessions.settings")}
          onClick={onOpenSettings}
        >
          <GearIcon />
        </button>
      )}
      {activeRepoName ? (
        <div className="session-list-header" data-testid="session-list-active-repo">
          {t("sessions.currentProject", { name: activeRepoName })}
        </div>
      ) : null}
      {sorted.length === 0 ? (
        <p className="session-list-empty" data-testid="session-list-empty">
          {t("sessions.empty")}
        </p>
      ) : (
        <ul className="session-list" data-testid="session-list">
          {sorted.map((session) => (
            <SessionRow
              key={session.id}
              session={session}
              selected={session.id === selectedId}
              onSelect={onSelect}
              runningLabel={t("sessions.statusRunning")}
              idleLabel={t("sessions.statusIdle")}
            />
          ))}
        </ul>
      )}
    </div>
  );
}

function SessionRow({
  session,
  selected,
  onSelect,
  runningLabel,
  idleLabel,
}: {
  session: SessionIndexRow;
  selected: boolean;
  onSelect: (sessionId: string) => void;
  runningLabel: string;
  idleLabel: string;
}) {
  const running = session.status === "running";
  return (
    <li className="session-list__item">
      <button
        type="button"
        className={`session-row${selected ? " session-row--selected" : ""}`}
        data-testid="session-row"
        data-session-id={session.id}
        aria-pressed={selected}
        onClick={() => onSelect(session.id)}
      >
        <span
          className={`session-status-dot session-status-dot--${running ? "running" : "idle"}`}
          data-testid="session-status-dot"
          aria-label={running ? runningLabel : idleLabel}
        />
        <span className="session-row__body">
          <span className="session-row__title">{session.title}</span>
          {session.last_msg_preview ? (
            <span
              className="session-row__preview"
              data-testid="session-row-preview"
              style={{
                overflow: "hidden",
                color: "var(--ink-3)",
                fontSize: "13px",
                textOverflow: "ellipsis",
                whiteSpace: "nowrap",
              }}
            >
              {session.last_msg_preview}
            </span>
          ) : null}
          <span className="session-row__meta">
            <span className="session-row__repo" data-testid="session-row-repo">
              {session.repo_name || session.repo_id}
            </span>
            <span className="session-row__time" data-testid="session-row-time">
              {formatUpdatedAt(session.last_activity_at ?? session.updated_at)}
            </span>
          </span>
        </span>
      </button>
    </li>
  );
}

/** 见文件头注"最近更新时间格式化"——UTC 分量手拼，不用本地时区相关 API。 */
function formatUpdatedAt(updatedAt: number): string {
  const d = new Date(normalizeTimestampMs(updatedAt));
  const pad2 = (n: number) => String(n).padStart(2, "0");
  const year = d.getUTCFullYear();
  const month = pad2(d.getUTCMonth() + 1);
  const day = pad2(d.getUTCDate());
  const hours = pad2(d.getUTCHours());
  const minutes = pad2(d.getUTCMinutes());
  return `${year}-${month}-${day} ${hours}:${minutes}`;
}

/** 线性齿轮图标（设置入口）——显式 width/height（本仓血泪教训：无尺寸 SVG 在 WKWebView 会撑爆
 *  布局，见 `SessionStreamScreen.tsx::StopIcon` 同款注释）。 */
function GearIcon() {
  return (
    <svg
      width="18"
      height="18"
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="2"
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden="true"
    >
      <circle cx="12" cy="12" r="3" />
      <path d="M19.4 15a1.65 1.65 0 0 0 .33 1.82l.06.06a2 2 0 1 1-2.83 2.83l-.06-.06a1.65 1.65 0 0 0-1.82-.33 1.65 1.65 0 0 0-1 1.51V21a2 2 0 0 1-4 0v-.09A1.65 1.65 0 0 0 9 19.4a1.65 1.65 0 0 0-1.82.33l-.06.06a2 2 0 1 1-2.83-2.83l.06-.06a1.65 1.65 0 0 0 .33-1.82 1.65 1.65 0 0 0-1.51-1H3a2 2 0 0 1 0-4h.09A1.65 1.65 0 0 0 4.6 9a1.65 1.65 0 0 0-.33-1.82l-.06-.06a2 2 0 1 1 2.83-2.83l.06.06a1.65 1.65 0 0 0 1.82.33H9a1.65 1.65 0 0 0 1-1.51V3a2 2 0 0 1 4 0v.09a1.65 1.65 0 0 0 1 1.51 1.65 1.65 0 0 0 1.82-.33l.06-.06a2 2 0 1 1 2.83 2.83l-.06.06a1.65 1.65 0 0 0-.33 1.82V9a1.65 1.65 0 0 0 1.51 1H21a2 2 0 0 1 0 4h-.09a1.65 1.65 0 0 0-1.51 1z" />
    </svg>
  );
}

/** SQLite 通常给秒，较新生产方/协议 fixture 也可能给毫秒；排序与显示必须复用同一归一规则。 */
function normalizeTimestampMs(timestamp: number): number {
  // SQLite 的 sessions/messages 时间戳是秒；既有协议 fixture/较新生产方也可能给毫秒。
  return timestamp < 100_000_000_000 ? timestamp * 1000 : timestamp;
}

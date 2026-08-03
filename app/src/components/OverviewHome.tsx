import { useEffect, useMemo, useState, type CSSProperties } from "react";
import { invoke } from "@tauri-apps/api/core";
import type { RepoMeta } from "../types/agent";
import { useI18n } from "../i18n";
import { formatLocalDayLabel } from "../lib/relativeTime";
import { formatTokenCount, sessionUsageFromSession } from "../lib/sessionUsage";

type Session = {
  id: string;
  title: string;
  repo_id?: string | null;
  group_id?: string | null;
  created_at?: number;
  pinned?: boolean;
  unread?: boolean;
  /** G3-B 用量排行用；缺省视为 0（旧调用点/测试不必都传）。 */
  total_input_tokens?: number;
  total_output_tokens?: number;
};

type Props = {
  sessions: Session[];
  repos?: RepoMeta[];
  runningSessionIds?: ReadonlySet<string>;
  onOpen: (id: string) => void;
  /** USAGE 项目排行行点击时切项目（namespace + repo）；缺省该行不可点。 */
  onSelectRepo?: (namespaceId: string, repoId: string) => void;
};

type SessionGroup = "attention" | "running" | "idle";

const emptyRunning = new Set<string>();
const emptyRepos: RepoMeta[] = [];

function repoLabel(
  session: Pick<Session, "repo_id">,
  reposById: Map<string, RepoMeta>,
  fallbackLocal: string,
  fallbackUnknown: string,
): string {
  if (!session.repo_id) return fallbackLocal;
  const repo = reposById.get(session.repo_id);
  if (!repo) return fallbackUnknown;
  return repo.owner ? `${repo.owner}/${repo.name}` : repo.name;
}

function classify(
  session: Session,
  runningSessionIds: ReadonlySet<string>,
): SessionGroup {
  if (session.unread) return "attention";
  if (runningSessionIds.has(session.id)) return "running";
  return "idle";
}

function sessionSort(a: Session, b: Session): number {
  if (Boolean(a.pinned) !== Boolean(b.pinned)) return a.pinned ? -1 : 1;
  return (b.created_at ?? 0) - (a.created_at ?? 0);
}

function OverviewSessionRow({
  session,
  group,
  repo,
  onOpen,
}: {
  session: Session;
  group: SessionGroup;
  repo: string;
  onOpen: (id: string) => void;
}) {
  const team = Boolean(session.group_id);
  const signal =
    group === "attention"
      ? "overview.signal.pending"
      : group === "running"
        ? "overview.signal.running"
        : "overview.signal.recent";
  const { t } = useI18n();

  return (
    <button
      type="button"
      className={`overview__sess overview__sess--${group}`}
      onClick={() => onOpen(session.id)}
    >
      <span className={`overview__dot overview__dot--${group}`} />
      <span className="overview__sessbody">
        <span className="overview__sttl">{session.title}</span>
        <span className="overview__meta">
          <span className="overview__repo">
            <svg viewBox="0 0 16 16" aria-hidden="true">
              <path d="M8 0a8 8 0 00-2.5 15.6V14c-2 .4-2.5-.8-2.7-1.2-.1-.2-.5-.9-.8-1.1-.3-.1-.7-.5 0-.5.6 0 1 .6 1.2.8.7 1.2 1.9.9 2.3.7.1-.5.3-.9.5-1.1-1.8-.2-3.6-.9-3.6-4 0-.9.3-1.6.8-2.1-.1-.2-.4-1 .1-2.1 0 0 .7-.2 2.2.8a7.6 7.6 0 014 0c1.5-1 2.2-.8 2.2-.8.4 1.1.2 1.9.1 2.1.5.5.8 1.2.8 2.1 0 3.1-1.9 3.8-3.7 4 .3.2.5.7.5 1.5v2.2A8 8 0 008 0z" />
            </svg>
            {repo}
          </span>
          <span>{team ? t("overview.team") : t("overview.normal")}</span>
        </span>
      </span>
      <span className="overview__agents" aria-hidden="true">
        {team ? (
          <>
            <span className="overview__agent overview__agent--lead">L</span>
            <span className="overview__agent overview__agent--worker">W</span>
          </>
        ) : (
          <span className="overview__agent overview__agent--normal">A</span>
        )}
      </span>
      <span className={`overview__sig overview__sig--${group}`}>
        {t(signal)}
      </span>
    </button>
  );
}

function OverviewSection({
  title,
  count,
  group,
  sessions,
  reposById,
  onOpen,
  collapsible = false,
}: {
  title: string;
  count: number;
  group: SessionGroup;
  sessions: Session[];
  reposById: Map<string, RepoMeta>;
  onOpen: (id: string) => void;
  collapsible?: boolean;
}) {
  const { t } = useI18n();
  const [expanded, setExpanded] = useState(false);
  const hasMore = collapsible && sessions.length > 6;
  const visibleSessions =
    hasMore && !expanded ? sessions.slice(0, 6) : sessions;

  return (
    <section className="overview__section" aria-label={title}>
      <div className={`overview__group overview__group--${group}`}>
        <span>{title}</span>
        <span className="overview__count">{count}</span>
      </div>
      <ul className="overview__sessionlist">
        {visibleSessions.map((session) => (
          <li key={session.id}>
            <OverviewSessionRow
              session={session}
              group={group}
              repo={repoLabel(
                session,
                reposById,
                t("overview.localDefault"),
                t("overview.unknownRepo"),
              )}
              onOpen={onOpen}
            />
          </li>
        ))}
      </ul>
      {hasMore && (
        <button
          type="button"
          className="overview__more"
          aria-expanded={expanded}
          onClick={() => setExpanded((current) => !current)}
        >
          {expanded
            ? t("overview.collapse")
            : t("overview.expandMore", { n: sessions.length - 6 })}
        </button>
      )}
    </section>
  );
}

/** G3-B「最近活动」一行的后端聚合形状（db::RecentActivityDay 镜像·snake_case 与后端 JSON 一致）。 */
type RecentActivityDay = {
  date: string;
  commits: number;
  files_changed: number;
  insertions: number;
  deletions: number;
  failed: number;
};

type ActivityState =
  | { kind: "loading" }
  | { kind: "error" }
  | { kind: "data"; days: RecentActivityDay[] };

function localIsoDate(date: Date): string {
  return [
    date.getFullYear(),
    String(date.getMonth() + 1).padStart(2, "0"),
    String(date.getDate()).padStart(2, "0"),
  ].join("-");
}

function fillRecentActivityDays(
  days: RecentActivityDay[],
  now = new Date(),
): RecentActivityDay[] {
  const byDate = new Map(days.map((day) => [day.date, day]));
  const today = new Date(now.getFullYear(), now.getMonth(), now.getDate());
  return Array.from({ length: 7 }, (_, index) => {
    const date = new Date(today);
    date.setDate(today.getDate() - (6 - index));
    const isoDate = localIsoDate(date);
    return (
      byDate.get(isoDate) ?? {
        date: isoDate,
        commits: 0,
        files_changed: 0,
        insertions: 0,
        deletions: 0,
        failed: 0,
      }
    );
  });
}

function isRecentActivityDayArray(
  value: unknown,
): value is RecentActivityDay[] {
  return (
    Array.isArray(value) &&
    value.every(
      (row) =>
        row != null &&
        typeof row === "object" &&
        "date" in row &&
        "commits" in row,
    )
  );
}

/**
 * 最近活动：自取数（run_commits 只读聚合），不吃 props——本节内容跟「当前会话列表」
 * 是两条独立数据轨（全库历史 vs. 当前会话），没必要绑在一起刷新。
 * 加载失败 / 后端返回形状不对 都兜底成人话文案，不炸页面（见 catch 与 isRecentActivityDayArray）。
 */
function RecentActivitySection() {
  const { t, locale } = useI18n();
  const [state, setState] = useState<ActivityState>({ kind: "loading" });
  const chartDays = useMemo(
    () =>
      state.kind === "data" && state.days.length > 0
        ? fillRecentActivityDays(state.days)
        : [],
    [state],
  );
  const maxChanges = Math.max(
    0,
    ...chartDays.map((day) => day.insertions + day.deletions),
  );
  const peakIndex = chartDays.findIndex(
    (day) => day.commits > 0 && day.insertions + day.deletions === maxChanges,
  );
  const hasFailures = chartDays.some((day) => day.failed > 0);

  useEffect(() => {
    let cancelled = false;
    invoke("recent_activity", {
      tzOffsetMinutes: -new Date().getTimezoneOffset(),
    })
      .then((result) => {
        if (cancelled) return;
        setState({
          kind: "data",
          days: isRecentActivityDayArray(result) ? result : [],
        });
      })
      .catch(() => {
        if (!cancelled) setState({ kind: "error" });
      });
    return () => {
      cancelled = true;
    };
  }, []);

  return (
    <section
      className="overview__activity overview__card"
      aria-label={t("overview.activity")}
    >
      <h3 className="overview__cardtitle">{t("overview.activity")}</h3>
      {state.kind === "error" && (
        <p className="overview__activityempty">{t("overview.activityError")}</p>
      )}
      {state.kind === "data" && state.days.length === 0 && (
        <p className="overview__activityempty">{t("overview.activityEmpty")}</p>
      )}
      {chartDays.length > 0 && (
        <>
          <div
            className="overview__activitychart"
            role="img"
            aria-label={t("overview.activityChartAria")}
          >
            <div className="overview__activitybars">
              {chartDays.map((day, index) => {
                const changes = day.insertions + day.deletions;
                const isEmpty = day.commits === 0;
                const isPeak = index === peakIndex;
                const height =
                  maxChanges > 0 ? (changes / maxChanges) * 100 : 0;
                const dayLabel = formatLocalDayLabel(day.date, locale);
                const tooltip = t(
                  day.failed > 0
                    ? "overview.activityTooltipFailed"
                    : "overview.activityTooltip",
                  {
                    date: dayLabel,
                    commits: day.commits,
                    insertions: day.insertions,
                    deletions: day.deletions,
                    failed: day.failed,
                  },
                );
                const barStyle = {
                  "--overview-activity-height": `${height}%`,
                } as CSSProperties;

                return (
                  <span
                    key={day.date}
                    className={`overview__activitydaybar${
                      isPeak ? " overview__activitydaybar--peak" : ""
                    }`}
                    data-date={day.date}
                    title={tooltip}
                    aria-hidden="true"
                  >
                    <span className="overview__activityvalue">{changes}</span>
                    <span className="overview__activitybarwell">
                      {isEmpty ? (
                        <span className="overview__activityplaceholder" />
                      ) : (
                        <span
                          className="overview__activitymark"
                          style={barStyle}
                        >
                          {day.failed > 0 && (
                            <span className="overview__activityfaildot" />
                          )}
                          <span className="overview__activitybar" />
                        </span>
                      )}
                    </span>
                  </span>
                );
              })}
            </div>
            <div className="overview__activityaxis" aria-hidden="true">
              {chartDays.map((day, index) => (
                <span
                  key={day.date}
                  className={`overview__activityaxislabel${
                    index === peakIndex
                      ? " overview__activityaxislabel--peak"
                      : ""
                  }`}
                >
                  {formatLocalDayLabel(day.date, locale)}
                </span>
              ))}
            </div>
          </div>
          {hasFailures && (
            <p className="overview__activitylegend">
              <span
                className="overview__activitylegenddot"
                aria-hidden="true"
              />
              {t("overview.activityFailureLegend")}
            </p>
          )}
          <ul className="overview__activitylist overview__activitysummary">
            {chartDays.map((day) => (
              <li key={day.date} className="overview__activityrow">
                <span className="overview__activityday">
                  {formatLocalDayLabel(day.date, locale)}
                </span>
                <span className="overview__activitystat">
                  {t("overview.activityCommits", { n: day.commits })}
                </span>
                <span className="overview__activitydelta">
                  <span>+{day.insertions}</span> <span>−{day.deletions}</span>
                </span>
                {day.failed > 0 && (
                  <span className="overview__activityfailed">
                    {t("overview.activityFailed", { n: day.failed })}
                  </span>
                )}
              </li>
            ))}
          </ul>
        </>
      )}
    </section>
  );
}

type RepoUsage = {
  key: string;
  label: string;
  /** 点击导航用；`key === "local-default"` 兜底桶或拿不到 repo 归属的 namespace 时缺省，行不可点。 */
  namespaceId?: string;
  sessionCount: number;
  input: number;
  output: number;
};

function buildUsageRanking(
  sessions: Session[],
  reposById: Map<string, RepoMeta>,
  fallbackLocal: string,
  fallbackUnknown: string,
): RepoUsage[] {
  const byRepo = new Map<string, RepoUsage>();
  for (const session of sessions) {
    const usage = sessionUsageFromSession({
      total_input_tokens: session.total_input_tokens ?? 0,
      total_output_tokens: session.total_output_tokens ?? 0,
    });
    if (usage.input === 0 && usage.output === 0) continue;
    const key = session.repo_id ?? "local-default";
    const existing = byRepo.get(key);
    if (existing) {
      existing.sessionCount += 1;
      existing.input += usage.input;
      existing.output += usage.output;
    } else {
      byRepo.set(key, {
        key,
        label: repoLabel(session, reposById, fallbackLocal, fallbackUnknown),
        namespaceId: session.repo_id
          ? reposById.get(session.repo_id)?.namespace_id
          : undefined,
        sessionCount: 1,
        input: usage.input,
        output: usage.output,
      });
    }
  }
  return Array.from(byRepo.values()).sort(
    (a, b) => b.input + b.output - (a.input + a.output),
  );
}

type SessionUsageRow = { id: string; title: string; total: number };

function buildTopSessions(
  sessions: Session[],
  limit: number,
): SessionUsageRow[] {
  return sessions
    .map((session) => ({
      id: session.id,
      title: session.title,
      total:
        (session.total_input_tokens ?? 0) + (session.total_output_tokens ?? 0),
    }))
    .filter((row) => row.total > 0)
    .sort((a, b) => b.total - a.total)
    .slice(0, limit);
}

function percentage(value: number, total: number, precision: number): number {
  if (total <= 0) return 0;
  const factor = 10 ** precision;
  return Math.round((value / total) * 100 * factor) / factor;
}

/**
 * 用量：纯 props 渲染（零后端改动·复用 sessions 上已有的 total_input_tokens /
 * total_output_tokens，App.tsx 早就在维护这两个字段）。按项目聚合成排行，
 * 外加可选的「用量最高的会话」小榜。
 *
 * 诚实标注（必做·见 overview.usageHint）：排行使用输入 + 输出总量的单色条，
 * 但带缓存命中的输入 token 会低报，因此不把输入 / 输出拆成两段强化展示。
 * Team 协作会话（lead + 队员）的用量已经并入同一会话账（merge 85589f7c）。
 */
function UsageSection({
  sessions,
  reposById,
  onOpen,
  onSelectRepo,
}: {
  sessions: Session[];
  reposById: Map<string, RepoMeta>;
  onOpen: (id: string) => void;
  onSelectRepo?: (namespaceId: string, repoId: string) => void;
}) {
  const { t } = useI18n();
  const ranking = useMemo(
    () =>
      buildUsageRanking(
        sessions,
        reposById,
        t("overview.localDefault"),
        t("overview.unknownRepo"),
      ),
    [sessions, reposById, t],
  );
  const topSessions = useMemo(() => buildTopSessions(sessions, 3), [sessions]);
  const maxRepoUsage = ranking.reduce(
    (max, repo) => Math.max(max, repo.input + repo.output),
    0,
  );
  const totalUsage = ranking.reduce(
    (total, repo) => total + repo.input + repo.output,
    0,
  );

  return (
    <section
      className="overview__usage overview__card"
      aria-label={t("overview.usage")}
    >
      <h3 className="overview__cardtitle">{t("overview.usage")}</h3>
      {ranking.length === 0 ? (
        <p className="overview__usageempty">{t("overview.usageEmpty")}</p>
      ) : (
        <ul className="overview__usagelist">
          {ranking.map((repo) => {
            const repoTotal = repo.input + repo.output;
            const tooltip = t("overview.usageTooltip", {
              project: repo.label,
              tokens: formatTokenCount(repoTotal),
              percent: percentage(repoTotal, totalUsage, 1),
            });
            const content = (
              <>
                <span className="overview__usagerepo">{repo.label}</span>
                <span className="overview__usagetrack" aria-hidden="true">
                  <span
                    className="overview__usagefill"
                    style={{
                      width: `${percentage(repoTotal, maxRepoUsage, 2)}%`,
                    }}
                  />
                </span>
                <span className="overview__usagetokens">
                  {formatTokenCount(repoTotal)}
                </span>
              </>
            );
            // 兜底桶（无 repo 归属）或拿不到 namespace 的行不可点——显式落 <li>，不装成按钮。
            const clickable =
              onSelectRepo != null &&
              repo.key !== "local-default" &&
              repo.namespaceId != null;
            return clickable ? (
              <li key={repo.key}>
                <button
                  type="button"
                  className="overview__usagerow"
                  title={tooltip}
                  onClick={() => onSelectRepo(repo.namespaceId!, repo.key)}
                >
                  {content}
                </button>
              </li>
            ) : (
              <li key={repo.key} className="overview__usagerow" title={tooltip}>
                {content}
              </li>
            );
          })}
        </ul>
      )}
      {topSessions.length > 0 && (
        <>
          <div className="overview__cardsplit" />
          <div className="overview__usagetop">
            <span className="overview__usagetoplabel">
              {t("overview.usageTopSessions")}
            </span>
            <ul className="overview__usagetoplist">
              {topSessions.map((session) => (
                <li key={session.id}>
                  <button
                    type="button"
                    className="overview__usagetoprow"
                    onClick={() => onOpen(session.id)}
                  >
                    <span className="overview__usagetopttl">
                      {session.title}
                    </span>
                    <span className="overview__usagetoptokens">
                      {formatTokenCount(session.total)}
                    </span>
                  </button>
                </li>
              ))}
            </ul>
          </div>
        </>
      )}
      <p className="overview__usagehint" title={t("overview.usageHint")}>
        {t("overview.usageHint")}
      </p>
    </section>
  );
}

export function OverviewHome({
  sessions,
  repos = emptyRepos,
  runningSessionIds = emptyRunning,
  onOpen,
  onSelectRepo,
}: Props) {
  const { t } = useI18n();
  const reposById = useMemo(() => {
    return new Map(repos.map((repo) => [repo.id, repo]));
  }, [repos]);

  const groups = useMemo(() => {
    const next: Record<SessionGroup, Session[]> = {
      attention: [],
      running: [],
      idle: [],
    };
    for (const session of sessions) {
      next[classify(session, runningSessionIds)].push(session);
    }
    next.attention.sort(sessionSort);
    next.running.sort(sessionSort);
    next.idle.sort(sessionSort);
    return next;
  }, [runningSessionIds, sessions]);

  const activeRepoCount = useMemo(() => {
    const ids = new Set(
      sessions.map((session) => session.repo_id ?? "local-default"),
    );
    return ids.size;
  }, [sessions]);

  return (
    <main className="overview">
      <div className="overview__inner">
        <header className="overview__head">
          <div>
            <h1 className="overview__title">{t("overview.title")}</h1>
            <p className="overview__subtitle">{t("overview.subtitle")}</p>
          </div>
        </header>

        {sessions.length > 0 && (
          <p
            className="overview__summary"
            aria-label={t("overview.summary", {
              attention: groups.attention.length,
              running: groups.running.length,
              repos: activeRepoCount,
            })}
          >
            <span
              className={`overview__summarycount${
                groups.attention.length > 0
                  ? " overview__summarycount--active"
                  : ""
              }`}
            >
              {groups.attention.length}
            </span>{" "}
            {t("overview.summaryAttentionSuffix")} ·{" "}
            <span
              className={`overview__summarycount${
                groups.running.length > 0
                  ? " overview__summarycount--active"
                  : ""
              }`}
            >
              {groups.running.length}
            </span>{" "}
            {t("overview.summaryRunningSuffix")} ·{" "}
            {t("overview.summaryRepos", { repos: activeRepoCount })}
          </p>
        )}

        <section
          className="overview__band"
          aria-labelledby="overview-action-band"
        >
          <h2 id="overview-action-band" className="overview__bandtitle">
            {t("overview.actionBand")}
          </h2>
          {sessions.length === 0 ? (
            <p className="overview__empty">{t("overview.empty")}</p>
          ) : (
            <>
              {groups.attention.length === 0 && groups.running.length === 0 && (
                <p className="overview__calm">{t("overview.allClear")}</p>
              )}
              <div className="overview__list">
                {groups.attention.length > 0 && (
                  <OverviewSection
                    title={t("overview.needsAttention")}
                    count={groups.attention.length}
                    group="attention"
                    sessions={groups.attention}
                    reposById={reposById}
                    onOpen={onOpen}
                  />
                )}
                {groups.running.length > 0 && (
                  <OverviewSection
                    title={t("overview.running")}
                    count={groups.running.length}
                    group="running"
                    sessions={groups.running}
                    reposById={reposById}
                    onOpen={onOpen}
                  />
                )}
                {groups.idle.length > 0 && (
                  <OverviewSection
                    title={t("overview.idle")}
                    count={groups.idle.length}
                    group="idle"
                    sessions={groups.idle}
                    reposById={reposById}
                    onOpen={onOpen}
                    collapsible
                  />
                )}
              </div>
            </>
          )}
        </section>

        <div className="overview__bandrule" />

        <section
          className="overview__band"
          aria-labelledby="overview-recap-band"
        >
          <h2 id="overview-recap-band" className="overview__bandtitle">
            {t("overview.recapBand")}
          </h2>
          <RecentActivitySection />
          <UsageSection
            sessions={sessions}
            reposById={reposById}
            onOpen={onOpen}
            onSelectRepo={onSelectRepo}
          />
        </section>
      </div>
    </main>
  );
}

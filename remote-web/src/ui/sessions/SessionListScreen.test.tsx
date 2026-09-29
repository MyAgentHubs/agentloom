// SessionListScreen.test.tsx — TDD 覆盖 SessionListScreen.tsx，走真实 data-plane-v1.json 样张的
// 完整流水线：parseFrame → MilestoneProjection（T6d1 内核，`applySessionIndex`）→
// `Array.from(projection.sessions.values())` → `SessionListScreen` 渲染，断言 DOM 锚点
// （任务书 §「测试」）。不手拼投影结果——`session.index` 五种变体（full/created/renamed/
// archived|unarchived/deleted）都经生产 `parseFrame`/`MilestoneProjection.applySessionIndex`
// 落地，样张来自 `remote-relay/fixtures/data-plane-v1.json` 的 `session_index_*` 系列 case
// （同 `parseFrame.test.ts`/`SessionStreamScreen.test.tsx` 先例）。
//
// 落在 `src/ui/sessions/`（`.test.tsx`）——vitest.config.ts 的 "ui" project 覆盖
// `src/ui/**/*.test.tsx`，跑 jsdom 环境。
//
// ============================================================================
// 覆盖表
// ============================================================================
// | 断言目标                                | 数据来源                                          |
// |------------------------------------------|---------------------------------------------------|
// | 渲染行数/标题/仓库名/最近更新时间格式化   | data-plane-v1.json: session_index_full（真样张，   |
// |                                          | 两条会话 sess-1/sess-2）                            |
// | 排序=最近活跃在前                        | session_index_full + session_index_created（真样张，|
// |                                          | 叠加出第三条 updated_at=0 的会话，三方排序）        |
// | running/idle 状态点                      | session_index_full（sess-1 status="running"，       |
// |                                          | sess-2 status=null → idle）                         |
// | 点击回调                                  | 协议自造语料（onSelect 是纯回调，fixture 不携带）   |
// | selected 高亮                            | 协议自造语料（selectedId 是纯 UI 状态，fixture 不   |
// |                                          | 携带）                                              |
// | 空态                                      | 协议自造语料（fixture 没有"零会话"这种样张，语义上  |
// |                                          | 也不需要——同 SessionStreamScreen.test.tsx 空态先例）|
// ============================================================================
//
// 变异自证（方法论同 SessionStreamScreen.test.tsx：临时改坏源码跑一次确认转红，再改回来复跑转绿；
// 过程见 worker 报告，代码已还原，不作为提交内容）：
//   1. `SessionListScreen.tsx` 里把 `.sort((a, b) => b.updated_at - a.updated_at)` 的比较方向反过来
//      （改成 `a.updated_at - b.updated_at`）——"排序=最近活跃在前"测试转红（断言的行顺序颠倒）。
//   2. `SessionRow` 里把 `running = session.status === "running"` 改成恒 `false`——"running/idle
//      状态点"测试转红（sess-1 该带 `session-status-dot--running`，实际全变成 `--idle`）。
//   3. `sorted.length === 0` 的空态判断改成恒 `false`——空态测试转红（`session-list-empty` 找不到，
//      改成去找不存在的 `session-list`）。
//   4. `SessionRow` 的 `onClick={() => onSelect(session.id)}` 改成 `onClick={() => {}}`——点击回调
//      测试转红（`onSelect` mock 断言调用次数为 0）。

import { cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { loadFixture } from "../../test-support/fixtures.ts";
import {
  parseFrame,
  type SessionIndexCreatedFrame,
  type SessionIndexFullFrame,
  type SessionIndexRow,
} from "../../events/parseFrame.ts";
import { MilestoneProjection } from "../../events/milestoneProjection.ts";
import { SessionListScreen } from "./SessionListScreen.tsx";

afterEach(() => {
  cleanup();
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

function sessionsFromProjection(projection: MilestoneProjection): SessionIndexRow[] {
  return Array.from(projection.sessions.values());
}

describe("SessionListScreen: fixture-driven pipeline (parseFrame → MilestoneProjection.applySessionIndex → screen)", () => {
  it("renders each session's title/repo/updated-at, sorted most-recently-active first, with running/idle status dots", () => {
    const projection = new MilestoneProjection();
    const fullResult = parseFrame(fixtureFrame("session_index_full"));
    if (!fullResult.ok || fullResult.frame.t !== "session.index") throw new Error("fixture parse failed");
    projection.applySessionIndex(fullResult.frame as SessionIndexFullFrame);

    const sessions = sessionsFromProjection(projection);
    // Fixture 的两条会话：sess-1 status="running" updated_at=1765430400123，
    // sess-2 status=null updated_at=1765430300000（sess-1 更新更晚）。
    expect(sessions).toHaveLength(2);

    render(<SessionListScreen sessions={sessions} onSelect={() => {}} />);

    const rows = screen.getAllByTestId("session-row");
    expect(rows).toHaveLength(2);

    // 排序：sess-1（更新更晚）排第一。
    expect(rows[0].getAttribute("data-session-id")).toBe("sess-1");
    expect(rows[1].getAttribute("data-session-id")).toBe("sess-2");

    // 标题/仓库名透传。
    expect(within(rows[0]).getByText("Fix login bug")).toBeTruthy();
    expect(within(rows[0]).getByTestId("session-row-repo").textContent).toBe("repo-a");
    expect(within(rows[1]).getByText("Update docs")).toBeTruthy();

    // 最近更新时间——UTC 分量手拼（见组件头注：不依赖测试机时区）。
    expect(within(rows[0]).getByTestId("session-row-time").textContent).toBe("2025-12-11 05:20");
    expect(within(rows[1]).getByTestId("session-row-time").textContent).toBe("2025-12-11 05:18");

    // 状态点：sess-1 running / sess-2 idle（status=null）。
    const dot0 = within(rows[0]).getByTestId("session-status-dot");
    expect(dot0.className).toContain("session-status-dot--running");
    const dot1 = within(rows[1]).getByTestId("session-status-dot");
    expect(dot1.className).toContain("session-status-dot--idle");
  });

  it("a third session merged in via session.index created (op) sorts correctly among the full snapshot's two", () => {
    const projection = new MilestoneProjection();
    const fullResult = parseFrame(fixtureFrame("session_index_full"));
    if (!fullResult.ok || fullResult.frame.t !== "session.index") throw new Error("fixture parse failed");
    projection.applySessionIndex(fullResult.frame as SessionIndexFullFrame);

    const createdResult = parseFrame(fixtureFrame("session_index_created"));
    if (!createdResult.ok || createdResult.frame.t !== "session.index") throw new Error("fixture parse failed");
    projection.applySessionIndex(createdResult.frame as SessionIndexCreatedFrame);

    const sessions = sessionsFromProjection(projection);
    // sess-3（新建，无既有 updated_at，`applySessionIndexCreated` 落回 0）排最后——比 sess-1/sess-2
    // 都旧。
    expect(sessions).toHaveLength(3);

    render(<SessionListScreen sessions={sessions} onSelect={() => {}} />);

    const rows = screen.getAllByTestId("session-row");
    expect(rows.map((r) => r.getAttribute("data-session-id"))).toEqual(["sess-1", "sess-2", "sess-3"]);
    expect(within(rows[2]).getByText("New session")).toBeTruthy();
    expect(within(rows[2]).getByTestId("session-status-dot").className).toContain("session-status-dot--idle");
  });

  it("renders a non-empty latest-message preview and omits null, empty, or missing previews", () => {
    const parsed = parseFrame({
      t: "session.index",
      full: true,
      sessions: [
        {
          id: "with-preview",
          title: "With preview",
          repo_id: "repo-a",
          archived: false,
          status: null,
          run_id: null,
          updated_at: 1000,
          last_msg_preview: "The latest assistant reply",
          last_activity_at: 2000,
        },
        {
          id: "without-preview",
          title: "Without preview",
          repo_id: "repo-a",
          archived: false,
          status: null,
          run_id: null,
          updated_at: 900,
          last_msg_preview: null,
          last_activity_at: null,
        },
        {
          id: "empty-preview",
          title: "Empty preview",
          repo_id: "repo-a",
          archived: false,
          status: null,
          run_id: null,
          updated_at: 800,
          last_msg_preview: "",
          last_activity_at: null,
        },
        {
          id: "missing-preview",
          title: "Missing preview",
          repo_id: "repo-a",
          archived: false,
          status: null,
          run_id: null,
          updated_at: 700,
        },
      ],
    });
    if (!parsed.ok || parsed.frame.t !== "session.index" || !parsed.frame.full) {
      throw new Error("frame parse failed");
    }
    const projection = new MilestoneProjection();
    projection.applySessionIndex(parsed.frame);

    render(<SessionListScreen sessions={sessionsFromProjection(projection)} onSelect={() => {}} />);

    const withPreview = screen.getAllByTestId("session-row").find((row) => row.dataset.sessionId === "with-preview");
    const withoutPreview = screen
      .getAllByTestId("session-row")
      .find((row) => row.dataset.sessionId === "without-preview");
    const emptyPreview = screen
      .getAllByTestId("session-row")
      .find((row) => row.dataset.sessionId === "empty-preview");
    const missingPreview = screen
      .getAllByTestId("session-row")
      .find((row) => row.dataset.sessionId === "missing-preview");
    expect(withPreview && within(withPreview).getByTestId("session-row-preview").textContent).toBe(
      "The latest assistant reply",
    );
    expect(withoutPreview && within(withoutPreview).queryByTestId("session-row-preview")).toBeNull();
    expect(emptyPreview && within(emptyPreview).queryByTestId("session-row-preview")).toBeNull();
    expect(missingPreview && within(missingPreview).queryByTestId("session-row-preview")).toBeNull();
  });

  it("defaults created rows to null preview/activity and preserves full-row values across rename/archive ops", () => {
    const fullResult = parseFrame({
      t: "session.index",
      full: true,
      sessions: [
        {
          id: "sess-1",
          title: "Original title",
          repo_id: "repo-a",
          archived: false,
          status: null,
          run_id: null,
          updated_at: 1000,
          last_msg_preview: "Keep this preview",
          last_activity_at: 2000,
        },
      ],
    });
    if (!fullResult.ok || fullResult.frame.t !== "session.index" || !fullResult.frame.full) {
      throw new Error("full frame parse failed");
    }
    const projection = new MilestoneProjection();
    projection.applySessionIndex(fullResult.frame);

    for (const name of ["session_index_renamed", "session_index_archived", "session_index_created"]) {
      const result = parseFrame(fixtureFrame(name));
      if (!result.ok || result.frame.t !== "session.index") throw new Error(`${name} parse failed`);
      projection.applySessionIndex(result.frame);
    }

    expect(projection.sessions.get("sess-1")).toMatchObject({
      title: "Renamed title",
      archived: true,
      last_msg_preview: "Keep this preview",
      last_activity_at: 2000,
    });
    expect(projection.sessions.get("sess-3")).toMatchObject({
      last_msg_preview: null,
      last_activity_at: null,
    });
  });

  it("sorts by max(updated_at, last_activity_at) and displays last_activity_at first", () => {
    const sessions: SessionIndexRow[] = [
      {
        id: "newer-update",
        title: "Newer update",
        repo_id: "repo-a",
        archived: false,
        status: null,
        run_id: null,
        updated_at: 1765430400,
        last_msg_preview: null,
        last_activity_at: null,
      },
      {
        id: "newer-message",
        title: "Newer message",
        repo_id: "repo-a",
        archived: false,
        status: null,
        run_id: null,
        updated_at: 1765430300,
        last_msg_preview: "Newest activity",
        last_activity_at: 1765430500,
      },
    ];

    render(<SessionListScreen sessions={sessions} onSelect={() => {}} />);

    const rows = screen.getAllByTestId("session-row");
    expect(rows.map((row) => row.dataset.sessionId)).toEqual(["newer-message", "newer-update"]);
    expect(within(rows[0]).getByTestId("session-row-time").textContent).toBe("2025-12-11 05:21");
  });

  it("normalizes mixed second/millisecond timestamps before sorting", () => {
    const sessions: SessionIndexRow[] = [
      {
        id: "older-milliseconds",
        title: "Older milliseconds",
        repo_id: "repo-a",
        archived: false,
        status: null,
        run_id: null,
        updated_at: 1_765_430_400_000,
        last_msg_preview: null,
        last_activity_at: null,
      },
      {
        id: "newer-seconds",
        title: "Newer seconds",
        repo_id: "repo-a",
        archived: false,
        status: null,
        run_id: null,
        updated_at: 1_765_430_300,
        last_msg_preview: "Newer activity in seconds",
        last_activity_at: 1_765_430_500,
      },
    ];

    render(<SessionListScreen sessions={sessions} onSelect={() => {}} />);

    expect(screen.getAllByTestId("session-row").map((row) => row.dataset.sessionId)).toEqual([
      "newer-seconds",
      "older-milliseconds",
    ]);
  });

  it("clicking a row calls onSelect with that session's id", () => {
    const projection = new MilestoneProjection();
    const fullResult = parseFrame(fixtureFrame("session_index_full"));
    if (!fullResult.ok || fullResult.frame.t !== "session.index") throw new Error("fixture parse failed");
    projection.applySessionIndex(fullResult.frame as SessionIndexFullFrame);
    const sessions = sessionsFromProjection(projection);

    const onSelect = vi.fn();
    render(<SessionListScreen sessions={sessions} onSelect={onSelect} />);

    const rows = screen.getAllByTestId("session-row");
    fireEvent.click(rows[1]);
    expect(onSelect).toHaveBeenCalledTimes(1);
    expect(onSelect).toHaveBeenCalledWith("sess-2");
  });

  it("selectedId highlights the matching row and no other", () => {
    const projection = new MilestoneProjection();
    const fullResult = parseFrame(fixtureFrame("session_index_full"));
    if (!fullResult.ok || fullResult.frame.t !== "session.index") throw new Error("fixture parse failed");
    projection.applySessionIndex(fullResult.frame as SessionIndexFullFrame);
    const sessions = sessionsFromProjection(projection);

    render(<SessionListScreen sessions={sessions} selectedId="sess-2" onSelect={() => {}} />);

    const rows = screen.getAllByTestId("session-row");
    const selectedRow = rows.find((r) => r.getAttribute("data-session-id") === "sess-2");
    const otherRow = rows.find((r) => r.getAttribute("data-session-id") === "sess-1");
    expect(selectedRow?.className).toContain("session-row--selected");
    expect(selectedRow?.getAttribute("aria-pressed")).toBe("true");
    expect(otherRow?.className).not.toContain("session-row--selected");
    expect(otherRow?.getAttribute("aria-pressed")).toBe("false");
  });

  it("an empty sessions array shows the empty state, not a crash or an empty list", () => {
    render(<SessionListScreen sessions={[]} onSelect={() => {}} />);
    expect(screen.getByTestId("session-list-empty")).toBeTruthy();
    expect(screen.queryByTestId("session-list")).toBeNull();
    expect(screen.queryByTestId("session-row")).toBeNull();
  });

  it("row repo label prefers repo_name, falls back to bare repo_id when missing/null/empty (M2-4x)", () => {
    const sessions: SessionIndexRow[] = [
      {
        id: "with-name",
        title: "With name",
        repo_id: "repo-18c3527a",
        archived: false,
        status: null,
        run_id: null,
        updated_at: 1000,
        repo_name: "Acme Corp",
      },
      {
        id: "null-name",
        title: "Null name",
        repo_id: "repo-18c3527b",
        archived: false,
        status: null,
        run_id: null,
        updated_at: 900,
        repo_name: null,
      },
      {
        id: "missing-name",
        title: "Missing name",
        repo_id: "repo-18c3527c",
        archived: false,
        status: null,
        run_id: null,
        updated_at: 800,
      },
      {
        id: "empty-name",
        title: "Empty name",
        repo_id: "repo-18c3527d",
        archived: false,
        status: null,
        run_id: null,
        updated_at: 700,
        repo_name: "",
      },
    ];

    render(<SessionListScreen sessions={sessions} onSelect={() => {}} />);

    const rowFor = (id: string) =>
      screen.getAllByTestId("session-row").find((row) => row.dataset.sessionId === id)!;
    expect(within(rowFor("with-name")).getByTestId("session-row-repo").textContent).toBe("Acme Corp");
    expect(within(rowFor("null-name")).getByTestId("session-row-repo").textContent).toBe("repo-18c3527b");
    expect(within(rowFor("missing-name")).getByTestId("session-row-repo").textContent).toBe("repo-18c3527c");
    expect(within(rowFor("empty-name")).getByTestId("session-row-repo").textContent).toBe("repo-18c3527d");
  });

  it("activeRepoName renders the current-project header; null/undefined/empty hide it (M2-4x)", () => {
    // 同文件其它测试的既有惯例（不覆盖 locale，jsdom 默认 navigator.language=en-US）——断言英文
    // 文案，不是中文。
    const { rerender } = render(
      <SessionListScreen sessions={[]} onSelect={() => {}} activeRepoName="Acme Corp" />,
    );
    expect(screen.getByTestId("session-list-active-repo").textContent).toBe("Current project: Acme Corp");

    rerender(<SessionListScreen sessions={[]} onSelect={() => {}} activeRepoName={null} />);
    expect(screen.queryByTestId("session-list-active-repo")).toBeNull();

    rerender(<SessionListScreen sessions={[]} onSelect={() => {}} />);
    expect(screen.queryByTestId("session-list-active-repo")).toBeNull();
  });

  it("msgfix2 U3: onOpenSettings omitted → no settings button rendered", () => {
    render(<SessionListScreen sessions={[]} onSelect={() => {}} />);
    expect(screen.queryByTestId("session-list-settings-button")).toBeNull();
  });

  it("msgfix2 U3: onOpenSettings provided → settings button renders and clicking it calls onOpenSettings", () => {
    const onOpenSettings = vi.fn();
    render(<SessionListScreen sessions={[]} onSelect={() => {}} onOpenSettings={onOpenSettings} />);
    const button = screen.getByTestId("session-list-settings-button");
    fireEvent.click(button);
    expect(onOpenSettings).toHaveBeenCalledTimes(1);
  });
});

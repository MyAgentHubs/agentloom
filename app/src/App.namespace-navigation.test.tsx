import { render, screen, fireEvent, waitFor } from "@testing-library/react";
import type { ComponentProps } from "react";
import { describe, it, expect, vi } from "vitest";
import App from "./App";
import { setupAppTests } from "./__tests__/helpers/appTestSetup";

const { invokeMock, listenMock, openMock, sessionMainProps } = vi.hoisted(
  () => ({
    invokeMock: vi.fn(),
    listenMock: vi.fn(),
    openMock: vi.fn(),
    sessionMainProps: [] as Array<{
      onOpenPreview?: (path: string) => void;
      busy?: boolean;
      messages?: Array<{
        role: "user" | "assistant";
        content: unknown[];
        engine?: string;
        agent_id?: string | null;
        agent_name_snapshot?: string | null;
        // V3a：活尾唯一不变量断言需要读这个字段。
        stream_live?: boolean;
      }>;
    }>,
  }),
);

// VITEST_DEFER_INVOKE=1 makes every invoke settle one macrotask later, which
// deterministically exposes assertions that read state landing from a *different*
// async source than the one they awaited. CI runners are ~12x slower than a dev
// machine and lose those races for real; this switch reproduces it on purpose.
vi.mock("@tauri-apps/api/core", () => ({
  invoke: (...args: unknown[]) =>
    process.env.VITEST_DEFER_INVOKE
      ? new Promise((r) => setTimeout(r, 0)).then(() =>
          (invokeMock as (...a: unknown[]) => unknown)(...args),
        )
      : (invokeMock as (...a: unknown[]) => unknown)(...args),
}));
vi.mock("@tauri-apps/api/event", () => ({ listen: listenMock }));
vi.mock("@tauri-apps/plugin-dialog", () => ({ open: openMock }));
vi.mock("./components/SessionMain", async (importOriginal) => {
  const actual =
    await importOriginal<typeof import("./components/SessionMain")>();
  const OriginalSessionMain = actual.SessionMain;
  return {
    ...actual,
    SessionMain: (props: ComponentProps<typeof OriginalSessionMain>) => {
      sessionMainProps.push(props);
      return <OriginalSessionMain {...props} />;
    },
  };
});

declare const process: { env: Record<string, string | undefined> };

describe("App", () => {
  const { agentProfiles } = setupAppTests({
    invokeMock,
    listenMock,
    openMock,
    sessionMainProps,
  });

  it("导航 IA：跨 namespace 选 repo 原子切换（set_active_namespace 先于 set_last_active_repo·显式 nsId·不 clobber·repo1 显选中）", async () => {
    const localNs = {
      id: "local",
      kind: "local",
      name: "本机",
      is_builtin: 1,
      last_active_repo_id: null,
      added_at: 0,
      last_used_at: null,
    };
    const acmeNs = {
      id: "gh:acme",
      kind: "github_org",
      name: "acme",
      is_builtin: 0,
      last_active_repo_id: "r-acme",
      added_at: 0,
      last_used_at: null,
    };
    // gh:other 的旧 last-active = r-stale（codex round-2 BLOCK：故意 ≠ 目标 r-other）
    const otherNs = {
      id: "gh:other",
      kind: "github_org",
      name: "other",
      is_builtin: 0,
      last_active_repo_id: "r-stale",
      added_at: 0,
      last_used_at: null,
    };
    const mk = (id: string, name: string, nsId: string, owner: string) => ({
      id,
      name,
      source: "github",
      owner,
      path: `/tmp/${name}`,
      status: "active",
      added_at: 0,
      last_used_at: null,
      namespace_id: nsId,
    });
    const rAcme = mk("r-acme", "acme-web", "gh:acme", "acme");
    const rStale = mk("r-stale", "stale-svc", "gh:other", "other"); // gh:other 旧 last-active
    const rOther = mk("r-other", "other-svc", "gh:other", "other"); // 用户真正点选的

    invokeMock.mockImplementation((cmd: string) => {
      if (cmd === "list_agents") return Promise.resolve(agentProfiles);
      if (cmd === "list_sessions") return Promise.resolve([]);
      if (cmd === "app_context")
        return Promise.resolve({
          namespaces: [localNs, acmeNs, otherNs],
          active_namespace_id: "gh:acme",
          active_repo_id: "r-acme",
          repos: [rAcme],
        });
      if (cmd === "list_repos") return Promise.resolve([rAcme, rStale, rOther]);
      if (cmd === "list_namespaces")
        return Promise.resolve([localNs, acmeNs, otherNs]);
      // 关键陷阱：set_active_namespace 返回 gh:other 的旧 last-active = "r-stale"。
      // 正确实现忽略此返回值·用显式 repoId="r-other"；错误实现（用返回值 set repo）会落到 r-stale → 测试抓住。
      if (cmd === "set_active_namespace") return Promise.resolve("r-stale");
      if (cmd === "set_last_active_repo") return Promise.resolve();
      if (cmd === "list_groups") return Promise.resolve([]);
      if (cmd === "session_review")
        return Promise.resolve({ has_changes: false });
      return Promise.resolve();
    });

    render(<App />);
    await waitFor(() => expect(invokeMock).toHaveBeenCalledWith("app_context"));
    // 初始项目切换器 repo = acme-web
    await waitFor(() =>
      expect(screen.getByLabelText("项目切换器")).toHaveTextContent("acme-web"),
    );

    // 开项目切换器下拉 → 选别 namespace 的 repo（other-svc 在 gh:other 段）
    fireEvent.click(screen.getByLabelText("项目切换器"));
    fireEvent.click(screen.getByText("other-svc"));

    // 持久化：set_last_active_repo 参数用新 nsId（非闭包旧 gh:acme）
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("set_last_active_repo", {
        namespaceId: "gh:other",
        repoId: "r-other",
      }),
    );
    // set_active_namespace 必先于 set_last_active_repo（禁顺序调 clobber 的核心断言）
    const calls = invokeMock.mock.calls.map((c) => c[0]);
    const iNs = calls.indexOf("set_active_namespace");
    const iRepo = calls.indexOf("set_last_active_repo");
    expect(iNs).toBeGreaterThanOrEqual(0);
    expect(iNs).toBeLessThan(iRepo);
    expect(invokeMock).toHaveBeenCalledWith("set_active_namespace", {
      id: "gh:other",
    });
    // 各只调一次（防顺序调/重复调污染·codex round-2）
    expect(calls.filter((c) => c === "set_active_namespace").length).toBe(1);
    expect(calls.filter((c) => c === "set_last_active_repo").length).toBe(1);
    // 切后项目切换器 = 选中的 other-svc（**非** set_active_namespace 返回的旧 stale-svc·这是抓 clobber 的关键断言）
    await waitFor(() =>
      expect(screen.getByLabelText("项目切换器")).toHaveTextContent(
        "other-svc",
      ),
    );
    expect(screen.getByLabelText("项目切换器")).not.toHaveTextContent(
      "stale-svc",
    );
  });

  it("导航历史：项目简介之间切 repo 后左侧栏后退/前进恢复具体 repo", async () => {
    const acmeNs = {
      id: "gh:acme",
      kind: "github_org",
      name: "acme",
      is_builtin: 0,
      last_active_repo_id: "r-acme",
      added_at: 0,
      last_used_at: null,
    };
    const otherNs = {
      id: "gh:other",
      kind: "github_org",
      name: "other",
      is_builtin: 0,
      last_active_repo_id: "r-other",
      added_at: 0,
      last_used_at: null,
    };
    const mk = (id: string, name: string, nsId: string, owner: string) => ({
      id,
      name,
      source: "github",
      owner,
      path: `/tmp/${name}`,
      status: "active",
      added_at: 0,
      last_used_at: null,
      namespace_id: nsId,
    });
    const rAcme = mk("r-acme", "acme-web", "gh:acme", "acme");
    const rOther = mk("r-other", "other-svc", "gh:other", "other");

    invokeMock.mockImplementation((cmd: string) => {
      if (cmd === "list_agents") return Promise.resolve(agentProfiles);
      if (cmd === "list_sessions") return Promise.resolve([]);
      if (cmd === "app_context")
        return Promise.resolve({
          namespaces: [acmeNs, otherNs],
          active_namespace_id: "gh:acme",
          active_repo_id: "r-acme",
          repos: [rAcme],
        });
      if (cmd === "list_repos") return Promise.resolve([rAcme, rOther]);
      if (cmd === "list_namespaces") return Promise.resolve([acmeNs, otherNs]);
      if (cmd === "set_active_namespace") return Promise.resolve("r-other");
      if (cmd === "set_last_active_repo") return Promise.resolve();
      if (cmd === "list_groups") return Promise.resolve([]);
      if (cmd === "create_session") return Promise.resolve();
      if (cmd === "session_review")
        return Promise.resolve({ has_changes: false });
      return Promise.resolve();
    });

    render(<App />);
    await waitFor(() =>
      expect(screen.getByLabelText("项目切换器")).toHaveTextContent("acme-web"),
    );

    fireEvent.click(screen.getByText("项目简介"));
    expect(
      await screen.findByRole("heading", { name: "acme-web" }),
    ).toBeInTheDocument();

    fireEvent.click(screen.getByLabelText("项目切换器"));
    fireEvent.click(screen.getByText("other-svc"));
    await waitFor(() =>
      expect(screen.getByLabelText("项目切换器")).toHaveTextContent(
        "other-svc",
      ),
    );
    expect(
      await screen.findByRole("heading", { name: "other-svc" }),
    ).toBeInTheDocument();

    await waitFor(() =>
      expect(screen.getByLabelText("后退")).not.toBeDisabled(),
    );
    fireEvent.click(screen.getByLabelText("后退"));
    await waitFor(() =>
      expect(screen.getByLabelText("项目切换器")).toHaveTextContent("acme-web"),
    );
    expect(
      await screen.findByRole("heading", { name: "acme-web" }),
    ).toBeInTheDocument();

    await waitFor(() =>
      expect(screen.getByLabelText("前进")).not.toBeDisabled(),
    );
    fireEvent.click(screen.getByLabelText("前进"));
    await waitFor(() =>
      expect(screen.getByLabelText("项目切换器")).toHaveTextContent(
        "other-svc",
      ),
    );
    expect(
      await screen.findByRole("heading", { name: "other-svc" }),
    ).toBeInTheDocument();
  });

  it("导航 IA：onSelectRepoInNamespace 刷新/持久化 IPC 失败 → catch return 不半切（UI 保持旧 repo·codex T3 审 BLOCK-1）", async () => {
    const localNs = {
      id: "local",
      kind: "local",
      name: "本机",
      is_builtin: 1,
      last_active_repo_id: null,
      added_at: 0,
      last_used_at: null,
    };
    const acmeNs = {
      id: "gh:acme",
      kind: "github_org",
      name: "acme",
      is_builtin: 0,
      last_active_repo_id: "r-acme",
      added_at: 0,
      last_used_at: null,
    };
    const otherNs = {
      id: "gh:other",
      kind: "github_org",
      name: "other",
      is_builtin: 0,
      last_active_repo_id: "r-other",
      added_at: 0,
      last_used_at: null,
    };
    const mk = (id: string, name: string, nsId: string, owner: string) => ({
      id,
      name,
      source: "github",
      owner,
      path: `/tmp/${name}`,
      status: "active",
      added_at: 0,
      last_used_at: null,
      namespace_id: nsId,
    });
    const rAcme = mk("r-acme", "acme-web", "gh:acme", "acme");
    const rOther = mk("r-other", "other-svc", "gh:other", "other");

    invokeMock.mockImplementation((cmd: string) => {
      if (cmd === "list_agents") return Promise.resolve(agentProfiles);
      if (cmd === "list_sessions") return Promise.resolve([]);
      if (cmd === "app_context")
        return Promise.resolve({
          namespaces: [localNs, acmeNs, otherNs],
          active_namespace_id: "gh:acme",
          active_repo_id: "r-acme",
          repos: [rAcme],
        });
      if (cmd === "list_repos") return Promise.resolve([rAcme, rOther]);
      if (cmd === "list_namespaces")
        return Promise.resolve([localNs, acmeNs, otherNs]);
      if (cmd === "set_active_namespace") return Promise.resolve("r-other");
      // 持久化第二步失败：handler 须 catch return·不切 UI（也不产生未捕获 rejection）
      if (cmd === "set_last_active_repo")
        return Promise.reject(new Error("DB_WRITE_FAILED"));
      if (cmd === "list_groups") return Promise.resolve([]);
      if (cmd === "session_review")
        return Promise.resolve({ has_changes: false });
      return Promise.resolve();
    });

    render(<App />);
    await waitFor(() => expect(invokeMock).toHaveBeenCalledWith("app_context"));
    await waitFor(() =>
      expect(screen.getByLabelText("项目切换器")).toHaveTextContent("acme-web"),
    );

    fireEvent.click(screen.getByLabelText("项目切换器"));
    fireEvent.click(screen.getByText("other-svc"));

    // set_last_active_repo 被调（确认走到了第二步才失败）
    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("set_last_active_repo", {
        namespaceId: "gh:other",
        repoId: "r-other",
      }),
    );
    // 失败后 UI 不半切：项目切换器仍显旧 repo（catch return·未 set state）
    expect(screen.getByLabelText("项目切换器")).toHaveTextContent("acme-web");
  });
});

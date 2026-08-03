import { render, screen, fireEvent } from "@testing-library/react";
import { describe, it, expect, vi, beforeEach } from "vitest";
import { RepoDropdown } from "./RepoDropdown";
import type { RepoMeta, Session } from "../types/agent";
import { makeSession } from "../test/factories";

const repoA: RepoMeta = {
  id: "r-a",
  source: "github",
  owner: "myagenthubs",
  name: "agentloom",
  path: "/tmp/a",
  status: "active",
  added_at: 0,
  last_used_at: null,
  namespace_id: "ns-a",
};
const repoB: RepoMeta = {
  id: "r-b",
  source: "github",
  owner: "myagenthubs",
  name: "my-blog",
  path: "/tmp/b",
  status: "active",
  added_at: 0,
  last_used_at: null,
  namespace_id: "ns-a",
};
const repoC: RepoMeta = {
  id: "r-c",
  source: "github",
  owner: "myagenthubs",
  name: "portfolio-site",
  path: "/tmp/c",
  status: "active",
  added_at: 0,
  last_used_at: null,
  namespace_id: "ns-a",
};

const sessions: Session[] = [
  makeSession({ id: "s1", title: "x1", repo_id: "r-a", namespace_id: "ns-a" }),
  makeSession({ id: "s2", title: "x2", repo_id: "r-a", namespace_id: "ns-a" }),
  makeSession({ id: "s3", title: "x3", repo_id: "r-a", namespace_id: "ns-a" }),
  makeSession({ id: "s4", title: "y1", repo_id: "r-b", namespace_id: "ns-a" }),
  makeSession({ id: "s5", title: "y2", repo_id: "r-b", namespace_id: "ns-a" }),
  makeSession({ id: "s6", title: "z1", repo_id: "r-c", namespace_id: "ns-a" }),
];

const base = {
  open: true,
  repos: [repoA, repoB, repoC],
  activeRepoId: "r-a" as string | null,
  sessions,
  onSelectRepo: vi.fn(),
  onClose: vi.fn(),
};

describe("RepoDropdown · v4 严格保真", () => {
  beforeEach(() => {
    base.onSelectRepo.mockReset();
    base.onClose.mockReset();
  });

  it("open=false 时不渲染", () => {
    const { container } = render(<RepoDropdown {...base} open={false} />);
    expect(container.firstChild).toBeNull();
  });

  it("v4 DOM：.dropdown.repo / .dd-search input placeholder「搜索 repo…」 / 3 .dd-row 含 .dd-av.repo + .dd-nm + .dd-ct + active 行 ✓", () => {
    const { container } = render(<RepoDropdown {...base} />);
    expect(container.querySelector(".dropdown.repo")).not.toBeNull();
    const search = container.querySelector(
      ".dd-search input",
    ) as HTMLInputElement;
    expect(search.placeholder).toMatch(/搜索 repo/);
    expect(container.querySelectorAll(".dd-row").length).toBe(3);
    const aRow = screen.getByText("agentloom").closest(".dd-row");
    expect(aRow!.classList.contains("active")).toBe(true);
    expect(aRow!.querySelector(".dd-check")!.textContent).toBe("✓");
    expect(aRow!.querySelector(".dd-av.repo")).not.toBeNull();
    expect(aRow!.querySelector(".dd-ct")!.textContent).toBe("3");
    const bRow = screen.getByText("my-blog").closest(".dd-row");
    expect(bRow!.querySelector(".dd-ct")!.textContent).toBe("2");
    const cRow = screen.getByText("portfolio-site").closest(".dd-row");
    expect(cRow!.querySelector(".dd-ct")!.textContent).toBe("1");
  });

  it("点 repo 行 → onSelectRepo + onClose", () => {
    const onSelectRepo = vi.fn();
    const onClose = vi.fn();
    render(
      <RepoDropdown {...base} onSelectRepo={onSelectRepo} onClose={onClose} />,
    );
    fireEvent.click(screen.getByText("my-blog"));
    expect(onSelectRepo).toHaveBeenCalledWith("r-b");
    expect(onClose).toHaveBeenCalledTimes(1);
  });

  it("search filter rows · 'port' 只剩 portfolio-site", () => {
    const { container } = render(<RepoDropdown {...base} />);
    const search = container.querySelector(
      ".dd-search input",
    ) as HTMLInputElement;
    fireEvent.change(search, { target: { value: "port" } });
    expect(screen.queryByText("agentloom")).not.toBeInTheDocument();
    expect(screen.queryByText("my-blog")).not.toBeInTheDocument();
    expect(screen.getByText("portfolio-site")).toBeInTheDocument();
  });

  it("无 section / 无 foot（v4 state 4 简洁版）", () => {
    const { container } = render(<RepoDropdown {...base} />);
    expect(container.querySelector(".dd-section-title")).toBeNull();
    expect(container.querySelector(".dd-foot")).toBeNull();
  });

  // R3
  it("R3 外点关闭：mousedown outside → onClose", () => {
    const onClose = vi.fn();
    render(<RepoDropdown {...base} onClose={onClose} />);
    fireEvent.mouseDown(document.body);
    expect(onClose).toHaveBeenCalledTimes(1);
  });

  it("R3 Esc 关闭：keydown Escape → onClose", () => {
    const onClose = vi.fn();
    render(<RepoDropdown {...base} onClose={onClose} />);
    fireEvent.keyDown(document, { key: "Escape" });
    expect(onClose).toHaveBeenCalledTimes(1);
  });
});

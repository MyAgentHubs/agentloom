import { describe, it, expect } from "vitest";
import { render } from "@testing-library/react";
import { NamespaceAvatar } from "./NamespaceAvatar";
import type { NamespaceMeta } from "../types/agent";

const gh = (id: string, name: string): NamespaceMeta =>
  ({ id, name, kind: "github_org" }) as NamespaceMeta;

describe("NamespaceAvatar", () => {
  it("Local（null）→ folder-git 图标·无 provider 角标", () => {
    const { container } = render(<NamespaceAvatar namespace={null} />);
    expect(container.querySelector(".ns-av--loc")).not.toBeNull();
    expect(container.querySelector(".ns-av--loc svg")).not.toBeNull();
    expect(container.querySelector(".ns-av__badge")).toBeNull();
  });
  it("github_org → 首字母色块 + GitHub 角标", () => {
    const { container, getByText } = render(
      <NamespaceAvatar namespace={gh("org-1", "impanda-cookie")} />,
    );
    expect(container.querySelector(".ns-av__sq")).not.toBeNull();
    expect(getByText("I")).not.toBeNull();
    expect(container.querySelector(".ns-av__badge--gh")).not.toBeNull();
  });
  it("不同 org 保留各自身份首字母（不被 provider 抹平）", () => {
    const { getByText: g1 } = render(
      <NamespaceAvatar namespace={gh("org-1", "impanda-cookie")} />,
    );
    const { getByText: g2 } = render(
      <NamespaceAvatar namespace={gh("org-2", "MyAgentHubs")} />,
    );
    expect(g1("I")).not.toBeNull();
    expect(g2("M")).not.toBeNull();
  });
});

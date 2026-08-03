import type { NamespaceMeta } from "../types/agent";

type Props = { namespace: NamespaceMeta | null; size?: number };

function FolderGit({ s }: { s: number }) {
  return (
    <svg
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth={1.8}
      strokeLinecap="round"
      strokeLinejoin="round"
      width={s}
      height={s}
    >
      <path d="M3 7a2 2 0 0 1 2-2h3l2 2h7a2 2 0 0 1 2 2v7a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z" />
      <circle cx="12" cy="13" r="1.7" fill="currentColor" stroke="none" />
    </svg>
  );
}

const githubMark = (
  <svg
    viewBox="0 0 24 24"
    width="100%"
    height="100%"
    fill="currentColor"
    aria-hidden="true"
  >
    <path d="M12 .297c-6.63 0-12 5.373-12 12 0 5.303 3.438 9.8 8.205 11.385.6.113.82-.258.82-.577 0-.285-.01-1.04-.015-2.04-3.338.724-4.042-1.61-4.042-1.61C4.422 18.07 3.633 17.7 3.633 17.7c-1.087-.744.084-.729.084-.729 1.205.084 1.838 1.236 1.838 1.236 1.07 1.835 2.809 1.305 3.495.998.108-.776.417-1.305.76-1.605-2.665-.305-5.467-1.334-5.467-5.931 0-1.311.469-2.381 1.236-3.221-.124-.303-.535-1.524.117-3.176 0 0 1.008-.322 3.301 1.23A11.5 11.5 0 0 1 12 5.803c1.02.005 2.047.138 3.006.404 2.291-1.552 3.297-1.23 3.297-1.23.653 1.653.242 2.874.118 3.176.77.84 1.235 1.911 1.235 3.221 0 4.609-2.807 5.624-5.479 5.921.43.372.823 1.102.823 2.222 0 1.606-.014 2.898-.014 3.293 0 .322.216.694.825.576C20.565 22.092 24 17.592 24 12.297c0-6.627-5.373-12-12-12" />
  </svg>
);

function letterClass(ns: NamespaceMeta): string {
  const last = ns.id.charCodeAt(ns.id.length - 1);
  return last % 2 === 0 ? "ns-av__sq--p1" : "ns-av__sq--p2";
}

/**
 * namespace 头像（spec §2.E·UX round-2 方案 B）：
 *  - Local → folder-git 线性图标（本机仓库集合）。
 *  - github_org → 首字母色块（保 org 身份·不同 org 一眼不同）+ 右下角单色 GitHub 角标（provider 维度）。
 *  - 未来 gitlab → 首字母 + GitLab 角标（角标待补·当前无 gitlab namespace）。
 * provider 徽标单色克制（不用品牌色·避免在暖米/暖橙体系抢注意力）。
 */
export function NamespaceAvatar({ namespace, size = 18 }: Props) {
  if (!namespace || namespace.kind === "local") {
    return (
      <span
        className="ns-av ns-av--loc"
        style={{ width: size, height: size }}
        aria-hidden="true"
      >
        <FolderGit s={Math.round(size * 0.92)} />
      </span>
    );
  }
  const isGithub = namespace.kind === "github_org";
  const letter = (namespace.name.slice(0, 1) || "?").toUpperCase();
  return (
    <span
      className="ns-av"
      style={{ width: size, height: size }}
      aria-hidden="true"
    >
      <span className={`ns-av__sq ${letterClass(namespace)}`}>{letter}</span>
      {isGithub && (
        <span className="ns-av__badge ns-av__badge--gh">{githubMark}</span>
      )}
    </span>
  );
}

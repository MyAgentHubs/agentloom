import type { CSSProperties, ReactNode } from "react";

type Kind = "claude" | "codex" | "deepseek" | "glm" | "kimi" | "user" | string;

const GLYPHS: Record<string, ReactNode> = {
  claude: (
    <svg viewBox="0 0 24 24" fill="currentColor" aria-hidden="true">
      <path d="M12 2l1.6 6.4L20 6l-4.4 5.2L22 14l-6.6-.4L17 20l-5-4-5 4 1.6-6.4L2 14l6.4-2.8L4 6l6.4 2.4z" />
    </svg>
  ),
  codex: (
    <svg
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="1.6"
      aria-hidden="true"
    >
      <path d="M12 5a3.2 3.2 0 013.1 4 3.2 3.2 0 011.9 5.6 3.2 3.2 0 01-5 1.4 3.2 3.2 0 01-6-1.4A3.2 3.2 0 017.9 9 3.2 3.2 0 0112 5z" />
    </svg>
  ),
  deepseek: (
    <svg viewBox="0 0 24 24" fill="currentColor" aria-hidden="true">
      <path d="M3 13c3 0 4-2 7-2s4 3 8 1c0 0-1 5-7 5-5 0-8-4-8-4z" />
    </svg>
  ),
  user: (
    <svg viewBox="0 0 24 24" fill="currentColor" aria-hidden="true">
      <circle cx="12" cy="8" r="4" />
      <path d="M4 21c0-4 4-6 8-6s8 2 8 6z" />
    </svg>
  ),
};

const STYLES: Record<string, CSSProperties> = {
  glm: { background: "#7c3aed" },
  kimi: { background: "#8b5cf6" },
};

const TEXT: Record<string, string> = {
  glm: "G",
  kimi: "K",
};

function resolveKind(kind: string): string | null {
  const normalized = kind.toLowerCase();
  if (GLYPHS[normalized] || STYLES[normalized]) return normalized;
  if (normalized.includes("claude")) return "claude";
  if (normalized.includes("codex")) return "codex";
  if (normalized.includes("deepseek")) return "deepseek";
  if (
    normalized.includes("glm") ||
    normalized.includes("zhipu") ||
    normalized.includes("bigmodel") ||
    normalized.includes("z.ai")
  )
    return "glm";
  if (normalized.includes("kimi")) return "kimi";
  return null;
}

export function AgentAvatar({ kind }: { kind: Kind }) {
  const avatarKind = resolveKind(kind);
  const glyph = avatarKind ? GLYPHS[avatarKind] : undefined;
  const cls = avatarKind ?? "unknown";

  return (
    <span
      className={`agent-avatar agent-avatar--${cls}`}
      style={avatarKind ? STYLES[avatarKind] : undefined}
    >
      {glyph ?? TEXT[cls] ?? kind.slice(0, 1).toUpperCase()}
    </span>
  );
}

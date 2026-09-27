import { PROVIDER_PRESETS } from "./agentFormPresets";

import type { AccessMode, ProviderId } from "./agentFormPresets";

export type EngineId = "claude-code" | "codex" | "myagent";
export type PresetMeta = { id: ProviderId; label: string; access: AccessMode };
export type EngineViewEntry = {
  engine: EngineId;
  label: string;
  desc: string;
  groups: Array<{ kind: "account" | "api_key"; presets: PresetMeta[] }>;
};

const providerMeta = (id: ProviderId): PresetMeta => {
  const preset = PROVIDER_PRESETS.find((provider) => provider.id === id);
  if (!preset) throw new Error(`Missing provider preset: ${id}`);
  return preset;
};

export function engineView(): EngineViewEntry[] {
  return [
    {
      engine: "claude-code",
      label: "Claude Code CLI",
      desc: "本机 claude 命令。可跑 Anthropic 自家，也可借壳跑别家",
      groups: [
        { kind: "account", presets: [providerMeta("claude")] },
        {
          kind: "api_key",
          presets: [
            providerMeta("deepseek"),
            providerMeta("kimi"),
            providerMeta("zhipu"),
            providerMeta("custom"),
          ],
        },
      ],
    },
    {
      engine: "codex",
      label: "Codex CLI",
      desc: "本机 codex 命令。跑 OpenAI 自家模型",
      groups: [
        { kind: "account", presets: [providerMeta("codex")] },
        { kind: "api_key", presets: [] },
      ],
    },
    {
      engine: "myagent",
      label: "myagent",
      desc: "自研 harness，直连各家 API",
      groups: [
        {
          kind: "api_key",
          presets: [
            providerMeta("harness-deepseek"),
            providerMeta("harness-gemini"),
            providerMeta("harness-glm"),
            providerMeta("harness-kimi"),
          ],
        },
      ],
    },
  ];
}

export function autoAgentName(
  presetId: ProviderId,
  accessPointId?: string,
): string {
  const preset = PROVIDER_PRESETS.find((provider) => provider.id === presetId);
  if (!preset) return presetId;

  if (preset.access === "native") return preset.label;
  if (preset.access === "harness") return `${preset.label}（myagent）`;

  if (preset.accessPoints.length > 1 && accessPointId) {
    const accessPoint = preset.accessPoints.find(
      (candidate) => candidate.id === accessPointId,
    );
    if (accessPoint) {
      return `${preset.label} ${accessPoint.label}（Claude Code 借壳）`;
    }
  }
  return `${preset.label}（Claude Code 借壳）`;
}

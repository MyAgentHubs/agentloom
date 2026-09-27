export type ReasoningTier =
  | "auto"
  | "none"
  | "minimal"
  | "low"
  | "medium"
  | "high"
  | "xhigh"
  | "max";

export type ComposerRuntimeConfig = {
  reasoningTier?: ReasoningTier;
};

export interface AgentProfile {
  id: string;
  name: string;
  access: string;
  provider: string;
  primary_model: string | null;
  endpoint: string | null;
  auth_mode: string | null;
  model_opus: string | null;
  model_sonnet: string | null;
  model_haiku: string | null;
  model_subagent: string | null;
  reasoning_default: string;
  max_output_tokens: number | null;
  api_timeout_ms: number | null;
  compat_disable_betas: boolean;
  compat_disable_nonessential: boolean;
  compat_disable_thinking: boolean;
  compat_proxy: string | null;
  custom_headers: string | null;
  extra_body: string | null;
  cap_reasoning: string | null;
  cap_computer_use: string | null;
  cap_lead: string | null;
  has_key: boolean;
  is_builtin: boolean;
  enabled: boolean;
  sort_order: number;
  created_at: number;
  updated_at: number;
}

export type ConnectionTestResult = {
  ok: boolean;
  category: string | null;
  raw_error: string | null;
};

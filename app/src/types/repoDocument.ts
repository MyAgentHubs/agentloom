export type GeneratedDocumentView = {
  repo_id: string;
  content: string;
  generated_at: number;
  head_sha: string;
  stale: boolean;
};

export type GenerationRun = {
  run_id: string;
};

export type GenerationDocument = {
  repo_id: string;
  content: string;
  generated_at: number;
  head_sha: string;
};

export type GenerationEvent = {
  feature: string;
  phase: string;
  repo_id: string;
  run_id: string;
  delta?: string;
  document?: GenerationDocument;
  message?: string;
};

export type UndoDiffLine = {
  kind: "ctx" | "add" | "del";
  text: string;
  oldLine: number | null;
  newLine: number | null;
};

export type UndoDiffResult = {
  lines: UndoDiffLine[];
  insertions: number;
  deletions: number;
};

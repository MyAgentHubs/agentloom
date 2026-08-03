import type { GateDraft } from "./gateReducer";
import type { DraftFailure } from "../types/gate";

export type GateView =
  | { kind: "proposing" }
  | { kind: "draft"; draft: GateDraft }
  | {
      kind: "failed";
      failure: DraftFailure;
      runId: string;
      contractId: string;
    };

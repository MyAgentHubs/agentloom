import { useEffect } from "react";
import type { EventStorePort } from "../store/port.ts";
import { parseFrame } from "../events/parseFrame.ts";
import { applyDecryptedMilestoneFrame, type AppRuntimeCore } from "./appRuntimeCore.ts";

// Caller must keep core and forceRender stable across renders; omitted effect dependencies assume stable references.
interface ColdReplayParams {
  eventStore: EventStorePort;
  core: AppRuntimeCore;
  forceRender: () => void;
}

export function useAppRuntimeColdReplay({ eventStore, core, forceRender }: ColdReplayParams) {
  // -------------------------------------------------------------------------
  // Cold-start replay (INT1c P0: replay all five milestones plus session.index,
  // instead of replaying only session.index).
  // -------------------------------------------------------------------------
  /**
   * Once `store/port.ts::StoredEvent.session` is persisted with the envelope in
   * the same transaction (INT1c), cold-start replay can safely reconstruct more
   * than `session.index`. `row.session ?? null` maps both explicit null (a real
   * session.index row) and a missing/undefined field (a row stored before this
   * field existed, at the migration boundary) to null for
   * `applyDecryptedMilestoneFrame`. That argument does not affect session.index
   * frames, which are always reconstructed. For the other five milestone types,
   * the function's existing rejection rule blocks `envelopeSession===null`.
   * Old and new rows without session ownership therefore follow the same safe
   * path, with no separate migration branch.
   */
  useEffect(() => {
    let cancelled = false;
    void eventStore.listEvents().then((rows) => {
      if (cancelled) return;
      for (const row of rows) {
        if (cancelled) return;
        const parsed = parseFrame(row.frame);
        if (!parsed.ok) continue;
        applyDecryptedMilestoneFrame(core, row.session ?? null, parsed.frame);
      }
      if (!cancelled) forceRender();
    });
    return () => {
      cancelled = true;
    };
    // core is a stable ref and forceRender is a stable dispatch; neither belongs in deps.
  }, [eventStore]);

}

import { useCallback, useEffect, useRef, useState, type RefObject } from "react";
import type { BodyCachePort } from "../store/bodyCache.ts";
import { deriveBodyCacheDbName, IndexedDbBodyCache } from "../store/bodyCache.indexeddb.ts";
import { withMemoryFallback } from "../store/bodyCache.ts";
import { loadCacheEnabledPreference, saveCacheEnabledPreference } from "../ui/settings/cachePreference.ts";
import { MsgFetchClient } from "../events/msgFetch.ts";
import type { WebSocketLike } from "../connection/types.ts";
import { getOrCreateSessionProjection, type AppRuntimeCore } from "./appRuntimeCore.ts";
import type { CommandChannel } from "./commandChannel.ts";

export interface UseAppRuntimeMsgFetchParams {
  room: string;
  kRoomKey: CryptoKey;
  bodyCache: BodyCachePort | undefined;
  commandChannel: CommandChannel;
  core: AppRuntimeCore;
  forceRender: () => void;
  currentEpochRef: RefObject<number | null>;
  currentSocketRef: RefObject<WebSocketLike | null>;
}

export function useAppRuntimeMsgFetch({
  room,
  kRoomKey,
  bodyCache,
  commandChannel,
  core,
  forceRender,
  currentEpochRef,
  currentSocketRef,
}: UseAppRuntimeMsgFetchParams) {
  // -------------------------------------------------------------------------
  // Use a room-specific IndexedDB body cache when none is provided, with one memory
  // fallback wrapper for transaction failures. RootRouter supplies the production
  // cache after probing IndexedDB; this default supports tests and direct mounts.
  // -------------------------------------------------------------------------
  const bodyCacheRef = useRef<BodyCachePort | null>(null);
  if (!bodyCacheRef.current) {
    bodyCacheRef.current = bodyCache ?? withMemoryFallback(new IndexedDbBodyCache(deriveBodyCacheDbName(room)));
  }
  const bodyCacheInstance = bodyCacheRef.current;

  // The device-wide setting follows the verbose preference. Turning it off stops
  // cache writes and clears stored bodies. The fetch client's callback is fixed at
  // construction, so it reads the current setting from a ref instead of a stale closure.
  const [cacheEnabled, setCacheEnabled] = useState(() => loadCacheEnabledPreference());
  const cacheEnabledRef = useRef(cacheEnabled);
  cacheEnabledRef.current = cacheEnabled;
  const toggleCacheEnabled = useCallback(
    (next: boolean) => {
      setCacheEnabled(next);
      // A state update does not flush the next render immediately. Update the ref
      // synchronously so callbacks in this click's call chain see the new setting.
      cacheEnabledRef.current = next;
      saveCacheEnabledPreference(next);
      if (!next) {
        // Wait for the clear inside the async task and report failures. The setting
        // and visual feedback update immediately, while a failed or interrupted clear
        // is retried by the startup backstop on the next mount.
        void (async () => {
          try {
            await bodyCacheInstance.clear();
          } catch (error) {
            console.error("msgfix2 U4 H5: cache toggle-off clear failed", error);
          }
        })();
      }
    },
    [bodyCacheInstance],
  );

  // If the saved setting is off, clear on mount again: a previous toggle may have
  // been interrupted or its transaction may have failed. Clear is cheap on an empty
  // cache, and this makes the stored bodies converge to the saved preference.
  const cacheBackstopRanRef = useRef(false);
  useEffect(() => {
    if (cacheBackstopRanRef.current) return;
    cacheBackstopRanRef.current = true;
    if (cacheEnabledRef.current) return;
    void bodyCacheInstance.clear().catch((error) => {
      console.error("msgfix2 U4 H5: startup body cache backstop clear failed", error);
    });
  }, [bodyCacheInstance]);

  // -------------------------------------------------------------------------
  // Full-text fetch client — `msg.fetch` reuses the existing control channel (same local
  // sliding window as `commandChannel.trySendControlSlot()`); `msg.chunk`/`msg.fetch.error` arrive
  // on the new `reply` kind, routed by AppRuntime's `processFrame`.
  // -------------------------------------------------------------------------
  const msgFetchClientRef = useRef<MsgFetchClient | null>(null);
  if (!msgFetchClientRef.current) {
    msgFetchClientRef.current = new MsgFetchClient({
      room,
      kRoomKey,
      getEpoch: () => currentEpochRef.current,
      getSocket: () => currentSocketRef.current,
      trySendControlSlot: (commandId, session) => commandChannel.trySendControlSlot(commandId, session, "msg.fetch"),
      applyFullText: (sessionId, messageId, revision, blocks) => {
        getOrCreateSessionProjection(core, sessionId).applyFullText(messageId, revision, blocks);
      },
      // Apply full text to the in-memory projection first, then write the cache
      // asynchronously without delaying display. The memory fallback handles storage
      // errors; this catch preserves the silent cache-write failure contract.
      onFetchCached: (input) => {
        if (!cacheEnabledRef.current) return;
        void bodyCacheInstance
          .put({ room: input.room, session: input.sessionId, messageId: input.messageId, contentSha256: input.contentSha256 }, input.blocks, input.bytes)
          .catch(() => {});
      },
      onChange: forceRender,
    });
  }
  const msgFetchClient = msgFetchClientRef.current;

  // Look up the cache before fetching full text. While a lookup is pending, block
  // another fetch for the same message. Both automatic and manual loads use this
  // entry point, so a cache hit avoids either kind of network request.
  const cacheLookupPendingRef = useRef<Set<number>>(new Set());
  const loadFullTextViaCacheOrFetch = useCallback(
    (sessionId: string, messageId: number, revision: number, contentSha256: string | undefined, staleRetried = false) => {
      if (cacheLookupPendingRef.current.has(messageId)) return; // Block duplicates while lookup is pending.
      // A fetch for this message is already in flight (e.g. dispatched by the stale-revision
      // fallback branch below) — skip the now-moot cache lookup.
      if (msgFetchClient.getState(messageId).status === "loading") return;
      if (!cacheEnabledRef.current) {
        // Disabling the cache stops reads as well as writes. Clearing is asynchronous,
        // so leftover entries may still exist; fetch from the network instead.
        void msgFetchClient.startFetch(sessionId, messageId, revision);
        return;
      }
      if (contentSha256 === undefined) {
        // A content reference should carry a hash; fall back to the network if it does not.
        void msgFetchClient.startFetch(sessionId, messageId, revision);
        return;
      }
      cacheLookupPendingRef.current.add(messageId);
      void bodyCacheInstance
        .get({ room, session: sessionId, messageId, contentSha256 })
        .catch(() => null)
        .then(async (cached) => {
          cacheLookupPendingRef.current.delete(messageId);
          if (cached) {
            const applied = getOrCreateSessionProjection(core, sessionId).applyFullText(messageId, revision, cached.blocks);
            if (applied) {
              forceRender();
              return;
            }
            // A newer completion frame may have advanced the projection's revision
            // during the lookup. The rejected entry is then a known stale orphan,
            // so delete it rather than waiting for LRU eviction.
            //
            // Retry against the fresh content reference at most once. Repeated
            // revision advances could otherwise cause an unbounded lookup/delete loop.
            // Decide and dispatch immediately after releasing the pending-lookup lock,
            // before any await. Otherwise the automatic load effect can observe an
            // idle client and start a third lookup in that gap. Delete the orphan only
            // after dispatch; awaiting deletion must not delay the branch decision.
            const freshMsg = core.sessionProjections.get(sessionId)?.messages.get(messageId);
            if (!staleRetried && freshMsg?.contentRef !== undefined) {
              loadFullTextViaCacheOrFetch(sessionId, messageId, freshMsg.contentRef.revision, freshMsg.contentRef.content_sha256, true);
              forceRender();
              // Await orphan deletion and log failure. LRU eviction can eventually
              // remove it, but a failed deletion should be visible.
              try {
                await bodyCacheInstance.delete({ room, session: sessionId, messageId, contentSha256 });
              } catch (error) {
                console.error("msgfix2 U4 I5: stale cache orphan delete failed", error);
              }
              return;
            }
            // Re-lock through the whole fallback: dispatch, then the orphan delete below, then
            // release in the finally block. The finally also covers the case where startFetch
            // itself rejects — it currently never does (it swallows its own errors internally),
            // but that is an implicit contract this lock should not silently depend on.
            cacheLookupPendingRef.current.add(messageId);
            try {
              await msgFetchClient.startFetch(sessionId, messageId, freshMsg?.contentRef?.revision ?? revision);
              forceRender();
              // Await orphan deletion and log failure. LRU eviction can eventually
              // remove it, but a failed deletion should be visible.
              try {
                await bodyCacheInstance.delete({ room, session: sessionId, messageId, contentSha256 });
              } catch (error) {
                console.error("msgfix2 U4 I5: stale cache orphan delete failed", error);
              }
            } finally {
              cacheLookupPendingRef.current.delete(messageId);
            }
            return;
          }
          // On a miss, recheck projection and client state: another path may have
          // finished loading during the lookup. Block only an in-flight fetch;
          // an error is terminal and the manual retry must be allowed through.
          const proj = core.sessionProjections.get(sessionId);
          const msg = proj?.messages.get(messageId);
          if (msg && msg.fullBlocks !== undefined) return;
          if (msgFetchClient.getState(messageId).status === "loading") return;
          void msgFetchClient.startFetch(sessionId, messageId, revision);
        });
    },
    [bodyCacheInstance, room, core, msgFetchClient, forceRender],
  );

  return { cacheEnabled, toggleCacheEnabled, msgFetchClient, loadFullTextViaCacheOrFetch };
}

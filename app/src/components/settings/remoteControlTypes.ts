export type RemoteControlSettings = {
  enabled: boolean;
  /** Stored raw value; empty means no custom relay, so the gateway and pairing use the official public relay. */
  relay_url: string;
  /** Official public relay used when relay is empty (always the backend `DEFAULT_PUBLIC_RELAY_URL` constant). */
  default_relay_url: string;
  /** `None` (serialized as `null` from the backend `Option<String>`) means no active project is set. */
  active_repo_id: string | null;
};

export type RemotePairingPayload = {
  v: number;
  relay_url: string;
  room: string;
  pairing_token: string;
  desktop_pub: string;
};

export type RemotePairingStatus =
  | { state: "Idle" }
  | { state: "WaitingForHello"; expires_at: number }
  | { state: "Done"; device_id: string };

export type RemoteGatewayStatus = {
  running: boolean;
  stopped_reason: string | null;
  /** Optional while a newer frontend is briefly paired with an older desktop IPC. */
  last_error?: string | null;
  counters?: Partial<GatewayCounters>;
};

export const GATEWAY_COUNTER_NAMES = [
  "frames_seen",
  "frames_sent",
  "keepalive_pings_sent",
  "bad_frames",
  "upstream_dropped",
  "upstream_stale_generation_dropped",
  "upstream_budget_dropped",
  "milestone_dropped",
  "session_index_snapshot_unavailable",
  "snapshot_worker_spawn_count",
  "tool_correlation_dropped",
  "classify_skipped",
  "connection_failures",
  "panics",
  "disconnect_config_stale",
  "disconnect_closed_by_peer",
  "disconnect_error",
  "upstream_repo_filtered",
  "partial_snapshot_capacity_dropped",
  "snapshot_oversized_dropped",
] as const;

export type GatewayCounters = Record<
  (typeof GATEWAY_COUNTER_NAMES)[number],
  number
> & {
  last_disconnect_reason: string;
};

export type RemoteDeviceView = {
  device_id: string;
  name: string;
  created_at: number;
  access_expires_at: number;
  revoked_at: number | null;
};

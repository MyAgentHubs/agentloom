// Shared WebSocket attachment reader for RoomDO modules.

export function safeAttachment(ws) {
  try {
    return ws.deserializeAttachment() || {};
  } catch {
    return {};
  }
}

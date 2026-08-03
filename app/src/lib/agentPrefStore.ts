const KEY = "agentloom.lastAgentId";

export function loadLastAgentId(): string | null {
  try {
    return localStorage.getItem(KEY);
  } catch {
    return null;
  }
}

export function saveLastAgentId(id: string): void {
  try {
    localStorage.setItem(KEY, id);
  } catch {
    // non-browser or storage error — silently ignore
  }
}

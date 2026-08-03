export type MemberIdlePollDeps = {
  checkRunning: () => Promise<unknown>;
  onIdle: () => void;
  intervalMs?: number;
  setTimer?: typeof setTimeout;
  clearTimer?: typeof clearTimeout;
};

const DEFAULT_INTERVAL_MS = 15_000;

export function startMemberIdlePoll(deps: MemberIdlePollDeps): () => void {
  const intervalMs = deps.intervalMs ?? DEFAULT_INTERVAL_MS;
  const setTimer = deps.setTimer ?? setTimeout;
  const clearTimer = deps.clearTimer ?? clearTimeout;
  let stopped = false;
  let timer: ReturnType<typeof setTimeout> | undefined;

  const stop = () => {
    if (stopped) return;
    stopped = true;
    if (timer !== undefined) {
      clearTimer(timer);
      timer = undefined;
    }
  };

  const scheduleNext = () => {
    if (stopped) return;
    timer = setTimer(() => {
      timer = undefined;
      void check();
    }, intervalMs);
  };

  const check = async () => {
    try {
      const running = await deps.checkRunning();
      if (stopped) return;
      if (running === false) {
        stop();
        deps.onIdle();
        return;
      }
    } catch {
      if (stopped) return;
    }
    scheduleNext();
  };

  void check();
  return stop;
}

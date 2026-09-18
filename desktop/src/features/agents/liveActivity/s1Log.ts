/**
 * Gate-run logging (Slice 1/2 shared helper).
 *
 * Set `localStorage["s1-gate"]="1"` to enable. Lines go to an in-memory ring
 * (`window.__s1Log`) that the gate script reads AFTER the run, so no debugger
 * has to be attached while measuring (React's DEV render profiler + an attached
 * debugger were found to block the main thread for seconds — see
 * SLICE-1-VERIFICATION.md "profile" section).
 *
 * Extracted from `LiveActivityTimeline.tsx` for reuse by `AgentControlsBar`
 * and other Slice 2 components (§6.6, §6.1).
 */

const MAX_LOG_LINES = 20000;
const EVICT_COUNT = 5000;

export function s1GateEnabled(): boolean {
  try {
    return (
      typeof window !== "undefined" &&
      window.localStorage.getItem("s1-gate") === "1"
    );
  } catch {
    return false;
  }
}

export function s1GateLog(line: string): void {
  const w = window as unknown as { __s1Log?: string[] };
  if (!w.__s1Log) w.__s1Log = [];
  w.__s1Log.push(`${new Date().toISOString()} ${line}`);
  if (w.__s1Log.length > MAX_LOG_LINES) {
    w.__s1Log.splice(0, EVICT_COUNT);
  }
  if (import.meta.env?.DEV) console.debug(line);
}
/**
 * Legacy browser marker retained only for compatibility tests and migration
 * diagnostics. Production archive code does not read or write this value.
 * SQLite `save_subscriptions` is the sole observer-archive consent authority.
 *
 * The key is identity-scoped so toggling off on one identity doesn't suppress
 * the choice for another identity. The value is:
 *   "1"  → user explicitly enabled
 *   "0"  → user explicitly disabled
 *   null → no explicit choice yet (archive remains off)
 *
 * Older builds kept this marker in device-level localStorage. Current startup
 * and settings code never consult it; these helpers remain isolated here only
 * so compatibility tests can model stale browser data.
 *
 * Storage-error contract: a read that throws returns `false`. Retention must
 * fail closed: inability to prove opt-in can never enable local archiving.
 */

const KEY_PREFIX = "buzz:observer-archive-default-seeded";

function storageKey(identityPubkey: string): string {
  return `${KEY_PREFIX}:${identityPubkey}`;
}

/**
 * Reads a legacy stored choice for this identity in one localStorage access.
 *
 * Returns:
 *   `false`    — user opted out ("0") or stored data is unrecognized
 *   `true`     — user explicitly opted in ("1" stored)
 *   `"unset"`  — no choice recorded yet
 *
 * On storage error, returns `false` so retention remains disabled.
 *
 * @deprecated SQLite `save_subscriptions` is the only production authority.
 */
export function readExplicitObserverArchiveChoice(
  identityPubkey: string,
): boolean | "unset" {
  if (typeof window === "undefined") return false;
  try {
    const raw = window.localStorage.getItem(storageKey(identityPubkey));
    if (raw === null) return "unset";
    if (raw === "1") return true;
    return false;
  } catch {
    return false;
  }
}

/**
 * Writes a legacy choice marker for compatibility tests.
 *
 * @deprecated SQLite `save_subscriptions` is the only production authority.
 */
export function setExplicitObserverArchiveChoice(
  identityPubkey: string,
  enabled: boolean,
): void {
  if (typeof window === "undefined") return;
  try {
    window.localStorage.setItem(
      storageKey(identityPubkey),
      enabled ? "1" : "0",
    );
  } catch {
    // Best-effort legacy marker only. Production consent is held in SQLite.
  }
}

/**
 * Clears a legacy choice marker for compatibility tests.
 *
 * @deprecated SQLite `save_subscriptions` is the only production authority.
 */
export function clearExplicitObserverArchiveChoice(
  identityPubkey: string,
): void {
  if (typeof window === "undefined") return;
  try {
    window.localStorage.removeItem(storageKey(identityPubkey));
  } catch {
    // ignore
  }
}

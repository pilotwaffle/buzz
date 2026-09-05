import assert from "node:assert/strict";
import test from "node:test";

import {
  isReconciledFor,
  reconcileObserverArchive,
  startReconciliation,
} from "./useObserverArchiveSeed.ts";

// ── Fake deps factory ────────────────────────────────────────────────────────

function makeDeps({ verifyShouldFail = false } = {}) {
  const calls = { verify: 0 };
  return {
    calls,
    verifyArchiveStore: async () => {
      calls.verify += 1;
      if (verifyShouldFail) throw new Error("authority store unavailable");
    },
  };
}

/** Helper: wait for microtasks/promises to settle */
function tick() {
  return new Promise((r) => setTimeout(r, 0));
}

// ── Fresh identities default off ─────────────────────────────────────────────

test("test_reconcile_fresh_identity_does_not_seed_24200", async () => {
  const deps = makeDeps();
  await reconcileObserverArchive("pk1", deps);

  assert.equal(deps.calls.verify, 1);
});

// ── Failure behavior ─────────────────────────────────────────────────────────

test("test_authority_store_failure_rejects", async () => {
  const deps = makeDeps({ verifyShouldFail: true });

  await assert.rejects(() => reconcileObserverArchive("pk1", deps), {
    message: "authority store unavailable",
  });
});

// ── SQLite is the sole consent authority ────────────────────────────────────

test("test_reconcile_only_verifies_store_and_never_mutates_consent", async () => {
  const deps = makeDeps();
  await reconcileObserverArchive("pk1", deps);
  await reconcileObserverArchive("pk1", deps);

  assert.equal(deps.calls.verify, 2);
  assert.deepEqual(
    Object.keys(deps).sort(),
    ["calls", "verifyArchiveStore"],
    "startup has no browser-choice or subscription-mutation capability",
  );
});

// ── Startup ordering ─────────────────────────────────────────────────────────
//
// The two ordering tests that lived here drove `ArchiveSyncManager` directly.
// That manager is gone: archive sync runs in Rust and the renderer keeps only
// the start gate. The same invariant — no listener opens before the authority
// store has been verified — is now asserted against the real gate in
// useArchiveSync.test.mjs.

// ── Identity-scoped readiness (exercises exported isReconciledFor) ──────────

test("test_isReconciledFor_null_returns_false", () => {
  assert.equal(isReconciledFor(null, "pk1"), false);
  assert.equal(isReconciledFor(null, undefined), false);
});

test("test_isReconciledFor_undefined_pubkey_returns_false", () => {
  assert.equal(isReconciledFor("pk1", undefined), false);
});

test("test_isReconciledFor_matching_returns_true", () => {
  assert.equal(isReconciledFor("pk1", "pk1"), true);
});

test("test_isReconciledFor_mismatch_returns_false", () => {
  assert.equal(isReconciledFor("pkA", "pkB"), false);
});

test("test_identity_change_resets_readiness", async () => {
  let reconciledPubkey = null;

  // Identity A reconciles successfully.
  const depsA = makeDeps();
  await reconcileObserverArchive("pkA", depsA);
  reconciledPubkey = "pkA";
  assert.equal(
    isReconciledFor(reconciledPubkey, "pkA"),
    true,
    "A is reconciled",
  );

  // Identity changes to B — gate must be false before B reconciles.
  assert.equal(
    isReconciledFor(reconciledPubkey, "pkB"),
    false,
    "gate must be false for new identity before reconciliation",
  );

  // B reconciles successfully.
  const depsB = makeDeps();
  await reconcileObserverArchive("pkB", depsB);
  reconciledPubkey = "pkB";
  assert.equal(
    isReconciledFor(reconciledPubkey, "pkB"),
    true,
    "B is reconciled",
  );
  assert.equal(
    isReconciledFor(reconciledPubkey, "pkA"),
    false,
    "gate must be false for previous identity",
  );
});

test("test_identity_change_b_failure_stays_closed", async () => {
  let reconciledPubkey = null;

  // Identity A reconciles successfully.
  const depsA = makeDeps();
  await reconcileObserverArchive("pkA", depsA);
  reconciledPubkey = "pkA";

  // Identity changes to B — B's reconciliation fails.
  const depsB = makeDeps({ verifyShouldFail: true });
  try {
    await reconcileObserverArchive("pkB", depsB);
    reconciledPubkey = "pkB";
  } catch {
    // B failed — reconciledPubkey stays "pkA" (stale).
  }

  // Gate for B must be false (stale A pubkey !== current B).
  assert.equal(
    isReconciledFor(reconciledPubkey, "pkB"),
    false,
    "gate must be false when B reconciliation fails",
  );
});

// ── startReconciliation lifecycle (cancellation guard) ──────────────────────
//
// These exercise the actual effect/cleanup code path extracted into
// `startReconciliation`, rather than only the pure `isReconciledFor` helper
// or manually-sequenced fakes. Mirrors what React calls on unmount / before
// re-running an effect with new deps (identity switch).

test("test_startReconciliation_calls_onReady_after_success", async () => {
  const deps = makeDeps();
  const readyCalls = [];

  startReconciliation("pk1", deps, (pubkey) => readyCalls.push(pubkey));
  await tick();

  assert.deepEqual(readyCalls, ["pk1"]);
  assert.equal(deps.calls.verify, 1);
});

test("test_startReconciliation_unmount_before_resolve_suppresses_onReady", async () => {
  let resolveVerify;
  const verifyPromise = new Promise((resolve) => {
    resolveVerify = resolve;
  });
  const deps = {
    verifyArchiveStore: () => verifyPromise,
  };
  const readyCalls = [];

  const cancel = startReconciliation("pk1", deps, (pubkey) =>
    readyCalls.push(pubkey),
  );

  // Unmount (or re-run effect) before the merge resolves.
  cancel();
  resolveVerify();
  await tick();

  assert.deepEqual(
    readyCalls,
    [],
    "onReady must not fire for a cancelled reconciliation",
  );
});

test("test_startReconciliation_identity_switch_stale_completion_suppressed", async () => {
  let resolveVerifyA;
  const verifyPromiseA = new Promise((resolve) => {
    resolveVerifyA = resolve;
  });
  const depsA = {
    verifyArchiveStore: () => verifyPromiseA,
  };
  const depsB = makeDeps();
  const readyCalls = [];
  const onReady = (pubkey) => readyCalls.push(pubkey);

  // Start reconciling for pkA (pending), then switch identity to pkB before
  // A resolves — this is exactly what the hook's effect does when `pubkey`
  // changes: it calls the previous effect's cleanup (cancelA) before
  // starting the new effect.
  const cancelA = startReconciliation("pkA", depsA, onReady);
  cancelA();
  startReconciliation("pkB", depsB, onReady);

  // A's store read now resolves late — its stale completion must not fire.
  resolveVerifyA();
  await tick();

  assert.deepEqual(
    readyCalls,
    ["pkB"],
    "only the current identity's completion should fire",
  );
});

test("test_startReconciliation_failure_does_not_call_onReady", async () => {
  const deps = makeDeps({ verifyShouldFail: true });
  const readyCalls = [];

  startReconciliation("pk1", deps, (pubkey) => readyCalls.push(pubkey));
  await tick();

  assert.deepEqual(readyCalls, [], "onReady must not fire on failure");
});

// ── Metric seed independence ─────────────────────────────────────────────────

test("test_observer_default_off_does_not_touch_metric_archive", async () => {
  const deps = makeDeps();
  await reconcileObserverArchive("pk1", deps);

  assert.equal(deps.calls.verify, 1);
});

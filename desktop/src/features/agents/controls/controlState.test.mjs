/**
 * controlState.test.mjs — T24
 *
 * Tests controlReducer transitions, single byte-identical retry at 3 s,
 * expiry at expires_at + 5 s, and ack-shape rejection.
 */
import assert from "node:assert/strict";
import test from "node:test";

// Fake-timer setup
let fakeNowMs = 0;
const origDateNow = Date.now;

function advanceMs(ms) {
  fakeNowMs += ms;
  Date.now = () => fakeNowMs;
}

function setEpoch(epochMs) {
  fakeNowMs = epochMs;
  Date.now = () => fakeNowMs;
}

test.before(() => {
  Date.now = () => fakeNowMs;
});

test.after(() => {
  Date.now = origDateNow;
});

test.beforeEach(() => {
  fakeNowMs = 0;
  Date.now = () => fakeNowMs;
});

// ── Bootstrap ────────────────────────────────────────────────────────────────

let createControlState, controlReducer, pendingForKind, pendingExpired, pendingRetry;

test("module loads", async () => {
  const mod = await import("./controlState.ts");
  createControlState = mod.createControlState;
  controlReducer = mod.controlReducer;
  pendingForKind = mod.pendingForKind;
  pendingExpired = mod.pendingExpired;
  pendingRetry = mod.pendingRetry;
  assert.ok(createControlState);
  assert.ok(controlReducer);
});

// ── Initial state ────────────────────────────────────────────────────────────

test("initial state has empty entries, running queue, nextSeq=1", () => {
  const s = createControlState();
  assert.deepStrictEqual(s.entries, []);
  assert.strictEqual(s.lease.queueState, "running");
  assert.strictEqual(s.lease.leaseId, null);
  assert.strictEqual(s.lease.generation, 0);
  assert.strictEqual(s.nextSeq, 1);
});

// ── control_sent ─────────────────────────────────────────────────────────────

test("control_sent inserts pending entry and bumps nextSeq", () => {
  const s = createControlState();
  const s2 = controlReducer(s, {
    type: "control_sent",
    kind: "cancel",
    id: "cmd-1",
    sentAt: 1800000000,
    expiresAt: 1800000030,
    sentEpochMs: 1000,
  });
  assert.strictEqual(s2.entries.length, 1);
  assert.strictEqual(s2.entries[0].kind, "cancel");
  assert.strictEqual(s2.entries[0].state, "pending");
  assert.strictEqual(s2.entries[0].retries, 1);
  assert.strictEqual(s2.nextSeq, 2);
});

// ── control_acked → acked ────────────────────────────────────────────────────

test("control_acked transitions pending to acked", () => {
  const s = createControlState();
  const s2 = controlReducer(s, {
    type: "control_sent",
    kind: "cancel",
    id: "cmd-1",
    sentAt: 1800000000,
    expiresAt: 1800000030,
    sentEpochMs: 1000,
  });
  const s3 = controlReducer(s2, {
    type: "control_acked",
    id: "cmd-1",
    ack: {
      ackId: "ack-1",
      status: "applied",
      ackedAt: 1800000001,
      receivedEpochMs: 1500,
    },
  });
  assert.strictEqual(s3.entries[0].state, "acked");
  assert.strictEqual(s3.entries[0].retries, 0);
  assert.strictEqual(s3.entries[0].ack.status, "applied");
});

// ── control_acked → rejected ─────────────────────────────────────────────────

test("control_acked with rejected status → rejected state", () => {
  const s = createControlState();
  const s2 = controlReducer(s, {
    type: "control_sent",
    kind: "pause",
    id: "tr-1",
    sentAt: 1800000000,
    expiresAt: 1800000060,
    sentEpochMs: 1000,
  });
  const s3 = controlReducer(s2, {
    type: "control_acked",
    id: "tr-1",
    ack: {
      ackId: "ack-2",
      status: "rejected",
      reason: "binding_mismatch",
      ackedAt: 1800000002,
      receivedEpochMs: 2000,
    },
  });
  assert.strictEqual(s3.entries[0].state, "rejected");
  assert.strictEqual(s3.entries[0].ack.reason, "binding_mismatch");
});

// ── Lease state from pause ack ───────────────────────────────────────────────

test("pause ack with queue_state=paused updates lease", () => {
  const s = createControlState();
  const s2 = controlReducer(s, {
    type: "control_sent",
    kind: "pause",
    id: "tr-pause-1",
    sentAt: 1800000000,
    expiresAt: 1800000060,
    sentEpochMs: 1000,
  });
  const s3 = controlReducer(s2, {
    type: "control_acked",
    id: "tr-pause-1",
    ack: {
      ackId: "ack-p1",
      status: "applied",
      ackedAt: 1800000001,
      receivedEpochMs: 1500,
    },
    leaseState: {
      leaseId: "lease-1",
      generation: 1,
      leaseExpiresAt: 1800000300,
      queueState: "paused",
    },
  });
  assert.strictEqual(s3.lease.leaseId, "lease-1");
  assert.strictEqual(s3.lease.generation, 1);
  assert.strictEqual(s3.lease.queueState, "paused");
});

// ── Resume ack clears lease ─────────────────────────────────────────────────

test("resume ack with queue_state=running clears lease hold", () => {
  const paused = {
    ...createControlState(),
    lease: {
      leaseId: "lease-1",
      generation: 1,
      leaseExpiresAt: 1800000300,
      queueState: "paused",
    },
  };
  const s2 = controlReducer(paused, {
    type: "control_sent",
    kind: "resume",
    id: "tr-resume-1",
    sentAt: 1800000002,
    expiresAt: 1800000062,
    sentEpochMs: 2000,
  });
  const s3 = controlReducer(s2, {
    type: "control_acked",
    id: "tr-resume-1",
    ack: {
      ackId: "ack-r1",
      status: "applied",
      ackedAt: 1800000003,
      receivedEpochMs: 2500,
    },
    leaseState: {
      leaseId: "lease-1",
      generation: 2,
      leaseExpiresAt: 1800000300,
      queueState: "running",
    },
  });
  assert.strictEqual(s3.lease.queueState, "running");
  assert.strictEqual(s3.lease.generation, 2);
});

// ── Retry ────────────────────────────────────────────────────────────────────

test("pending entries are retry-eligible after 3 s", () => {
  setEpoch(0);
  const s = createControlState();
  const s2 = controlReducer(s, {
    type: "control_sent",
    kind: "cancel",
    id: "cmd-1",
    sentAt: 1800000000,
    expiresAt: 1800000030,
    sentEpochMs: 0,
  });

  // Not eligible yet
  advanceMs(2500);
  let eligible = pendingRetry(s2, fakeNowMs);
  assert.strictEqual(eligible.length, 0);

  // Eligible at 3 s
  advanceMs(600); // → 3100 ms
  eligible = pendingRetry(s2, fakeNowMs);
  assert.strictEqual(eligible.length, 1);
  assert.strictEqual(eligible[0].id, "cmd-1");
});

test("control_retried decrements retries and updates lastSentEpochMs", () => {
  setEpoch(0);
  const s = createControlState();
  const s2 = controlReducer(s, {
    type: "control_sent",
    kind: "cancel",
    id: "cmd-1",
    sentAt: 1800000000,
    expiresAt: 1800000030,
    sentEpochMs: 0,
  });
  advanceMs(3000);
  const s3 = controlReducer(s2, {
    type: "control_retried",
    id: "cmd-1",
    sentEpochMs: 3000,
  });
  assert.strictEqual(s3.entries[0].retries, 0);
  assert.strictEqual(s3.entries[0].lastSentEpochMs, 3000);

  // No longer retry-eligible
  const eligible = pendingRetry(s3, fakeNowMs);
  assert.strictEqual(eligible.length, 0);
});

test("only one retry is performed — retries at 0 prevents further retry", () => {
  setEpoch(0);
  const s = createControlState();
  const s2 = controlReducer(s, {
    type: "control_sent",
    kind: "steer",
    id: "cmd-2",
    sentAt: 1800000000,
    expiresAt: 1800000060,
    sentEpochMs: 0,
  });
  // Already have 1 retry
  advanceMs(4000);
  const s3 = controlReducer(s2, {
    type: "control_retried",
    id: "cmd-2",
    sentEpochMs: 4000,
  });
  advanceMs(10000);
  const eligible = pendingRetry(s3, fakeNowMs);
  assert.strictEqual(eligible.length, 0);
});

// ── Expiry ───────────────────────────────────────────────────────────────────

test("pending entry expires at expires_at + 5 s", () => {
  setEpoch(0);
  const s = createControlState();
  const s2 = controlReducer(s, {
    type: "control_sent",
    kind: "cancel",
    id: "cmd-1",
    sentAt: 1800000000,
    expiresAt: 1800000030, // 30 s after issued
    sentEpochMs: 0,
  });

  // Now = 1800000030 + 4 → 34 s after epoch of expires_at, not expired yet
  const notExpired = pendingExpired(s2, (1800000030 + 4) * 1000);
  assert.strictEqual(notExpired.length, 0);

  // Now = 1800000030 + 5 → expired
  const expired = pendingExpired(s2, (1800000030 + 5) * 1000);
  assert.strictEqual(expired.length, 1);
  assert.strictEqual(expired[0].id, "cmd-1");
});

test("control_expired transitions pending to expired", () => {
  const s = createControlState();
  const s2 = controlReducer(s, {
    type: "control_sent",
    kind: "cancel",
    id: "cmd-1",
    sentAt: 1800000000,
    expiresAt: 1800000030,
    sentEpochMs: 0,
  });
  const s3 = controlReducer(s2, {
    type: "control_expired",
    id: "cmd-1",
    nowEpochMs: (1800000030 + 5) * 1000,
  });
  assert.strictEqual(s3.entries[0].state, "expired");
  assert.strictEqual(s3.entries[0].retries, 0);
});

test("control_expired does not affect already-acked entries", () => {
  const s = createControlState();
  const s2 = controlReducer(s, {
    type: "control_sent",
    kind: "cancel",
    id: "cmd-1",
    sentAt: 1800000000,
    expiresAt: 1800000030,
    sentEpochMs: 0,
  });
  const s3 = controlReducer(s2, {
    type: "control_acked",
    id: "cmd-1",
    ack: {
      ackId: "ack-1",
      status: "applied",
      ackedAt: 1800000001,
      receivedEpochMs: 500,
    },
  });
  const s4 = controlReducer(s3, {
    type: "control_expired",
    id: "cmd-1",
    nowEpochMs: (1800000030 + 5) * 1000,
  });
  assert.strictEqual(s4.entries[0].state, "acked");
});

// ── pendingForKind ───────────────────────────────────────────────────────────

test("pendingForKind returns most recent pending entry for a kind", () => {
  const s = createControlState();
  const s2 = controlReducer(s, {
    type: "control_sent",
    kind: "cancel",
    id: "cmd-1",
    sentAt: 1800000000,
    expiresAt: 1800000030,
    sentEpochMs: 0,
  });
  const s3 = controlReducer(s2, {
    type: "control_acked",
    id: "cmd-1",
    ack: {
      ackId: "ack-1",
      status: "applied",
      ackedAt: 1800000001,
      receivedEpochMs: 500,
    },
  });
  const s4 = controlReducer(s3, {
    type: "control_sent",
    kind: "cancel",
    id: "cmd-2",
    sentAt: 1800000010,
    expiresAt: 1800000040,
    sentEpochMs: 10000,
  });
  const pending = pendingForKind(s4, "cancel");
  assert.strictEqual(pending.id, "cmd-2");
});

test("pendingForKind returns undefined when no pending", () => {
  const s = createControlState();
  assert.strictEqual(pendingForKind(s, "cancel"), undefined);
});

// ── Acked entries not retried ────────────────────────────────────────────────

test("acked entries are not retry-eligible", () => {
  setEpoch(0);
  const s = createControlState();
  const s2 = controlReducer(s, {
    type: "control_sent",
    kind: "cancel",
    id: "cmd-1",
    sentAt: 1800000000,
    expiresAt: 1800000030,
    sentEpochMs: 0,
  });
  const s3 = controlReducer(s2, {
    type: "control_acked",
    id: "cmd-1",
    ack: {
      ackId: "ack-1",
      status: "applied",
      ackedAt: 1800000001,
      receivedEpochMs: 500,
    },
  });
  advanceMs(5000);
  const eligible = pendingRetry(s3, fakeNowMs);
  assert.strictEqual(eligible.length, 0);
});

// ── lease_updated action (seeds lease state from sidecar frames) ───────────

test("lease_updated replaces lease state from sidecar frame", () => {
  const s = createControlState();
  const s2 = controlReducer(s, {
    type: "lease_updated",
    lease: {
      leaseId: "lease-sidecar-1",
      generation: 3,
      leaseExpiresAt: 1800000600,
      queueState: "paused",
    },
  });
  assert.strictEqual(s2.lease.leaseId, "lease-sidecar-1");
  assert.strictEqual(s2.lease.generation, 3);
  assert.strictEqual(s2.lease.leaseExpiresAt, 1800000600);
  assert.strictEqual(s2.lease.queueState, "paused");
});

test("lease_updated transitions paused back to running", () => {
  const paused = {
    ...createControlState(),
    lease: {
      leaseId: "lease-1",
      generation: 1,
      leaseExpiresAt: 1800000300,
      queueState: "paused",
    },
  };
  const s2 = controlReducer(paused, {
    type: "lease_updated",
    lease: {
      leaseId: null,
      generation: 0,
      leaseExpiresAt: 0,
      queueState: "running",
    },
  });
  assert.strictEqual(s2.lease.leaseId, null);
  assert.strictEqual(s2.lease.queueState, "running");
});

// ── Shared lease store seeding (Defect 7) ──────────────────────────────────

test("getSharedLeaseState returns seedable state after setSharedLeaseState", async () => {
  // The shared store module is a singleton — import it fresh.
  const { setSharedLeaseState, getSharedLeaseState } = await import(
    "./controlState.ts"
  );
  const pk = "deadbeef00001111";
  setSharedLeaseState(pk, {
    leaseId: "lease-shared-1",
    generation: 2,
    leaseExpiresAt: 1800000900,
    queueState: "paused",
  });
  const stored = getSharedLeaseState(pk);
  assert.ok(stored, "shared lease state should be set");
  assert.strictEqual(stored.leaseId, "lease-shared-1");
  assert.strictEqual(stored.generation, 2);
  assert.strictEqual(stored.queueState, "paused");
});
/**
 * agentStructuredControl.test.mjs — T23
 *
 * Each builder produces JSON that matches the NIP-AO closed-object shape exactly.
 * Compares key sets against the canonical fixture entries for command, pause,
 * renew, resume. Validates expiry bounds.
 */
import assert from "node:assert/strict";
import test from "node:test";

const FIXTURE_COMMAND_KEYS = new Set([
  "format",
  "version",
  "command_id",
  "control",
  "operator_pubkey",
  "target",
  "seq",
  "issued_at",
  "expires_at",
  // steer_message_event_id present only for steer
]);

const FIXTURE_TARGET_KEYS = new Set([
  "computer_id",
  "agent_pubkey",
  "channel_id",
  "run_id",
]);

const FIXTURE_PAUSE_KEYS = new Set([
  "format",
  "version",
  "transition_id",
  "lease_id",
  "generation",
  "transition",
  "operator_pubkey",
  "target",
  "seq",
  "issued_at",
  "transition_expires_at",
  "lease_expires_at",
]);

const FIXTURE_RENEW_KEYS = new Set([
  "format",
  "version",
  "transition_id",
  "lease_id",
  "generation",
  "transition",
  "operator_pubkey",
  "target",
  "seq",
  "issued_at",
  "transition_expires_at",
  "lease_expires_at",
]);

const FIXTURE_RESUME_KEYS = new Set([
  "format",
  "version",
  "transition_id",
  "lease_id",
  "generation",
  "transition",
  "operator_pubkey",
  "target",
  "seq",
  "issued_at",
  "transition_expires_at",
  // lease_expires_at absent for resume
]);

function uuid() {
  // Simple v4-like uuid for testing; deterministic enough for key checks
  const hex = () => Math.floor(Math.random() * 0xffff).toString(16).padStart(4, "0");
  return `${hex()}${hex()}-${hex()}-4${hex().slice(1)}-a${hex().slice(1)}-${hex()}${hex()}${hex()}`;
}

// ── Bootstrap: load the module under test ────────────────────────────────────

let builders;

test("module loads", async () => {
  builders = await import(
    "./agentStructuredControl.ts"
  );
  assert.ok(builders.buildCancelCommand, "buildCancelCommand exported");
  assert.ok(builders.buildSteerCommand, "buildSteerCommand exported");
  assert.ok(builders.buildPauseTransition, "buildPauseTransition exported");
  assert.ok(builders.buildRenewTransition, "buildRenewTransition exported");
  assert.ok(builders.buildResumeTransition, "buildResumeTransition exported");
});

// ── Helpers ──────────────────────────────────────────────────────────────────

function sharedInput() {
  return {
    operatorPubkey: "a".repeat(64),
    target: {
      computer_id: "test-computer-id",
      agent_pubkey: "b".repeat(64),
      channel_id: uuid(),
      run_id: "test-run-1",
    },
    seq: 7,
    issuedAt: 1800000000,
    leaseExpiresAt: 1800000300,
    steerMessageEventId: "c".repeat(64),
    leaseId: uuid(),
    generation: 2,
    commandId: uuid(),
    transitionId: uuid(),
  };
}

// ── Cancel ───────────────────────────────────────────────────────────────────

test("buildCancelCommand key set matches NIP-AO command shape", () => {
  const input = sharedInput();
  const cmd = builders.buildCancelCommand(input);
  const keys = new Set(Object.keys(cmd));
  // Cancel MUST NOT include steer_message_event_id
  const expected = new Set(FIXTURE_COMMAND_KEYS);
  assert.deepStrictEqual(keys, expected);
  assert.strictEqual(cmd.format, "buzz-agent-control-command");
  assert.strictEqual(cmd.version, 1);
  assert.strictEqual(cmd.control, "cancel");
  assert.strictEqual(typeof cmd.command_id, "string");
  assert.ok(cmd.command_id.length > 0);
  assert.ok(!("steer_message_event_id" in cmd));
});

test("buildCancelCommand target keys match NIP-AO target shape", () => {
  const cmd = builders.buildCancelCommand(sharedInput());
  const targetKeys = new Set(Object.keys(cmd.target));
  assert.deepStrictEqual(targetKeys, FIXTURE_TARGET_KEYS);
});

test("buildCancelCommand expiry bounds: expires_at - issued_at <= 300", () => {
  const cmd = builders.buildCancelCommand(sharedInput());
  assert.ok(cmd.expires_at > cmd.issued_at);
  assert.ok(cmd.expires_at - cmd.issued_at <= 300);
  // Default TTL = 30 s
  assert.strictEqual(cmd.expires_at - cmd.issued_at, 30);
});

test("buildCancelCommand custom expiresAt honoured", () => {
  const input = { ...sharedInput(), expiresAt: 1800000045 };
  const cmd = builders.buildCancelCommand(input);
  assert.strictEqual(cmd.expires_at, 1800000045);
});

// ── Steer ────────────────────────────────────────────────────────────────────

test("buildSteerCommand key set includes steer_message_event_id", () => {
  const cmd = builders.buildSteerCommand(sharedInput());
  const keys = new Set(Object.keys(cmd));
  const expected = new Set([...FIXTURE_COMMAND_KEYS, "steer_message_event_id"]);
  assert.deepStrictEqual(keys, expected);
  assert.strictEqual(cmd.control, "steer");
  assert.strictEqual(cmd.steer_message_event_id, "c".repeat(64));
});

test("buildSteerCommand throws without steerMessageEventId", () => {
  const input = sharedInput();
  delete input.steerMessageEventId;
  assert.throws(() => builders.buildSteerCommand(input), /steerMessageEventId/);
});

test("buildSteerCommand expiry bounds: default 60 s", () => {
  const cmd = builders.buildSteerCommand(sharedInput());
  assert.strictEqual(cmd.expires_at - cmd.issued_at, 60);
  assert.ok(cmd.expires_at - cmd.issued_at <= 300);
});

// ── Pause ────────────────────────────────────────────────────────────────────

test("buildPauseTransition key set matches NIP-AO pause-lease shape", () => {
  const input = { ...sharedInput(), generation: undefined, leaseId: undefined };
  const t = builders.buildPauseTransition(input);
  const keys = new Set(Object.keys(t));
  assert.deepStrictEqual(keys, FIXTURE_PAUSE_KEYS);
  assert.strictEqual(t.format, "buzz-agent-pause-lease");
  assert.strictEqual(t.version, 1);
  assert.strictEqual(t.transition, "pause");
  assert.strictEqual(t.generation, 1);
  assert.ok(t.lease_id.length > 0);
  assert.ok(t.transition_id.length > 0);
  assert.ok(typeof t.lease_expires_at === "number");
});

test("buildPauseTransition lease_expires_at <= issued_at + 3600", () => {
  const input = { ...sharedInput(), generation: undefined, leaseId: undefined };
  const t = builders.buildPauseTransition(input);
  assert.ok(t.lease_expires_at - t.issued_at <= 3600);
  // Default = 300 s
  assert.strictEqual(t.lease_expires_at - t.issued_at, 300);
});

test("buildPauseTransition target keys match NIP-AO target shape", () => {
  const input = { ...sharedInput(), generation: undefined, leaseId: undefined };
  const t = builders.buildPauseTransition(input);
  const targetKeys = new Set(Object.keys(t.target));
  assert.deepStrictEqual(targetKeys, FIXTURE_TARGET_KEYS);
});

// ── Renew ────────────────────────────────────────────────────────────────────

test("buildRenewTransition key set matches pause-lease shape", () => {
  const input = sharedInput(); // generation=2, leaseId set
  const t = builders.buildRenewTransition(input);
  const keys = new Set(Object.keys(t));
  assert.deepStrictEqual(keys, FIXTURE_RENEW_KEYS);
  assert.strictEqual(t.transition, "renew");
  assert.strictEqual(t.generation, 2);
  assert.ok(typeof t.lease_expires_at === "number");
});

test("buildRenewTransition throws without leaseId", () => {
  const input = { ...sharedInput(), leaseId: undefined };
  assert.throws(() => builders.buildRenewTransition(input), /leaseId/);
});

test("buildRenewTransition throws without generation >= 2", () => {
  const input = { ...sharedInput(), generation: 1 };
  assert.throws(() => builders.buildRenewTransition(input), /generation/);
});

test("buildRenewTransition throws without leaseExpiresAt", () => {
  const input = { ...sharedInput(), leaseExpiresAt: undefined };
  assert.throws(() => builders.buildRenewTransition(input), /leaseExpiresAt/);
});

// ── Resume ───────────────────────────────────────────────────────────────────

test("buildResumeTransition key set has no lease_expires_at", () => {
  const input = sharedInput();
  const t = builders.buildResumeTransition(input);
  const keys = new Set(Object.keys(t));
  assert.deepStrictEqual(keys, FIXTURE_RESUME_KEYS);
  assert.strictEqual(t.transition, "resume");
  assert.strictEqual(t.generation, 2);
  assert.ok(!("lease_expires_at" in t));
});

test("buildResumeTransition throws without leaseId", () => {
  const input = { ...sharedInput(), leaseId: undefined };
  assert.throws(() => builders.buildResumeTransition(input), /leaseId/);
});

test("buildResumeTransition throws without generation >= 2", () => {
  const input = { ...sharedInput(), generation: 1 };
  assert.throws(() => builders.buildResumeTransition(input), /generation/);
});

// ── Field ordering ───────────────────────────────────────────────────────────

test("cancel command JSON field order matches Rust declaration order", () => {
  const cmd = builders.buildCancelCommand(sharedInput());
  const json = JSON.stringify(cmd);
  const keys = Object.keys(cmd);
  // Verify the keys array is in the exact declaration order
  assert.deepStrictEqual(keys, [
    "format",
    "version",
    "command_id",
    "control",
    "operator_pubkey",
    "target",
    "seq",
    "issued_at",
    "expires_at",
  ]);
  // Verify the JSON string has keys in that order too
  assert.ok(json.startsWith('{"format":"buzz-agent-control-command"'));
});

test("pause transition JSON field order matches Rust declaration order", () => {
  const input = { ...sharedInput(), generation: undefined, leaseId: undefined };
  const t = builders.buildPauseTransition(input);
  const keys = Object.keys(t);
  assert.deepStrictEqual(keys, [
    "format",
    "version",
    "transition_id",
    "lease_id",
    "generation",
    "transition",
    "operator_pubkey",
    "target",
    "seq",
    "issued_at",
    "transition_expires_at",
    "lease_expires_at",
  ]);
});

// ── seq clamping ─────────────────────────────────────────────────────────────

test("seq is clamped to at least 1", () => {
  const input = { ...sharedInput(), seq: 0 };
  const cmd = builders.buildCancelCommand(input);
  assert.strictEqual(cmd.seq, 1);
});

test("issued_at is truncated to integer", () => {
  const input = { ...sharedInput(), issuedAt: 1800000000.9 };
  const cmd = builders.buildCancelCommand(input);
  assert.strictEqual(cmd.issued_at, 1800000000);
});
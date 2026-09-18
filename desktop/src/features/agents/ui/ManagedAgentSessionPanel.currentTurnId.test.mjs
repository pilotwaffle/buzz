/**
 * ManagedAgentSessionPanel currentTurnId resolution test (Defect 5 fix).
 *
 * Verifies that resolveCurrentTurnId walks channel-scoped events backwards
 * and correctly returns "idle" after a terminal turn event, or the active
 * turnId when the most recent event is turn_started.
 *
 * Uses node:test. No jsdom or React needed — the function is pure.
 */

import assert from "node:assert/strict";
import { describe, it } from "node:test";

import { resolveCurrentTurnId } from "@/features/agents/ui/agentSessionPanelLayout.ts";

// ── Helpers ─────────────────────────────────────────────────────────────────

function makeEvent(overrides = {}) {
  return {
    kind: "acp_write",
    channelId: "chan-1",
    turnId: null,
    ...overrides,
  };
}

// ── Tests ───────────────────────────────────────────────────────────────────

describe("resolveCurrentTurnId", () => {
  it("returns idle when channelId is null", () => {
    const events = [makeEvent({ kind: "turn_started", turnId: "t1" })];
    assert.equal(resolveCurrentTurnId(events, null), "idle");
  });

  it("returns idle for empty event list", () => {
    assert.equal(resolveCurrentTurnId([], "chan-1"), "idle");
  });

  it("returns idle when no turn_started and no terminal in channel", () => {
    const events = [
      makeEvent({ kind: "acp_write", turnId: "t1" }),
      makeEvent({ kind: "acp_thought" }),
    ];
    assert.equal(resolveCurrentTurnId(events, "chan-1"), "idle");
  });

  // ── Defect 5 core cases ─────────────────────────────────────────────────

  it("started+completed sequence returns idle", () => {
    // turn_started, then turn_completed — turn is over.
    const events = [
      makeEvent({ kind: "turn_started", turnId: "t1", seq: 1 }),
      makeEvent({ kind: "acp_write", turnId: "t1", seq: 2 }),
      makeEvent({ kind: "turn_completed", turnId: "t1", seq: 3 }),
    ];
    assert.equal(
      resolveCurrentTurnId(events, "chan-1"),
      "idle",
      "after turn_completed, resolveCurrentTurnId must be idle",
    );
  });

  it("started-only returns the turn id", () => {
    const events = [
      makeEvent({ kind: "turn_started", turnId: "t1", seq: 1 }),
      makeEvent({ kind: "acp_thought", turnId: "t1", seq: 2 }),
    ];
    assert.equal(
      resolveCurrentTurnId(events, "chan-1"),
      "t1",
      "in-flight turn_started must return its turnId",
    );
  });

  it("turn_failed before turn_started returns idle", () => {
    const events = [
      makeEvent({ kind: "turn_started", turnId: "t1", seq: 1 }),
      makeEvent({ kind: "turn_failed", turnId: "t1", seq: 2 }),
    ];
    assert.equal(
      resolveCurrentTurnId(events, "chan-1"),
      "idle",
      "turn_failed is a terminal event",
    );
  });

  it("turn_error before turn_started returns idle", () => {
    const events = [
      makeEvent({ kind: "turn_started", turnId: "t1", seq: 1 }),
      makeEvent({ kind: "turn_error", turnId: "t1", seq: 2 }),
    ];
    assert.equal(
      resolveCurrentTurnId(events, "chan-1"),
      "idle",
      "turn_error is a terminal event",
    );
  });

  it("agent_panic before turn_started returns idle", () => {
    const events = [
      makeEvent({ kind: "turn_started", turnId: "t1", seq: 1 }),
      makeEvent({ kind: "agent_panic", turnId: "t1", seq: 2 }),
    ];
    assert.equal(
      resolveCurrentTurnId(events, "chan-1"),
      "idle",
      "agent_panic is a terminal event",
    );
  });

  it("second turn started after first completed returns second turnId", () => {
    // turn_started t1 → turn_completed t1 → turn_started t2: t2 is in-flight.
    const events = [
      makeEvent({ kind: "turn_started", turnId: "t1", seq: 1 }),
      makeEvent({ kind: "turn_completed", turnId: "t1", seq: 2 }),
      makeEvent({ kind: "turn_started", turnId: "t2", seq: 3 }),
    ];
    assert.equal(
      resolveCurrentTurnId(events, "chan-1"),
      "t2",
      "newest turn_started after completion must win",
    );
  });

  it("terminal from different channel does not affect this channel", () => {
    // turn_started t1 on chan-1, turn_completed on chan-2 — t1 is still in-flight.
    const events = [
      makeEvent({ kind: "turn_started", turnId: "t1", seq: 1, channelId: "chan-1" }),
      makeEvent({ kind: "turn_completed", turnId: "t2", seq: 2, channelId: "chan-2" }),
    ];
    assert.equal(
      resolveCurrentTurnId(events, "chan-1"),
      "t1",
      "terminal on a different channel must not affect this channel",
    );
  });

  it("non-turn events between turn_started and terminal are skipped in backward walk", () => {
    // turn_started → acp_write → turn_completed → acp_thought → acp_write
    // Walking backwards: acp_write (not terminal, not started) → acp_thought (ditto)
    // → turn_completed (terminal) → return "idle".
    const events = [
      makeEvent({ kind: "turn_started", turnId: "t1", seq: 1 }),
      makeEvent({ kind: "acp_write", turnId: "t1", seq: 2 }),
      makeEvent({ kind: "turn_completed", turnId: "t1", seq: 3 }),
      makeEvent({ kind: "acp_thought", turnId: "t1", seq: 4 }),
      makeEvent({ kind: "acp_write", turnId: "t1", seq: 5 }),
    ];
    assert.equal(
      resolveCurrentTurnId(events, "chan-1"),
      "idle",
      "terminal deeper in the history still ends the turn",
    );
  });
});
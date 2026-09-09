/**
 * mapObserverEvents test suite.
 *
 * Fixtures are SYNTHETIC, hand-constructed via the `ev()` helper below —
 * they are NOT captured from live harness runs. Capturing real decoded
 * Claude + goose observer frames and adding them as static JSON fixtures
 * is an operator-attended open item (docs/nips/SLICE-1-VERIFICATION.md,
 * runbook §9 "Real-Frame Fixture Capture").
 *
 * Tests verify: mapping correctness, seq-gap detection, reset handling,
 * no-raw-JSON assertion, byte caps, and per-stream gap state.
 */

import { describe, it } from "node:test";
import assert from "node:assert/strict";
import {
  mapObserverEvents,
  mergeTimelineResults,
} from "./mapObserverEvents.ts";

// ── Helpers ───────────────────────────────────────────────────────────────

/** Build a minimal observer event. */
function ev(overrides = {}) {
  return {
    seq: 1,
    timestamp: "2026-09-06T10:00:00.000Z",
    kind: "turn_started",
    agentIndex: 0,
    channelId: "11111111-1111-1111-1111-111111111111",
    sessionId: "sess-01",
    turnId: "turn-01",
    payload: {},
    ...overrides,
  };
}

/** Check that a string contains no raw JSON bracket patterns (assertion guard). */
function assertNoRawJson(value) {
  if (typeof value !== "string") return;
  assert.ok(
    !value.includes('"kind":"') &&
      !value.includes('"payload":') &&
      !value.includes('"seq":'),
    `raw JSON leaked into output: ${value.slice(0, 80)}`,
  );
}

function assertEntriesClean(entries) {
  for (const entry of entries) {
    assertNoRawJson(entry.title);
    if (entry.detail) assertNoRawJson(entry.detail);
    if (entry.excerpt) assertNoRawJson(entry.excerpt);
  }
}

// ── Basic mapping ─────────────────────────────────────────────────────────

describe("mapObserverEvents — basic mapping", () => {
  it("maps a turn_started event with a human title", () => {
    const result = mapObserverEvents(
      [ev({ kind: "turn_started", payload: {} })],
      "agent-pk-1",
    );
    assert.equal(result.entries.length, 1);
    assert.equal(result.entries[0].title, "Turn started");
    assert.equal(result.entries[0].kind, "turn_started");
    assert.equal(result.entries[0].streamKey, "agent-pk-1");
  });

  it("assigns stable id from seq:timestamp", () => {
    const result = mapObserverEvents(
      [ev({ seq: 42, timestamp: "2026-09-06T10:00:05.000Z" })],
      "agent-pk-1",
    );
    assert.equal(result.entries[0].id, "42:2026-09-06T10:00:05.000Z");
  });

  it("extracts detail from body", () => {
    const result = mapObserverEvents(
      [ev({ kind: "acp_read", payload: { body: "file contents here" } })],
      "agent-pk-1",
    );
    assert.equal(result.entries[0].detail, "file contents here");
  });

  it("extracts detail from text", () => {
    const result = mapObserverEvents(
      [ev({ kind: "acp_thought", payload: { text: "I should check X" } })],
      "agent-pk-1",
    );
    assert.equal(result.entries[0].detail, "I should check X");
  });

  it("extracts detail from command", () => {
    const result = mapObserverEvents(
      [
        ev({
          kind: "acp_shell",
          payload: { command: "npm run build" },
        }),
      ],
      "agent-pk-1",
    );
    assert.equal(result.entries[0].detail, "$ npm run build");
  });

  it("excerpts detail exceeding byte cap", () => {
    const big = "x".repeat(500);
    const result = mapObserverEvents(
      [ev({ kind: "acp_read", payload: { body: big } })],
      "agent-pk-1",
    );
    assert.equal(result.entries[0].hasMore, true);
    assert.ok(result.entries[0].excerpt != null);
    assert.ok(result.entries[0].excerpt.length < 500);
  });

  it("does not set hasMore when detail fits", () => {
    const result = mapObserverEvents(
      [ev({ kind: "acp_read", payload: { body: "short" } })],
      "agent-pk-1",
    );
    assert.equal(result.entries[0].hasMore, false);
    assert.equal(result.entries[0].excerpt, null);
  });

  it("uses human title for known kinds", () => {
    const kinds = [
      ["turn_completed", "Turn completed"],
      ["acp_write", "Writing"],
      ["acp_edit", "Editing file"],
      ["acp_todo", "Planning"],
      ["acp_permission", "Permission request"],
    ];
    for (const [kind, title] of kinds) {
      const result = mapObserverEvents([ev({ kind })], "agent-pk-1");
      assert.equal(result.entries[0].title, title, `mismatch for ${kind}`);
    }
  });

  it("uses underscore-to-space fallback for unknown kinds", () => {
    const result = mapObserverEvents(
      [ev({ kind: "custom_event_type" })],
      "agent-pk-1",
    );
    assert.equal(result.entries[0].title, "custom event type");
  });
});

// ── No raw JSON assertion ─────────────────────────────────────────────────

describe("mapObserverEvents — no raw JSON", () => {
  it("does not leak raw observer JSON in titles", () => {
    const result = mapObserverEvents(
      [
        ev({
          kind: "turn_started",
          payload: { nested: { deep: "value" } },
        }),
      ],
      "agent-pk-1",
    );
    assertEntriesClean(result.entries);
  });

  it("does not leak raw observer wire-format fields", () => {
    // Observer wire format fields like "kind", "payload", "seq" must never
    // appear in output titles/details. User content that happens to contain
    // JSON-looking strings is fine — the guard is against the wire envelope.
    const result = mapObserverEvents(
      [
        ev({
          kind: "acp_read",
          payload: { body: '{"user":"data with json inside"}' },
        }),
      ],
      "agent-pk-1",
    );
    // The raw observer JSON envelope keys must not appear.
    for (const entry of result.entries) {
      const combined = `${entry.title} ${entry.detail ?? ""} ${entry.excerpt ?? ""}`;
      assert.ok(
        !combined.includes('"kind":"'),
        "raw observer kind field leaked",
      );
      assert.ok(
        !combined.includes('"payload":'),
        "raw observer payload field leaked",
      );
      assert.ok(
        !combined.includes('"seq":'),
        "raw observer seq field leaked",
      );
      assert.ok(
        !combined.includes('"timestamp":"'),
        "raw observer timestamp field leaked",
      );
    }
  });
});

// ── Seq-gap detection (Q2) ────────────────────────────────────────────────

describe("mapObserverEvents — seq-gap detection", () => {
  it("detects a simple gap and sets incomplete=true", () => {
    const events = [
      ev({ seq: 1 }),
      ev({ seq: 2 }),
      ev({ seq: 5 }), // gap: 3,4 missing
    ];
    const result = mapObserverEvents(events, "agent-pk-1");
    assert.equal(result.streams["agent-pk-1"].incomplete, true);
    assert.equal(result.streams["agent-pk-1"].gapCount, 2); // 5-2-1 = 2 missing
  });

  it("detects multiple gaps and accumulates gapCount", () => {
    const events = [
      ev({ seq: 1 }),
      ev({ seq: 3 }), // gap: 2 missing
      ev({ seq: 7 }), // gap: 4,5,6 missing
    ];
    const result = mapObserverEvents(events, "agent-pk-1");
    assert.equal(result.streams["agent-pk-1"].incomplete, true);
    assert.equal(result.streams["agent-pk-1"].gapCount, 4); // 1+3
  });

  it("detects gap on first event after many missed", () => {
    const events = [
      ev({ seq: 100 }),
      ev({ seq: 200 }), // 99 missing
    ];
    const result = mapObserverEvents(events, "agent-pk-1");
    assert.equal(result.streams["agent-pk-1"].incomplete, true);
    assert.equal(result.streams["agent-pk-1"].gapCount, 99);
  });
});

// ── Stream reset handling (Q2) ────────────────────────────────────────────

describe("mapObserverEvents — stream reset", () => {
  it("does NOT flag decreasing seq as incomplete (harness restart)", () => {
    const events = [
      ev({ seq: 10 }),
      ev({ seq: 11 }),
      ev({ seq: 1 }), // reset: harness restarted, seq back to 1
      ev({ seq: 2 }),
    ];
    const result = mapObserverEvents(events, "agent-pk-1");
    // No gap flagged — decreasing seq is a reset, not loss.
    assert.equal(result.streams["agent-pk-1"].incomplete, false);
    assert.equal(result.streams["agent-pk-1"].gapCount, 0);
  });

  it("does NOT flag equal seq as incomplete (dedup)", () => {
    const events = [
      ev({ seq: 5 }),
      ev({ seq: 5 }), // duplicate
      ev({ seq: 6 }),
    ];
    const result = mapObserverEvents(events, "agent-pk-1");
    assert.equal(result.streams["agent-pk-1"].incomplete, false);
    assert.equal(result.streams["agent-pk-1"].gapCount, 0);
  });

  it("handles gap then reset: gaps before reset are counted", () => {
    const events = [
      ev({ seq: 1 }),
      ev({ seq: 4 }), // gap: 2,3
      ev({ seq: 1 }), // reset — NOT a gap
      ev({ seq: 2 }),
    ];
    const result = mapObserverEvents(events, "agent-pk-1");
    assert.equal(result.streams["agent-pk-1"].incomplete, true);
    assert.equal(result.streams["agent-pk-1"].gapCount, 2);
  });
});

// ── Multi-harness fixtures ────────────────────────────────────────────────

describe("mapObserverEvents — multi-harness", () => {
  it("keys streams by agent pubkey independently", () => {
    const claudeEvents = [
      ev({ seq: 1, sessionId: "sess-claude", kind: "turn_started" }),
      ev({ seq: 2, sessionId: "sess-claude", kind: "turn_completed" }),
    ];
    const gooseEvents = [
      ev({ seq: 1, sessionId: "sess-goose", kind: "turn_started" }),
      ev({ seq: 3, sessionId: "sess-goose", kind: "turn_completed" }), // gap: seq 2
    ];

    const claudeResult = mapObserverEvents(claudeEvents, "claude-pk");
    const gooseResult = mapObserverEvents(gooseEvents, "goose-pk");
    const merged = mergeTimelineResults([claudeResult, gooseResult]);

    assert.equal(merged.entries.length, 4);
    assert.equal(merged.streams["claude-pk"].incomplete, false);
    assert.equal(merged.streams["claude-pk"].gapCount, 0);
    assert.equal(merged.streams["goose-pk"].incomplete, true);
    assert.equal(merged.streams["goose-pk"].gapCount, 1);
  });
});

// ── Quota-rejection-shaped gap fixture ─────────────────────────────────────

describe("mapObserverEvents — quota-rejection gap", () => {
  it("treats any missing seq as incomplete regardless of cause", () => {
    // A quota rejection looks the same as any other message loss to the
    // client — missing seqs. This fixture simulates a quota drop.
    const events = [
      ev({ seq: 50 }),
      ev({ seq: 51 }),
      // seq 52,53,54 dropped by relay quota
      ev({ seq: 55 }),
      ev({ seq: 56 }),
    ];
    const result = mapObserverEvents(events, "agent-pk-1");
    assert.equal(result.streams["agent-pk-1"].incomplete, true);
    assert.equal(result.streams["agent-pk-1"].gapCount, 3); // 52,53,54
  });
});

// ── Byte-cap edge cases ───────────────────────────────────────────────────

describe("mapObserverEvents — byte caps", () => {
  it("handles null payload gracefully", () => {
    const result = mapObserverEvents(
      [ev({ kind: "turn_started", payload: null })],
      "agent-pk-1",
    );
    assert.equal(result.entries[0].detail, null);
    assert.equal(result.entries[0].hasMore, false);
  });

  it("handles undefined payload gracefully", () => {
    const result = mapObserverEvents(
      [ev({ kind: "turn_started", payload: undefined })],
      "agent-pk-1",
    );
    assert.equal(result.entries[0].detail, null);
  });

  it("handles empty object payload gracefully", () => {
    const result = mapObserverEvents(
      [ev({ kind: "turn_started", payload: {} })],
      "agent-pk-1",
    );
    assert.equal(result.entries[0].detail, null);
  });
});

// ── Existing streams carry-forward ────────────────────────────────────────

describe("mapObserverEvents — existing streams", () => {
  it("carries forward existing gap state", () => {
    const existing = {
      "agent-pk-1": { incomplete: true, gapCount: 5 },
    };
    const events = [ev({ seq: 10 }), ev({ seq: 11 })];
    const result = mapObserverEvents(events, "agent-pk-1", existing);
    // No new gaps, but existing state is preserved.
    assert.equal(result.streams["agent-pk-1"].incomplete, true);
    assert.equal(result.streams["agent-pk-1"].gapCount, 5);
  });

  it("adds new gaps on top of existing state", () => {
    const existing = {
      "agent-pk-1": { incomplete: true, gapCount: 3 },
    };
    const events = [
      ev({ seq: 100 }),
      ev({ seq: 103 }), // 2 new gaps
    ];
    const result = mapObserverEvents(events, "agent-pk-1", existing);
    assert.equal(result.streams["agent-pk-1"].incomplete, true);
    assert.equal(result.streams["agent-pk-1"].gapCount, 5); // 3 + 2
  });
});
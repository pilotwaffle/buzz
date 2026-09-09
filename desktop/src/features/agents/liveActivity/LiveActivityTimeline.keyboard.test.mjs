/**
 * LiveActivityTimeline keyboard-expand regression test (G2A M1 / AC12).
 *
 * Operator live test: Enter on a focused entry did NOT expand it, while click
 * did. Root cause: the row is a native <button> — browsers fire `click` on
 * Enter (keydown) and Space (keyup) — and the explicit onKeyDown Enter/Space
 * handler toggled `expanded` a second time, so each keypress toggled twice
 * (net zero). Fix: the onKeyDown handler was removed; activation flows
 * through the native click only.
 *
 * These tests pin that contract in jsdom (jsdom does not synthesize button
 * activation from key events, so keyDown here exercises only our handlers —
 * a leftover onKeyDown toggle would fail the first two tests).
 */

import assert from "node:assert/strict";
import { after, afterEach, before, test } from "node:test";

import { JSDOM } from "jsdom";

const dom = new JSDOM("<!doctype html><html><body></body></html>", {
  url: "http://localhost",
});

before(() => {
  Object.assign(globalThis, {
    document: dom.window.document,
    HTMLElement: dom.window.HTMLElement,
    IS_REACT_ACT_ENVIRONMENT: true,
    window: dom.window,
  });
});

afterEach(async () => {
  const { cleanup } = await import("@testing-library/react");
  cleanup();
});

after(() => dom.window.close());

function observerEvent(overrides = {}) {
  return {
    seq: 1,
    timestamp: "2026-09-06T10:00:00.000Z",
    kind: "acp_read",
    agentIndex: 0,
    channelId: "11111111-1111-1111-1111-111111111111",
    sessionId: "sess-01",
    turnId: "turn-01",
    payload: { body: "read body text that becomes the expandable detail" },
    ...overrides,
  };
}

async function renderTimeline(events) {
  const { createElement } = await import("react");
  const { render } = await import("@testing-library/react");
  const { LiveActivityTimeline } = await import("./LiveActivityTimeline.tsx");
  return render(
    createElement(LiveActivityTimeline, {
      events,
      connectionState: "open",
      errorMessage: null,
      agentRunning: true,
      agentPubkey: "agent-pk-1",
      archiveEnabled: false,
    }),
  );
}

test("keyDown Enter alone does NOT toggle expansion (no double-toggle handler)", async () => {
  const { fireEvent, screen } = await import("@testing-library/react");
  await renderTimeline([observerEvent()]);

  const row = screen.getByRole("button", { expanded: false });
  fireEvent.keyDown(row, { key: "Enter" });
  // Expansion must happen only via the native click activation — a leftover
  // onKeyDown toggle would flip aria-expanded here.
  assert.equal(row.getAttribute("aria-expanded"), "false");

  fireEvent.keyDown(row, { key: " " });
  assert.equal(row.getAttribute("aria-expanded"), "false");
});

test("click (what Enter/Space natively produce on a <button>) toggles expand and collapse", async () => {
  const { fireEvent, screen } = await import("@testing-library/react");
  await renderTimeline([observerEvent()]);

  const row = screen.getByRole("button", { expanded: false });

  fireEvent.click(row);
  assert.equal(row.getAttribute("aria-expanded"), "true");

  // Click again collapses — exactly one toggle per activation.
  fireEvent.click(row);
  assert.equal(row.getAttribute("aria-expanded"), "false");
});

test("a full browser Enter sequence (keydown then native click) expands exactly once", async () => {
  const { fireEvent, screen } = await import("@testing-library/react");
  await renderTimeline([observerEvent()]);

  const row = screen.getByRole("button", { expanded: false });

  // Simulate the browser's real Enter behavior on a focused button:
  // keydown, then activation (click). This is the exact sequence that
  // double-toggled before the fix.
  row.focus();
  fireEvent.keyDown(row, { key: "Enter" });
  fireEvent.click(row);
  assert.equal(row.getAttribute("aria-expanded"), "true");
});

test("empty timeline still shows the Live status badge (G2A M2 cue visibility)", async () => {
  const { screen } = await import("@testing-library/react");
  await renderTimeline([]);

  // Before the fix the empty state rendered without the StatusBar, so no
  // Live badge was visible until the first frame arrived.
  assert.ok(screen.getByText("Live"));
});

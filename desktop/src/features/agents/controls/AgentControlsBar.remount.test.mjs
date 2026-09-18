/**
 * AgentControlsBar remount test (Defect 7).
 *
 * Verifies that paused/lease state survives panel remount: after closing
 * and reopening the session panel while a lease is active, the bar must
 * show Resume (not Pause) and a PAUSED badge. The shared lease store must
 * not be cleared on unmount, and the component must seed its initial state
 * from it on mount.
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

const DEFAULT_PROPS = {
  agentPubkey: "agent-pk-1",
  computerId: "comp-001",
  operatorPubkey: "op-pk-1",
  channelId: "11111111-1111-1111-1111-111111111111",
  turnId: "turn-01",
};

async function renderControls(props = {}) {
  const { createElement } = await import("react");
  const { render } = await import("@testing-library/react");
  const { AgentControlsBar } = await import("./AgentControlsBar.tsx");
  return render(
    createElement(AgentControlsBar, { ...DEFAULT_PROPS, ...props }),
  );
}

// ── Remount with active lease (Defect 7 fix) ────────────────────────────

test("remount with active paused lease shows Resume + PAUSED badge", async () => {
  const { screen } = await import("@testing-library/react");

  // Seed the shared lease store with a paused lease BEFORE the first render.
  const { setSharedLeaseState, getSharedLeaseState } = await import(
    "./controlState.ts"
  );
  setSharedLeaseState(DEFAULT_PROPS.agentPubkey, {
    leaseId: "lease-remount-1",
    generation: 1,
    leaseExpiresAt: Math.floor(Date.now() / 1000) + 300,
    queueState: "paused",
  });

  // First mount — should seed from shared store and show Resume + PAUSED.
  const result1 = await renderControls();

  const resumeBtn1 = screen.queryByRole("button", {
    name: "Resume agent queue",
  });
  assert.ok(resumeBtn1, "Resume button must be visible on first mount");

  const pausedBadge1 = screen.queryByText("Paused");
  assert.ok(pausedBadge1, "PAUSED badge must be visible on first mount");

  // Unmount.
  result1.unmount();

  // Verify the shared lease store survived unmount.
  const stored = getSharedLeaseState(DEFAULT_PROPS.agentPubkey);
  assert.ok(stored, "shared lease store must survive unmount");
  assert.strictEqual(stored.queueState, "paused");

  // Remount — should STILL show Resume + PAUSED.
  const { screen: screen2 } = await import("@testing-library/react");
  const result2 = await renderControls();

  const resumeBtn2 = screen2.queryByRole("button", {
    name: "Resume agent queue",
  });
  assert.ok(
    resumeBtn2,
    "Resume button must still be visible after remount (defect 7 fix)",
  );

  const pausedBadge2 = screen2.queryByText("Paused");
  assert.ok(
    pausedBadge2,
    "PAUSED badge must still be visible after remount (defect 7 fix)",
  );

  result2.unmount();
});

test("remount without active lease shows Pause button", async () => {
  const { screen } = await import("@testing-library/react");

  // Ensure no active lease for this agent.
  const { setSharedLeaseState } = await import("./controlState.ts");
  setSharedLeaseState("agent-clean", {
    leaseId: null,
    generation: 0,
    leaseExpiresAt: 0,
    queueState: "running",
  });

  const result1 = await renderControls({ agentPubkey: "agent-clean" });

  const pauseBtn = screen.queryByRole("button", {
    name: "Pause agent queue",
  });
  assert.ok(pauseBtn, "Pause button must be visible when no lease is active");

  result1.unmount();

  const result2 = await renderControls({ agentPubkey: "agent-clean" });

  const pauseBtn2 = screen.queryByRole("button", {
    name: "Pause agent queue",
  });
  assert.ok(pauseBtn2, "Pause button must remain visible after remount");

  result2.unmount();
});
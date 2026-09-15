import assert from "node:assert/strict";
import { describe, it } from "node:test";

import { AGENT_COMPUTER_FEATURE_FLAGS } from "./agentComputerFlags.ts";
import { desktopFeatures, getFeature } from "./manifest.ts";
import { resolveEnabled } from "./resolveEnabled.ts";

// Gates whose implementation slice has passed its exit gate and flipped the
// flag to the desktop surface (still default-off, opt-in via Experiments).
// Slice 1 wired BUZZ_LIVE_ACTIVITY (8abc8886a); Slice 2 wired
// BUZZ_AGENT_CONTROLS (e1c321f95); Slice 3 wired BUZZ_ROUTINES. Add a gate here
// only in its slice's flag-flip commit.
const WIRED_TO_DESKTOP = new Set([
  "BUZZ_LIVE_ACTIVITY",
  "BUZZ_AGENT_CONTROLS",
  "BUZZ_ROUTINES",
]);

describe("Agent Computer rollout flags", () => {
  it("freezes the exact PRD identifiers as default-off gates", () => {
    assert.deepEqual(AGENT_COMPUTER_FEATURE_FLAGS, [
      "BUZZ_LIVE_ACTIVITY",
      "BUZZ_AGENT_CONTROLS",
      "BUZZ_ROUTINES",
      "BUZZ_DELEGATION",
    ]);

    for (const id of AGENT_COMPUTER_FEATURE_FLAGS) {
      const definition = getFeature(id);
      assert.ok(definition, `${id} must be declared in preview-features.json`);
      assert.equal(definition.defaultEnabled, false, `${id} must default off`);
      assert.deepEqual(
        definition.platforms,
        WIRED_TO_DESKTOP.has(id) ? ["desktop"] : [],
        WIRED_TO_DESKTOP.has(id)
          ? `${id} is wired: desktop opt-in only`
          : `${id} must remain hidden until its implementation slice wires it`,
      );
      assert.equal(
        resolveEnabled(id, {}, definition.defaultEnabled),
        false,
        `${id} must resolve off without an explicit override`,
      );
    }
  });

  it("exposes exactly the wired gates on the current desktop surface", () => {
    const visibleIds = new Set(desktopFeatures.map((feature) => feature.id));
    for (const id of AGENT_COMPUTER_FEATURE_FLAGS) {
      assert.equal(
        visibleIds.has(id),
        WIRED_TO_DESKTOP.has(id),
        WIRED_TO_DESKTOP.has(id)
          ? `${id} must be offered as an opt-in toggle`
          : `${id} exposed a dead UI toggle`,
      );
    }
  });

  it("allows each gate to be enabled independently when its slice is wired", () => {
    for (const selected of AGENT_COMPUTER_FEATURE_FLAGS) {
      const overrides = { [selected]: true };
      for (const id of AGENT_COMPUTER_FEATURE_FLAGS) {
        assert.equal(
          resolveEnabled(id, overrides, getFeature(id)?.defaultEnabled),
          id === selected,
          `${selected} must not enable ${id}`,
        );
      }
    }
  });
});

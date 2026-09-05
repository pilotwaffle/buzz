import assert from "node:assert/strict";
import { describe, it } from "node:test";

import { AGENT_COMPUTER_FEATURE_FLAGS } from "./agentComputerFlags.ts";
import { desktopFeatures, getFeature } from "./manifest.ts";
import { resolveEnabled } from "./resolveEnabled.ts";

describe("Agent Computer Slice-0 rollout flags", () => {
  it("freezes the exact PRD identifiers as hidden, default-off gates", () => {
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
        [],
        `${id} must remain hidden until its implementation slice wires it`,
      );
      assert.equal(
        resolveEnabled(id, {}, definition.defaultEnabled),
        false,
        `${id} must resolve off without an explicit override`,
      );
    }
  });

  it("keeps all four gates out of the current desktop surface", () => {
    const visibleIds = new Set(desktopFeatures.map((feature) => feature.id));
    for (const id of AGENT_COMPUTER_FEATURE_FLAGS) {
      assert.equal(visibleIds.has(id), false, `${id} exposed a dead UI toggle`);
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

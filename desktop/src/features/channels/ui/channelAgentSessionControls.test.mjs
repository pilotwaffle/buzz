/**
 * Slice 2 structured-controls — channel-agent session wiring tests.
 *
 * Defect 1 (operator gate 2026-09-11): AgentControlsBar never mounted from the
 * channel-side session panel because ChannelAgentSessionAgent had no computerId.
 *
 * These tests verify:
 *   1. buildChannelAgentSessionCandidates populates computerId for managed agents.
 *   2. AgentControlsBar renders its root toolbar with aria-label="Agent controls".
 */

import assert from "node:assert/strict";
import test from "node:test";

import React from "react";
import { renderToStaticMarkup } from "react-dom/server";

import { QueryClient, QueryClientProvider } from "@tanstack/react-query";

import { buildChannelAgentSessionCandidates } from "./useChannelAgentSessions.ts";
import { AgentControlsBar } from "@/features/agents/controls/AgentControlsBar.tsx";

// ── Helpers ──────────────────────────────────────────────────────────────────

function managedAgent(overrides = {}) {
  return {
    pubkey: "00abcd1234abcd1234abcd1234abcd1234abcd1234abcd1234abcd1234abcd12",
    name: "test-agent",
    computerId: "ed56f9ce-548a-437f-9e93-648eee8e3a48",
    status: "deployed",
    ...overrides,
  };
}

function relayAgent(overrides = {}) {
  return {
    pubkey: "00eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee",
    name: "relay-agent",
    status: "online",
    channelIds: [],
    channels: [],
    ...overrides,
  };
}

/**
 * Render AgentControlsBar inside a QueryClientProvider so hooks that use
 * @tanstack/react-query don't throw in a test environment.
 */
function renderBar(props = {}) {
  const queryClient = new QueryClient({
    defaultOptions: { queries: { enabled: false } },
  });
  return renderToStaticMarkup(
    React.createElement(
      QueryClientProvider,
      { client: queryClient },
      React.createElement(AgentControlsBar, {
        agentPubkey: "00abcd1234abcd1234abcd1234abcd1234abcd1234abcd1234abcd1234abcd12",
        computerId: "ed56f9ce-548a-437f-9e93-648eee8e3a48",
        operatorPubkey: "00ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff",
        channelId: "11111111-1111-4111-8111-111111111111",
        turnId: "idle",
        ...props,
      }),
    ),
  );
}

// ── Defect 1: computerId data flow ───────────────────────────────────────────

test("buildChannelAgentSessionCandidates: managed agent carries computerId", () => {
  const candidates = buildChannelAgentSessionCandidates({
    managedAgents: [managedAgent()],
    relayAgents: [],
    channelMembers: [],
  });

  assert.equal(candidates.length, 1);
  const agent = candidates[0];
  assert.equal(agent.agentSource, "managed");
  assert.equal(
    agent.computerId,
    "ed56f9ce-548a-437f-9e93-648eee8e3a48",
    "managed agent must carry computerId so AgentControlsBar can bind controls",
  );
  assert.equal(agent.canInterruptTurn, true);
});

test("buildChannelAgentSessionCandidates: relay agent has no computerId", () => {
  const candidates = buildChannelAgentSessionCandidates({
    managedAgents: [],
    relayAgents: [relayAgent()],
    channelMembers: [],
  });

  assert.equal(candidates.length, 1);
  assert.equal(candidates[0].agentSource, "relay");
  assert.equal(
    candidates[0].computerId,
    undefined,
    "relay agents must not have computerId (cannot be controlled)",
  );
  assert.equal(candidates[0].canInterruptTurn, false);
});

test("buildChannelAgentSessionCandidates: managed wins over relay with computerId", () => {
  // Same pubkey in both lists — managed must win and carry computerId.
  const pubkey = "00abcd1234abcd1234abcd1234abcd1234abcd1234abcd1234abcd1234abcd12";
  const candidates = buildChannelAgentSessionCandidates({
    managedAgents: [managedAgent({ pubkey })],
    relayAgents: [relayAgent({ pubkey })],
    channelMembers: [],
  });

  assert.equal(candidates.length, 1);
  assert.equal(candidates[0].agentSource, "managed");
  assert.equal(candidates[0].computerId, "ed56f9ce-548a-437f-9e93-648eee8e3a48");
  assert.equal(candidates[0].canInterruptTurn, true);
});

// ── AgentControlsBar render test ─────────────────────────────────────────────

test("AgentControlsBar renders toolbar with aria-label", () => {
  const html = renderBar();

  assert.match(
    html,
    /aria-label="Agent controls"/,
    "AgentControlsBar must render its toolbar with aria-label='Agent controls'",
  );
});

test("AgentControlsBar renders Cancel, Steer, and Pause buttons in default state", () => {
  const html = renderBar();

  // Default (idle) state: Cancel, Steer, and Pause are always rendered.
  // Renew only appears when the agent is paused — it's a toggle pair with Pause.
  assert.match(html, /aria-label="Cancel current turn"/);
  assert.match(html, /aria-label="Steer agent with a message"/);
  assert.match(html, /aria-label="Pause agent queue"/);
  // Renew is NOT rendered in the default (non-paused) state.
  assert.doesNotMatch(html, /aria-label="Renew pause lease by 5 minutes"/);
});

test("AgentControlsBar renders without error in default (idle) state", () => {
  // Default state: no active lease → Pause button visible, Resume not rendered.
  const html = renderBar();
  assert.match(html, /aria-label="Pause agent queue"/);
  assert.doesNotMatch(html, /aria-label="Resume agent queue"/);
});
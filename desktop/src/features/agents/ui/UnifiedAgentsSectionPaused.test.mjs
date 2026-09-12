/**
 * Defect 7b — paused badge on the Agents page card.
 *
 * The paused badge originally added to ManagedAgentRow.tsx never rendered
 * because ManagedAgentRow is only used by AgentGroupRows, which has no callers.
 * The visible Agents page cards are rendered by UnifiedAgentsSection.tsx
 * (both AgentPersonaCard and StandaloneAgentCard). This test verifies that
 * both card kinds show a PAUSED badge when the agent's lease state is paused.
 */

import assert from "node:assert/strict";
import { after, afterEach, before, test } from "node:test";
import { JSDOM } from "jsdom";

const dom = new JSDOM("<!doctype html><html><body></body></html>", {
  url: "http://localhost",
});

const clients = [];

let act;
let cleanup;
let render;
let screen;
let createElement;
let QueryClient;
let QueryClientProvider;
let UnifiedAgentsSection;
let useAgentAvailabilityLookup;

const ipcHandlers = new Map();

const TEST_PK = "d".repeat(64);

function agent(overrides = {}) {
  return {
    pubkey: TEST_PK,
    name: "Test Agent",
    personaId: null, // ungrouped → renders via StandaloneAgentCard
    status: "running",
    model: null,
    modelSource: "global",
    lastError: null,
    lastErrorCode: null,
    needsRestart: false,
    personaOrphaned: false,
    ...overrides,
  };
}

function persona(overrides = {}) {
  return {
    id: "persona-1",
    displayName: "Test Persona",
    avatarUrl: null,
    model: null,
    isBuiltIn: false,
    sourceTeam: null,
    ...overrides,
  };
}

function baseProps(overrides = {}) {
  return {
    defaultModel: "gpt-x",
    actionErrorMessage: null,
    actionNoticeMessage: null,
    agents: [],
    agentsError: null,
    isActionPending: false,
    isAgentsLoading: false,
    restartingAgentPubkey: null,
    startingAgentPubkey: null,
    startingPersonaIds: new Set(),
    onOpenAgentProfile: () => {},
    onOpenPersonaProfile: () => {},
    onRestartAgent: () => {},
    onStartAgent: () => {},
    onStartPersona: () => {},
    personas: [],
    personasError: null,
    personaFeedbackErrorMessage: null,
    personaFeedbackNoticeMessage: null,
    isPersonasLoading: false,
    isPersonasPending: false,
    onOpenCatalog: () => {},
    onDuplicatePersona: () => {},
    onEditPersona: () => {},
    onSharePersona: () => {},
    onDeactivatePersona: () => {},
    onDeletePersona: () => {},
    ...overrides,
  };
}

function Surface(props) {
  const { getAvailability } = useAgentAvailabilityLookup(
    props.agents.map((a) => a.pubkey),
  );
  return createElement(UnifiedAgentsSection, { ...props, getAvailability });
}

function renderSection(props) {
  const client = new QueryClient({
    defaultOptions: {
      queries: { retry: false, gcTime: 0 },
      mutations: { gcTime: 0 },
    },
  });
  clients.push(client);
  return render(
    createElement(
      QueryClientProvider,
      { client },
      createElement(Surface, props),
    ),
  );
}

before(async () => {
  Object.assign(globalThis, {
    document: dom.window.document,
    HTMLElement: dom.window.HTMLElement,
    window: dom.window,
    IS_REACT_ACT_ENVIRONMENT: true,
  });
  Object.defineProperty(globalThis, "navigator", {
    configurable: true,
    value: dom.window.navigator,
    writable: true,
  });
  dom.window.matchMedia = () => ({
    matches: true,
    addEventListener() {},
    removeEventListener() {},
  });
  dom.window.__TAURI_INTERNALS__ = {
    invoke: (cmd, args) => {
      const handler = ipcHandlers.get(cmd);
      if (handler) return handler(args);
      return Promise.reject(new Error(`unmocked Tauri command: ${cmd}`));
    },
    transformCallback: () => Math.random(),
  };

  ({ act, cleanup, render, screen } = await import(
    "@testing-library/react"
  ));
  ({ createElement } = await import("react"));
  ({ QueryClient, QueryClientProvider } = await import(
    "@tanstack/react-query"
  ));
  ({ UnifiedAgentsSection } = await import("./UnifiedAgentsSection.tsx"));
  ({ useAgentAvailabilityLookup } = await import(
    "../lib/useAgentAvailability.ts"
  ));
});

afterEach(async () => {
  cleanup?.();
  for (const client of clients.splice(0)) {
    client.cancelQueries();
    client.clear();
  }
  ipcHandlers.clear();
  // Clear the shared lease store between tests.
  const { setSharedLeaseState } = await import(
    "../controls/controlState.ts"
  );
  setSharedLeaseState(TEST_PK, {
    leaseId: null,
    generation: 0,
    leaseExpiresAt: 0,
    queueState: "running",
  });
});

after(() => dom.window.close());

function installMinimalIpc() {
  ipcHandlers.set("get_identity", () =>
    Promise.resolve({ pubkey: "owner-pk", display_name: "Me" }),
  );
  ipcHandlers.set("list_archived_identities", () => Promise.resolve([]));
  ipcHandlers.set("get_user_profile", () =>
    Promise.resolve({
      pubkey: TEST_PK,
      display_name: null,
      avatar_url: null,
      about: null,
      nip05_handle: null,
      owner_pubkey: null,
    }),
  );
}

// ── StandaloneAgentCard: paused badge ─────────────────────────────────────

test("ungrouped agent card shows PAUSED badge when lease is paused", async () => {
  installMinimalIpc();

  const { setSharedLeaseState } = await import(
    "../controls/controlState.ts"
  );
  setSharedLeaseState(TEST_PK, {
    leaseId: "lease-test-1",
    generation: 1,
    leaseExpiresAt: Math.floor(Date.now() / 1000) + 300,
    queueState: "paused",
  });

  await act(async () => {
    renderSection(
      baseProps({
        agents: [agent()],
        personas: [],
      }),
    );
  });

  const pausedBadge = screen.queryByText("Paused");
  assert.ok(pausedBadge, "PAUSED badge must be visible on the card when lease is paused");

  const pauseIcon = pausedBadge.closest("[aria-label]");
  assert.ok(pauseIcon, "PAUSED badge must have an aria-label");
});

test("ungrouped agent card shows NO paused badge when lease is running", async () => {
  installMinimalIpc();

  const { setSharedLeaseState } = await import(
    "../controls/controlState.ts"
  );
  setSharedLeaseState(TEST_PK, {
    leaseId: null,
    generation: 0,
    leaseExpiresAt: 0,
    queueState: "running",
  });

  await act(async () => {
    renderSection(
      baseProps({
        agents: [agent()],
        personas: [],
      }),
    );
  });

  const pausedBadge = screen.queryByText("Paused");
  assert.equal(pausedBadge, null, "PAUSED badge must NOT be visible when lease is running");
});

// ── AgentPersonaCard: paused badge ────────────────────────────────────────

test("persona-linked agent card shows PAUSED badge when lease is paused", async () => {
  installMinimalIpc();

  const { setSharedLeaseState } = await import(
    "../controls/controlState.ts"
  );
  setSharedLeaseState(TEST_PK, {
    leaseId: "lease-test-2",
    generation: 1,
    leaseExpiresAt: Math.floor(Date.now() / 1000) + 300,
    queueState: "paused",
  });

  await act(async () => {
    renderSection(
      baseProps({
        agents: [agent({ personaId: "persona-1" })],
        personas: [persona()],
      }),
    );
  });

  const pausedBadge = screen.queryByText("Paused");
  assert.ok(pausedBadge, "PAUSED badge must be visible on persona card when lease is paused");
});
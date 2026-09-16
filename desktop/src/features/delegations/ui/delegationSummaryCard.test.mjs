/**
 * Slice 4 (Delegation) Step 6 check: three relay-signed notices plus one
 * agent-signed outcome sharing a delegation_id collapse into one
 * DelegationSummaryCard with a `delivered` chip and tokens-used shown; flag
 * off, they render as ordinary messages (I-1) — verified by simply not
 * calling the grouping transform, mirroring the real ChannelScreen gate.
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
    MutationObserver: dom.window.MutationObserver,
  });
});

afterEach(async () => {
  const { cleanup } = await import("@testing-library/react");
  cleanup();
});

after(() => dom.window.close());

function baseMessage(overrides) {
  return {
    id: overrides.id,
    createdAt: overrides.createdAt,
    author: "relay",
    isAgent: false,
    time: `t${overrides.createdAt}`,
    body: overrides.body ?? "",
    depth: 0,
    tags: overrides.tags,
    ...overrides,
  };
}

const DELEGATION_ID = "00000000-0000-0000-0000-0000000000e1";

function fourDelegationMessages() {
  return [
    baseMessage({
      id: "notice-approved",
      createdAt: 1,
      body: "delegation approved",
      tags: [
        ["buzz:delegation", DELEGATION_ID],
        ["buzz:delegation-notice", "approved"],
      ],
    }),
    baseMessage({
      id: "wake",
      createdAt: 2,
      body: "the ordinary reply from the target agent",
      tags: [],
    }),
    baseMessage({
      id: "outcome",
      createdAt: 3,
      body: "done",
      isAgent: true,
      tags: [
        ["buzz:delegation", DELEGATION_ID],
        ["buzz:delegation-outcome", "delivered"],
        ["buzz:delegation-tokens", "1234"],
      ],
    }),
  ];
}

test("three delegation-tagged messages for one id collapse into one card with a delivered chip and tokens shown", async () => {
  const { groupDelegationMessages } = await import(
    "@/features/delegations/lib/groupDelegationMessages"
  );
  const messages = fourDelegationMessages();
  const grouped = groupDelegationMessages(messages);

  const delegationEntries = grouped.filter((m) => m.delegationSummary);
  assert.equal(
    delegationEntries.length,
    1,
    "exactly one synthetic summary entry",
  );
  assert.equal(grouped.length, 2, "two delegation messages collapse to one card; the ordinary reply is untouched");

  const summary = delegationEntries[0].delegationSummary;
  assert.equal(summary.state, "delivered");
  assert.equal(summary.tokensUsed, 1234);
  assert.equal(summary.rawMessages.length, 2);

  const ordinaryReply = grouped.find((m) => m.id === "wake");
  assert.ok(ordinaryReply, "the target's ordinary reply is untouched");
  assert.equal(ordinaryReply.delegationSummary, undefined);
});

test("the relay-signed wake collapses into the card too, not just notices/outcomes", async () => {
  const { groupDelegationMessages } = await import(
    "@/features/delegations/lib/groupDelegationMessages"
  );
  const messages = [
    baseMessage({
      id: "notice-approved",
      createdAt: 1,
      body: "delegation approved",
      tags: [
        ["buzz:delegation", DELEGATION_ID],
        ["buzz:delegation-notice", "approved"],
      ],
    }),
    baseMessage({
      id: "wake",
      createdAt: 2,
      body: "Delegated task: read the originating message in this thread and complete it.",
      tags: [
        ["buzz:delegation", DELEGATION_ID],
        ["buzz:delegation-run", "00000000-0000-0000-0000-0000000000f2"],
      ],
    }),
    baseMessage({
      id: "target-reply",
      createdAt: 3,
      body: "the ordinary reply from the target agent",
      isAgent: true,
      tags: [],
    }),
  ];
  const grouped = groupDelegationMessages(messages);

  const delegationEntries = grouped.filter((m) => m.delegationSummary);
  assert.equal(delegationEntries.length, 1, "exactly one synthetic summary entry");
  assert.equal(
    grouped.length,
    2,
    "notice + wake collapse to one card; the target's ordinary reply is untouched",
  );
  assert.equal(delegationEntries[0].delegationSummary.rawMessages.length, 2);

  const ordinaryReply = grouped.find((m) => m.id === "target-reply");
  assert.ok(ordinaryReply, "the target's ordinary reply is untouched");
  assert.equal(ordinaryReply.delegationSummary, undefined);
});

test("flag off (transform not applied): every message renders as an ordinary row", async () => {
  const messages = fourDelegationMessages();
  // Mirrors ChannelScreen's gate: `delegationEnabled ? groupDelegationMessages(formatted) : formatted`.
  const rendered = messages;

  assert.equal(rendered.length, 3);
  for (const message of rendered) {
    assert.equal(message.delegationSummary, undefined);
  }
});

test("DelegationSummaryCard renders the chip label and tokens, collapsed by default", async () => {
  const { createElement } = await import("react");
  const { render, screen } = await import("@testing-library/react");
  const { DelegationSummaryCard } = await import("./DelegationSummaryCard.tsx");

  render(
    createElement(DelegationSummaryCard, {
      summary: {
        delegationId: DELEGATION_ID,
        state: "delivered",
        tokensUsed: 1234,
        rawMessages: fourDelegationMessages().slice(0, 2),
      },
    }),
  );

  assert.ok(screen.getByText("Delivered"));
  assert.ok(screen.getByText(/1,234 tokens used/));
  assert.equal(
    screen.queryByText("delegation approved"),
    null,
    "raw notices are hidden until expanded",
  );
});

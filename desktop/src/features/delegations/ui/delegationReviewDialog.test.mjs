/**
 * Slice 4 (Delegation) Step 6 check: approving a delegation calls
 * `approveDelegation` with the origin event id and the exact request JSON
 * the block carried (build_spec.md 6.3, 6.7).
 *
 * Exercises `DelegationReviewDialog` directly via its `approveDelegationFn`
 * injection seam rather than mocking the Tauri IPC layer — the test-loader's
 * `@tauri-apps/api/core` stub always returns `'{}'` and cannot be
 * overridden per test, so dependency injection is the only reliable seam.
 */

import assert from "node:assert/strict";
import { after, afterEach, before, test } from "node:test";
import { JSDOM } from "jsdom";

const dom = new JSDOM("<!doctype html><html><body></body></html>", {
  url: "http://localhost",
});

dom.window.matchMedia ??= (query) => ({
  matches: false,
  media: query,
  onchange: null,
  addListener: () => {},
  removeListener: () => {},
  addEventListener: () => {},
  removeEventListener: () => {},
  dispatchEvent: () => false,
});

before(() => {
  Object.assign(globalThis, {
    document: dom.window.document,
    HTMLElement: dom.window.HTMLElement,
    IS_REACT_ACT_ENVIRONMENT: true,
    window: dom.window,
    matchMedia: dom.window.matchMedia,
    MutationObserver: dom.window.MutationObserver,
  });

  // Radix Dialog's focus/dismiss machinery references many DOM globals
  // without a window. prefix; copy them in bulk to avoid per-global
  // whack-a-mole (mirrors CommunityCatalogDialogAvatarLeak.test.mjs and
  // delegationCodeBlock.test.mjs).
  for (const key of Object.getOwnPropertyNames(dom.window)) {
    if (
      !(key in globalThis) &&
      (key.startsWith("HTML") ||
        key.startsWith("SVG") ||
        key.startsWith("CSS") ||
        [
          "Node",
          "NodeFilter",
          "NodeList",
          "NamedNodeMap",
          "Event",
          "CustomEvent",
          "MouseEvent",
          "KeyboardEvent",
          "FocusEvent",
          "InputEvent",
          "PointerEvent",
          "TouchEvent",
          "WheelEvent",
          "EventTarget",
          "Text",
          "Comment",
          "DocumentFragment",
          "Range",
          "Selection",
          "getComputedStyle",
          "IntersectionObserver",
          "ResizeObserver",
        ].includes(key))
    ) {
      const val = dom.window[key];
      if (val !== undefined) globalThis[key] = val;
    }
  }
  globalThis.getComputedStyle = dom.window.getComputedStyle.bind(dom.window);

  // Radix DismissableLayer/FocusScope dispatch plain objects; JSDOM's strict
  // Event validation throws on them. Drop non-Event objects so the dialog
  // renders without throwing from effects; real Event delivery is unaffected.
  const origDispatch = dom.window.EventTarget.prototype.dispatchEvent;
  dom.window.EventTarget.prototype.dispatchEvent = function (event) {
    if (!(event instanceof dom.window.Event)) return false;
    return origDispatch.call(this, event);
  };
  globalThis.EventTarget = dom.window.EventTarget;
});

afterEach(async () => {
  const { cleanup } = await import("@testing-library/react");
  cleanup();
});

after(() => dom.window.close());

const SAMPLE_REQUEST = JSON.stringify({
  delegation_id: "00000000-0000-0000-0000-0000000000e1",
  parent_approval_event_id: null,
  source_agent: "a".repeat(64),
  target_agent: "b".repeat(64),
  agent_path: ["a".repeat(64), "b".repeat(64)],
  hop_budget: 1,
  max_turns: 3,
  cost_cap_microusd: null,
  token_budget: 100000,
  idempotency_key: "idem-1",
  expires_at: 4102444800,
});

async function renderDialog({ approveDelegationFn, originEventId } = {}) {
  const { createElement } = await import("react");
  const { render } = await import("@testing-library/react");
  const { QueryClient, QueryClientProvider } = await import(
    "@tanstack/react-query"
  );
  const { ThemeProvider } = await import("@/shared/theme/ThemeProvider.tsx");
  const { DelegationReviewDialog } = await import("./DelegationReviewDialog.tsx");

  const queryClient = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  });

  return render(
    createElement(
      QueryClientProvider,
      { client: queryClient },
      createElement(
        ThemeProvider,
        null,
        createElement(DelegationReviewDialog, {
          open: true,
          onOpenChange: () => {},
          requestJson: SAMPLE_REQUEST,
          originEventId,
          approveDelegationFn,
        }),
      ),
    ),
  );
}

test("approve calls approveDelegationFn with the origin event id and exact request JSON", async () => {
  const { screen, fireEvent } = await import("@testing-library/react");
  let calledWith = null;
  const approveDelegationFn = async (originEventId, requestJson) => {
    calledWith = { originEventId, requestJson };
    return { eventId: "approved-event-id" };
  };

  await renderDialog({
    approveDelegationFn,
    originEventId: "the-origin-event-id",
  });

  fireEvent.click(screen.getByRole("button", { name: /^approve$/i }));
  // Flush the async approve handler.
  await new Promise((resolve) => setTimeout(resolve, 0));

  assert.ok(calledWith, "approveDelegationFn must have been called");
  assert.equal(calledWith.originEventId, "the-origin-event-id");
  assert.equal(calledWith.requestJson, SAMPLE_REQUEST);
});

test("approve button is disabled when the origin event id is missing", async () => {
  const { screen } = await import("@testing-library/react");
  let called = false;
  const approveDelegationFn = async () => {
    called = true;
    return { eventId: "unused" };
  };

  await renderDialog({ approveDelegationFn, originEventId: undefined });

  const approveButton = screen.getByRole("button", { name: /^approve$/i });
  assert.equal(approveButton.disabled, true);
  assert.equal(called, false);
});

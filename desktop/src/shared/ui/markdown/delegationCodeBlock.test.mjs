/**
 * Slice 4 (Delegation) Step 6 check: a `buzz-delegation` fenced code block
 * renders a "Review delegation" button only when BUZZ_DELEGATION is on AND
 * the message author is a managed agent; every other combination renders
 * identically to a plain code block (I-1).
 *
 * Mirrors routineCodeBlock.test.mjs, same jsdom + @testing-library/react
 * harness. `DelegationReviewDialog` calls `useManagedAgentsQuery`, so every
 * render here needs a `QueryClientProvider` ancestor.
 */

import assert from "node:assert/strict";
import { after, afterEach, before, beforeEach, test } from "node:test";
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
    localStorage: dom.window.localStorage,
    matchMedia: dom.window.matchMedia,
    MutationObserver: dom.window.MutationObserver,
  });

  // Radix Dialog's focus/dismiss machinery references many DOM globals
  // without a window. prefix; copy them in bulk to avoid per-global
  // whack-a-mole (mirrors CommunityCatalogDialogAvatarLeak.test.mjs).
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

beforeEach(() => {
  dom.window.localStorage.clear();
});

afterEach(async () => {
  const { cleanup } = await import("@testing-library/react");
  cleanup();
});

after(() => dom.window.close());

async function setDelegationEnabled(enabled) {
  const { setOverride } = await import("@/shared/features/store.ts");
  const { emitChange } = await import("@/shared/features/useFeatureEnabled.ts");
  setOverride("BUZZ_DELEGATION", enabled);
  emitChange();
}

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

async function renderCodeBlock({
  authorIsManagedAgent,
  language = "buzz-delegation",
  messageId = "origin-event-id",
  content = SAMPLE_REQUEST,
} = {}) {
  const { createElement } = await import("react");
  const { render } = await import("@testing-library/react");
  const { QueryClient, QueryClientProvider } = await import(
    "@tanstack/react-query"
  );
  const { ThemeProvider } = await import("@/shared/theme/ThemeProvider.tsx");
  const { TooltipProvider } = await import("@/shared/ui/tooltip");
  const { MarkdownCodeBlock } = await import("./CodeBlock.tsx");
  const { MarkdownRuntimeContext } = await import("./runtimeContext.ts");
  const { WorkflowEditorOverlayProvider } = await import(
    "@/shared/context/WorkflowEditorOverlayContext.tsx"
  );

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
        createElement(
          TooltipProvider,
          null,
          createElement(
            WorkflowEditorOverlayProvider,
            {
              onOpenNewWorkflow: () => {},
              onOpenWorkflow: () => {},
            },
            createElement(
              MarkdownRuntimeContext.Provider,
              {
                value: {
                  authorIsManagedAgent,
                  channels: [],
                  messageId,
                  onOpenChannel: () => {},
                  onOpenEntityLink: () => {},
                  onOpenMessageLink: () => {},
                  relayOrigin: null,
                },
              },
              createElement(MarkdownCodeBlock, { language }, content),
            ),
          ),
        ),
      ),
    ),
  );
}

test("flag off + managed-agent author: renders no review button", async () => {
  await setDelegationEnabled(false);
  const { screen } = await import("@testing-library/react");
  await renderCodeBlock({ authorIsManagedAgent: true });

  assert.equal(
    screen.queryByRole("button", { name: /review delegation/i }),
    null,
  );
});

test("flag on + non-agent author: renders no review button", async () => {
  await setDelegationEnabled(true);
  const { screen } = await import("@testing-library/react");
  await renderCodeBlock({ authorIsManagedAgent: false });

  assert.equal(
    screen.queryByRole("button", { name: /review delegation/i }),
    null,
  );
});

test("flag on + managed-agent author: renders the review button", async () => {
  await setDelegationEnabled(true);
  const { screen } = await import("@testing-library/react");
  await renderCodeBlock({ authorIsManagedAgent: true });

  assert.ok(screen.getByRole("button", { name: /review delegation/i }));
});

test("flag on + managed-agent author + non-delegation language: renders no review button", async () => {
  await setDelegationEnabled(true);
  const { screen } = await import("@testing-library/react");
  await renderCodeBlock({ authorIsManagedAgent: true, language: "json" });

  assert.equal(
    screen.queryByRole("button", { name: /review delegation/i }),
    null,
  );
});

test("flag on + managed-agent author + cost cap present: review dialog shows the warning text", async () => {
  await setDelegationEnabled(true);
  const { screen, fireEvent } = await import("@testing-library/react");
  const withCostCap = JSON.stringify({
    ...JSON.parse(SAMPLE_REQUEST),
    cost_cap_microusd: 500,
  });
  await renderCodeBlock({ authorIsManagedAgent: true, content: withCostCap });

  fireEvent.click(screen.getByRole("button", { name: /review delegation/i }));

  assert.ok(
    screen.getByText(
      "cost cap set: this delegation cannot run in this release",
    ),
  );
});

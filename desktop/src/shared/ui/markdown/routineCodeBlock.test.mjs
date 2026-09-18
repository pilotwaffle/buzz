/**
 * Slice 3 (Routines) Step 6 check: a `buzz-routine` fenced code block renders
 * a "Review routine" button only when BUZZ_ROUTINES is on AND the message
 * author is a managed agent; every other combination renders identically to
 * a plain YAML code block (I-1, AC-17).
 *
 * Uses the same jsdom + @testing-library/react harness as
 * AgentControlsBar.keyboard.test.mjs.
 */

import assert from "node:assert/strict";
import { after, afterEach, before, beforeEach, test } from "node:test";
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
    localStorage: dom.window.localStorage,
    MutationObserver: dom.window.MutationObserver,
  });
});

beforeEach(() => {
  dom.window.localStorage.clear();
});

afterEach(async () => {
  const { cleanup } = await import("@testing-library/react");
  cleanup();
});

after(() => dom.window.close());

async function setRoutinesEnabled(enabled) {
  const { setOverride } = await import("@/shared/features/store.ts");
  const { emitChange } = await import("@/shared/features/useFeatureEnabled.ts");
  setOverride("BUZZ_ROUTINES", enabled);
  emitChange();
}

async function renderCodeBlock({
  authorIsManagedAgent,
  language = "buzz-routine",
  openNewWorkflow,
} = {}) {
  const { createElement } = await import("react");
  const { render } = await import("@testing-library/react");
  const { TooltipProvider } = await import("@/shared/ui/tooltip");
  const { MarkdownCodeBlock } = await import("./CodeBlock.tsx");
  const { MarkdownRuntimeContext } = await import("./runtimeContext.ts");
  const { WorkflowEditorOverlayProvider } = await import(
    "@/shared/context/WorkflowEditorOverlayContext.tsx"
  );

  return render(
    createElement(
      TooltipProvider,
      null,
      createElement(
        WorkflowEditorOverlayProvider,
        {
          onOpenNewWorkflow: openNewWorkflow ?? (() => {}),
          onOpenWorkflow: () => {},
        },
        createElement(
          MarkdownRuntimeContext.Provider,
          {
            value: {
              authorIsManagedAgent,
              channels: [],
              onOpenChannel: () => {},
              onOpenEntityLink: () => {},
              onOpenMessageLink: () => {},
              relayOrigin: null,
            },
          },
          createElement(
            MarkdownCodeBlock,
            { language },
            "on: schedule\ninterval: 15m\n",
          ),
        ),
      ),
    ),
  );
}

test("flag off + managed-agent author: renders no review button (plain YAML block)", async () => {
  await setRoutinesEnabled(false);
  const { screen } = await import("@testing-library/react");
  await renderCodeBlock({ authorIsManagedAgent: true });

  assert.equal(
    screen.queryByRole("button", { name: /review routine/i }),
    null,
  );
});

test("flag on + non-agent author: renders no review button", async () => {
  await setRoutinesEnabled(true);
  const { screen } = await import("@testing-library/react");
  await renderCodeBlock({ authorIsManagedAgent: false });

  assert.equal(
    screen.queryByRole("button", { name: /review routine/i }),
    null,
  );
});

test("flag on + managed-agent author: renders the review button for a buzz-routine block", async () => {
  await setRoutinesEnabled(true);
  const { screen } = await import("@testing-library/react");
  await renderCodeBlock({ authorIsManagedAgent: true });

  assert.ok(screen.getByRole("button", { name: /review routine/i }));
});

test("flag on + managed-agent author + non-routine language: renders no review button", async () => {
  await setRoutinesEnabled(true);
  const { screen } = await import("@testing-library/react");
  await renderCodeBlock({ authorIsManagedAgent: true, language: "yaml" });

  assert.equal(
    screen.queryByRole("button", { name: /review routine/i }),
    null,
  );
});

test("review button opens the create-workflow editor seeded with the block's YAML", async () => {
  await setRoutinesEnabled(true);
  const { screen, fireEvent } = await import("@testing-library/react");
  let openedYaml = null;
  await renderCodeBlock({
    authorIsManagedAgent: true,
    openNewWorkflow: (_channelId, yaml) => {
      openedYaml = yaml;
    },
  });

  fireEvent.click(screen.getByRole("button", { name: /review routine/i }));

  assert.equal(openedYaml, "on: schedule\ninterval: 15m");
});

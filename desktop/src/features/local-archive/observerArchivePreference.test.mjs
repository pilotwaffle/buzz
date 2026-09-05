import assert from "node:assert/strict";
import test from "node:test";

import {
  readExplicitObserverArchiveChoice,
  setExplicitObserverArchiveChoice,
} from "./observerArchivePreference.ts";

function withWindow(windowValue, run) {
  const previous = globalThis.window;
  globalThis.window = windowValue;
  try {
    return run();
  } finally {
    if (previous === undefined) {
      delete globalThis.window;
    } else {
      globalThis.window = previous;
    }
  }
}

test("fresh identity has no archive choice", () => {
  withWindow({ localStorage: { getItem: () => null } }, () =>
    assert.equal(readExplicitObserverArchiveChoice("pk1"), "unset"),
  );
});

test("stored archive choices remain identity scoped", () => {
  const values = new Map([
    ["buzz:observer-archive-default-seeded:pk-on", "1"],
    ["buzz:observer-archive-default-seeded:pk-off", "0"],
  ]);
  withWindow(
    { localStorage: { getItem: (key) => values.get(key) ?? null } },
    () => {
      assert.equal(readExplicitObserverArchiveChoice("pk-on"), true);
      assert.equal(readExplicitObserverArchiveChoice("pk-off"), false);
      assert.equal(readExplicitObserverArchiveChoice("pk-new"), "unset");
    },
  );
});

test("storage failure cannot enable activity retention", () => {
  withWindow(
    {
      localStorage: {
        getItem: () => {
          throw new Error("storage unavailable");
        },
      },
    },
    () => assert.equal(readExplicitObserverArchiveChoice("pk1"), false),
  );
});

test("unrecognized stored value cannot manufacture archive consent", () => {
  for (const raw of ["true", "yes", "2", "", " 1 "]) {
    withWindow({ localStorage: { getItem: () => raw } }, () =>
      assert.equal(readExplicitObserverArchiveChoice("pk1"), false),
    );
  }
});

test("explicit opt-in is persisted for only the selected identity", () => {
  const writes = [];
  withWindow(
    {
      localStorage: {
        setItem: (key, value) => writes.push([key, value]),
      },
    },
    () => setExplicitObserverArchiveChoice("pk1", true),
  );

  assert.deepEqual(writes, [["buzz:observer-archive-default-seeded:pk1", "1"]]);
});

import { expect, test } from "@playwright/test";

import { installMockBridge } from "../helpers/bridge";

async function openLocalArchiveSettings(page: import("@playwright/test").Page) {
  await page.goto("/", { waitUntil: "domcontentloaded" });
  await page.getByTestId("open-settings").click();
  await page.getByTestId("profile-popover-settings").click();
  await expect(page.getByTestId("settings-view")).toBeVisible();
  await page.getByTestId("settings-nav-local-archive").click();
  const card = page.getByTestId("settings-local-archive");
  await expect(card).toBeVisible({ timeout: 10_000 });
  return card;
}

async function observerArchiveMergeCount(
  page: import("@playwright/test").Page,
): Promise<number> {
  return page.evaluate(
    () =>
      (window.__BUZZ_E2E_COMMAND_LOG__ ?? []).filter((entry) => {
        if (entry.command !== "merge_save_subscription_kinds") return false;
        if (!entry.payload || typeof entry.payload !== "object") return false;
        return (entry.payload as Record<string, unknown>).kind === 24200;
      }).length,
  );
}

test.describe("observer archive policy — Settings toggle", () => {
  test("existing observer subscription remains enabled and checked", async ({
    page,
  }) => {
    // Preserve an existing owner-scoped subscription. The consent-default
    // change applies only to fresh identities and must not silently remove
    // retention that is already configured.
    await installMockBridge(page, {
      saveSubscriptions: [
        {
          scope_type: "owner_p",
          scope_value: "deadbeef".repeat(8),
          kinds: "[24200]",
        },
      ],
    });

    const card = await openLocalArchiveSettings(page);
    const toggle = card.getByTestId("local-archive-observer-toggle");
    await expect(toggle).toBeVisible({ timeout: 5_000 });
    await expect(card).toContainText(
      "Turning this off stops future saves but keeps existing history.",
    );
    await expect(toggle).toBeEnabled();
    await expect(toggle).toBeChecked();
  });

  test("toggle click OFF disables, then ON re-enables", async ({ page }) => {
    await installMockBridge(page, {
      saveSubscriptions: [
        {
          scope_type: "owner_p",
          scope_value: "deadbeef".repeat(8),
          kinds: "[24200]",
        },
      ],
    });

    const card = await openLocalArchiveSettings(page);
    const toggle = card.getByTestId("local-archive-observer-toggle");
    await expect(toggle).toBeVisible({ timeout: 5_000 });
    await expect(toggle).toBeChecked();

    // OFF: removes kind 24200.
    await toggle.click();
    await expect(toggle).not.toBeChecked();

    // ON again: re-creates the row from empty.
    await toggle.click();
    await expect(toggle).toBeChecked();
  });

  test("absent SQLite consent stays OFF despite a browser opt-out marker", async ({
    page,
  }) => {
    // Browser storage is deliberately irrelevant. The missing owner_p/24200
    // SQLite row is the authoritative OFF state.
    const MOCK_PUBKEY = "deadbeef".repeat(8);
    await page.addInitScript(
      ({ storageKey }) => {
        window.localStorage.setItem(storageKey, "0");
      },
      {
        storageKey: `buzz:observer-archive-default-seeded:${MOCK_PUBKEY}`,
      },
    );

    await installMockBridge(page, {
      saveSubscriptions: [],
    });

    const card = await openLocalArchiveSettings(page);
    const toggle = card.getByTestId("local-archive-observer-toggle");
    await expect(toggle).toBeVisible({ timeout: 5_000 });
    await expect(toggle).toBeEnabled();
    await expect(toggle).not.toBeChecked();
  });

  test("no subscriptions and no stored choice: defaults OFF and can opt in", async ({
    page,
  }) => {
    // A fresh identity with no stored choice and an empty subscription table
    // must remain OFF until the operator explicitly chooses retention.
    await installMockBridge(page, {
      saveSubscriptions: [],
    });

    const card = await openLocalArchiveSettings(page);
    const toggle = card.getByTestId("local-archive-observer-toggle");
    await expect(toggle).toBeVisible({ timeout: 5_000 });

    await expect(toggle).toBeEnabled();
    await expect(toggle).not.toBeChecked();

    expect(await observerArchiveMergeCount(page)).toBe(0);

    // The operator can still opt in; the row is created only after this click.
    await toggle.click();
    await expect(toggle).toBeChecked();
    expect(await observerArchiveMergeCount(page)).toBe(1);
  });

  test("browser-storage write failures cannot split ON/OFF consent", async ({
    page,
  }) => {
    await page.addInitScript(
      ({ observerKeyPrefix }) => {
        const win = window as Window & {
          __OBSERVER_MARKER_WRITE_ATTEMPTS__?: number;
        };
        win.__OBSERVER_MARKER_WRITE_ATTEMPTS__ = 0;
        const originalSetItem = Storage.prototype.setItem;
        Storage.prototype.setItem = function (key, value) {
          if (key.startsWith(observerKeyPrefix)) {
            win.__OBSERVER_MARKER_WRITE_ATTEMPTS__ =
              (win.__OBSERVER_MARKER_WRITE_ATTEMPTS__ ?? 0) + 1;
            throw new Error("observer marker storage unavailable");
          }
          return originalSetItem.call(this, key, value);
        };
      },
      { observerKeyPrefix: "buzz:observer-archive-default-seeded:" },
    );
    await installMockBridge(page, { saveSubscriptions: [] });

    const card = await openLocalArchiveSettings(page);
    const toggle = card.getByTestId("local-archive-observer-toggle");
    await expect(toggle).not.toBeChecked();

    await toggle.click();
    await expect(toggle).toBeChecked();
    await toggle.click();
    await expect(toggle).not.toBeChecked();

    const markerWriteAttempts = await page.evaluate(
      () =>
        (
          window as Window & {
            __OBSERVER_MARKER_WRITE_ATTEMPTS__?: number;
          }
        ).__OBSERVER_MARKER_WRITE_ATTEMPTS__ ?? 0,
    );
    expect(markerWriteAttempts).toBe(0);
  });
});

test.describe("observer archive policy — reconciliation gate", () => {
  test("archive sync reaches subscription path after reconciliation", async ({
    page,
  }) => {
    // The reconciliation gate (useObserverArchiveReconciliation) must resolve
    // successfully for a fresh identity, allowing useArchiveSync to start the
    // ArchiveSyncManager, which calls list_save_subscriptions.
    await installMockBridge(page, {
      saveSubscriptions: [
        {
          scope_type: "owner_p",
          scope_value: "deadbeef".repeat(8),
          kinds: "[24200]",
        },
      ],
    });

    await page.goto("/", { waitUntil: "domcontentloaded" });

    // Wait for the channel list (proves AppShell mounted fully).
    await expect(page.getByTestId("channel-general")).toBeVisible({
      timeout: 10_000,
    });

    // The IPC counter proves the subscription path was reached.
    await page.waitForFunction(
      () => {
        const counters = (window as Record<string, unknown>)
          .__BUZZ_E2E_IPC_COUNTERS__ as Record<string, number> | undefined;
        return (counters?.list_save_subscriptions ?? 0) > 0;
      },
      null,
      { timeout: 10_000 },
    );

    const count = await page.evaluate(() => {
      const counters = (window as Record<string, unknown>)
        .__BUZZ_E2E_IPC_COUNTERS__ as Record<string, number> | undefined;
      return counters?.list_save_subscriptions ?? 0;
    });
    expect(count).toBeGreaterThan(0);

    // The reconciliation gate must also result in a real `#p` + kind-24200
    // live REQ filter.
    const hasOwnerKindSubscription = await page.evaluate(
      (ownerPubkey) =>
        (
          window as Window & {
            __BUZZ_E2E_HAS_MOCK_OWNER_KIND_SUBSCRIPTION__?: (input: {
              ownerPubkey: string;
              kind: number;
            }) => boolean;
          }
        ).__BUZZ_E2E_HAS_MOCK_OWNER_KIND_SUBSCRIPTION__?.({
          ownerPubkey,
          kind: 24200,
        }) ?? false,
      "deadbeef".repeat(8),
    );
    expect(hasOwnerKindSubscription).toBe(true);
  });

  test("fresh install with empty subscriptions does not seed kind 24200", async ({
    page,
  }) => {
    // Wait for archive sync to pass its reconciliation gate, then prove the
    // unset choice did not manufacture an owner_p/24200 subscription.
    await installMockBridge(page, {
      saveSubscriptions: [],
    });

    await page.goto("/", { waitUntil: "domcontentloaded" });
    await expect(page.getByTestId("channel-general")).toBeVisible({
      timeout: 10_000,
    });

    await page.waitForFunction(
      () => {
        const counters = (window as Record<string, unknown>)
          .__BUZZ_E2E_IPC_COUNTERS__ as Record<string, number> | undefined;
        return (counters?.list_save_subscriptions ?? 0) > 0;
      },
      null,
      { timeout: 10_000 },
    );

    const hasObserverSubscription = await page.evaluate(
      (ownerPubkey) =>
        (
          window as Window & {
            __BUZZ_E2E_HAS_MOCK_OWNER_KIND_SUBSCRIPTION__?: (input: {
              ownerPubkey: string;
              kind: number;
            }) => boolean;
          }
        ).__BUZZ_E2E_HAS_MOCK_OWNER_KIND_SUBSCRIPTION__?.({
          ownerPubkey,
          kind: 24200,
        }) ?? false,
      "deadbeef".repeat(8),
    );
    expect(hasObserverSubscription).toBe(false);

    expect(await observerArchiveMergeCount(page)).toBe(0);
  });

  test("stale browser opt-in marker cannot recreate missing SQLite consent", async ({
    page,
  }) => {
    const mockPubkey = "deadbeef".repeat(8);
    await page.addInitScript(
      ({ storageKey }) => window.localStorage.setItem(storageKey, "1"),
      {
        storageKey: `buzz:observer-archive-default-seeded:${mockPubkey}`,
      },
    );
    await installMockBridge(page, { saveSubscriptions: [] });

    await page.goto("/", { waitUntil: "domcontentloaded" });
    await expect(page.getByTestId("channel-general")).toBeVisible({
      timeout: 10_000,
    });

    await page.waitForFunction(
      () => {
        const counters = (window as Record<string, unknown>)
          .__BUZZ_E2E_IPC_COUNTERS__ as Record<string, number> | undefined;
        return (counters?.list_save_subscriptions ?? 0) > 0;
      },
      null,
      { timeout: 10_000 },
    );

    const hasObserverSubscription = await page.evaluate(
      (ownerPubkey) =>
        (
          window as Window & {
            __BUZZ_E2E_HAS_MOCK_OWNER_KIND_SUBSCRIPTION__?: (input: {
              ownerPubkey: string;
              kind: number;
            }) => boolean;
          }
        ).__BUZZ_E2E_HAS_MOCK_OWNER_KIND_SUBSCRIPTION__?.({
          ownerPubkey,
          kind: 24200,
        }) ?? false,
      mockPubkey,
    );
    expect(hasObserverSubscription).toBe(false);
    expect(await observerArchiveMergeCount(page)).toBe(0);
  });
});

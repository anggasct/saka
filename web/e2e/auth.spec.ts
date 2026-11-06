import AxeBuilder from "@axe-core/playwright";
import { expect, test } from "@playwright/test";
import type { CDPSession, Page } from "@playwright/test";

const ORIGIN = "http://localhost:4173";
const AXE_TAGS = ["wcag2a", "wcag2aa", "wcag21a", "wcag21aa", "wcag22aa"];
const CHALLENGE = "q83vEjRWeJA_q83vEjRWeJA_q83vEjRWeJA_q83vEjQ";
const USER_ID = "AAECAwQFBgcICQoLDA0ODw";
const ACTOR = "operator:passkey:0193f0c4-0000-7000-8000-0000000000c1";
const AT = "2026-11-06T10:00:00.000Z";

type Mock = {
  signedIn: boolean;
  hasPasskey: boolean;
  credentialId: string | null;
  registered: Record<string, unknown> | null;
  assertion: Record<string, unknown> | null;
  pairings: number;
  devices: { id: string; name: string; kind: string }[];
};

function newMock(): Mock {
  return {
    signedIn: false,
    hasPasskey: false,
    credentialId: null,
    registered: null,
    assertion: null,
    pairings: 0,
    devices: [
      {
        id: "0193f0c4-0000-7000-8000-0000000000d1",
        name: "Local CLI",
        kind: "local_cli",
      },
      {
        id: "0193f0c4-0000-7000-8000-0000000000d2",
        name: "Pixel app",
        kind: "client",
      },
    ],
  };
}

async function mockApi(page: Page, mock: Mock) {
  await page.route("**/api/v1/**", async (route) => {
    const request = route.request();
    const path = new URL(request.url()).pathname;
    const json = (body: unknown, status = 200) =>
      route.fulfill({ status, json: body });
    const refuse = () => json({ code: "auth_required", params: {} }, 401);
    if (path === "/api/v1/events") {
      return route.fulfill({ status: 204 });
    }
    if (path === "/api/v1/auth/status") {
      return json({
        enabled: true,
        setup_required: !mock.hasPasskey,
        authenticated: mock.signedIn,
        actor: mock.signedIn ? ACTOR : null,
      });
    }
    if (path === "/api/v1/auth/passkeys/register/start") {
      return json({
        ceremony_id: "reg-1",
        options: {
          publicKey: {
            challenge: CHALLENGE,
            rp: { id: "localhost", name: "Saka" },
            user: { id: USER_ID, name: "operator", displayName: "Laptop" },
            pubKeyCredParams: [{ type: "public-key", alg: -7 }],
            authenticatorSelection: { userVerification: "required" },
            attestation: "none",
            timeout: 60000,
          },
        },
      });
    }
    if (path === "/api/v1/auth/passkeys/register/finish") {
      mock.registered = request.postDataJSON() as Record<string, unknown>;
      mock.credentialId = (mock.registered.credential as { id: string }).id;
      mock.hasPasskey = true;
      mock.signedIn = true;
      return json(
        {
          id: "0193f0c4-0000-7000-8000-0000000000c1",
          name: "Laptop",
          created_at: AT,
          last_used_at: null,
        },
        201,
      );
    }
    if (path === "/api/v1/auth/login/start") {
      return json({
        ceremony_id: "login-1",
        options: {
          publicKey: {
            challenge: CHALLENGE,
            rpId: "localhost",
            userVerification: "required",
            allowCredentials: [{ type: "public-key", id: mock.credentialId }],
            timeout: 60000,
          },
        },
      });
    }
    if (path === "/api/v1/auth/login/finish") {
      mock.assertion = request.postDataJSON() as Record<string, unknown>;
      mock.signedIn = true;
      return route.fulfill({ status: 204 });
    }
    if (path === "/api/v1/auth/logout") {
      mock.signedIn = false;
      return route.fulfill({ status: 204 });
    }
    if (!mock.signedIn) {
      return refuse();
    }
    if (path === "/api/v1/inbox") {
      return json({ items: [], next_cursor: null });
    }
    if (path === "/api/v1/inbox/summary") {
      return json({
        open: 0,
        by_kind: {
          decision: 0,
          approval: 0,
          permission: 0,
          blocker: 0,
          spec_unclear: 0,
        },
        running: 0,
        merged_today: 0,
      });
    }
    if (path === "/api/v1/projects") {
      return json({ items: [], next_cursor: null });
    }
    if (path === "/api/v1/work-items") {
      return json({ items: [], next_cursor: null });
    }
    if (path === "/api/v1/auth/devices") {
      return json({
        devices: mock.devices.map((d) => ({
          ...d,
          created_at: AT,
          last_used_at: d.kind === "local_cli" ? AT : null,
          revoked_at: null,
        })),
        credentials: mock.hasPasskey
          ? [
              {
                id: "0193f0c4-0000-7000-8000-0000000000c1",
                name: "Laptop",
                created_at: AT,
                last_used_at: null,
              },
            ]
          : [],
      });
    }
    if (path === "/api/v1/auth/pairings") {
      mock.pairings += 1;
      const purpose = (request.postDataJSON() as { purpose: string }).purpose;
      return json(
        {
          id: `p${mock.pairings}`,
          purpose,
          code: "WDJBMJHT12",
          expires_at: new Date(Date.now() + 300_000).toISOString(),
        },
        201,
      );
    }
    const revoke = /^\/api\/v1\/auth\/devices\/([^/]+)\/revoke$/.exec(path);
    if (revoke !== null) {
      mock.devices = mock.devices.filter((d) => d.id !== revoke[1]);
      return json({});
    }
    return json({ code: "not_found", params: {} }, 404);
  });
}

async function virtualAuthenticator(page: Page): Promise<CDPSession> {
  const cdp = await page.context().newCDPSession(page);
  await cdp.send("WebAuthn.enable");
  await cdp.send("WebAuthn.addVirtualAuthenticator", {
    options: {
      protocol: "ctap2",
      transport: "internal",
      hasResidentKey: true,
      hasUserVerification: true,
      isUserVerified: true,
      automaticPresenceSimulation: true,
    },
  });
  return cdp;
}

async function accessible(page: Page) {
  const results = await new AxeBuilder({ page }).withTags(AXE_TAGS).analyze();
  expect(results.violations).toEqual([]);
}

async function noOverflow(page: Page) {
  const overflow = await page.evaluate(
    () =>
      document.documentElement.scrollWidth -
      document.documentElement.clientWidth,
  );
  expect(overflow).toBeLessThanOrEqual(0);
}

async function registerFirstPasskey(page: Page) {
  await page.getByLabel("Code").fill("wdjb-mjht1");
  await page.getByLabel("Name of this device").fill("Laptop");
  await page.getByRole("button", { name: "Create passkey" }).click();
}

for (const width of [375, 1280]) {
  test(`a new instance sends the operator to setup, registers a passkey, and signs in again at ${width} px`, async ({
    page,
  }) => {
    await page.setViewportSize({ width, height: 900 });
    const mock = newMock();
    await mockApi(page, mock);
    await virtualAuthenticator(page);

    await page.goto(`${ORIGIN}/`);
    await expect(
      page.getByRole("heading", { level: 1, name: "Register a passkey" }),
    ).toBeVisible();
    await expect(page).toHaveURL(/\/setup\?next=%2F$|\/setup$/);
    await expect(page.getByText(/saka auth setup/)).toBeVisible();
    await expect(page.getByRole("link", { name: "Docs" })).toHaveCount(0);
    await noOverflow(page);
    await accessible(page);

    await registerFirstPasskey(page);
    await expect(
      page.getByRole("heading", { level: 1, name: "Inbox" }),
    ).toBeVisible();
    const credential = mock.registered?.credential as {
      response: { attestationObject: string; clientDataJSON: string };
    };
    expect(credential.response.attestationObject).toMatch(/^[A-Za-z0-9_-]+$/);
    expect(credential.response.clientDataJSON).toMatch(/^[A-Za-z0-9_-]+$/);
    expect(mock.registered?.ceremony_id).toBe("reg-1");

    await page.getByRole("button", { name: "Sign out" }).click();
    await expect(
      page.getByRole("heading", { level: 1, name: "Sign in" }),
    ).toBeVisible();
    await noOverflow(page);
    await accessible(page);

    await page.getByRole("button", { name: "Sign in with a passkey" }).click();
    await expect(
      page.getByRole("heading", { level: 1, name: "Inbox" }),
    ).toBeVisible();
    const assertion = mock.assertion?.credential as {
      response: { signature: string; authenticatorData: string };
    };
    expect(assertion.response.signature).toMatch(/^[A-Za-z0-9_-]+$/);
    expect(assertion.response.authenticatorData).toMatch(/^[A-Za-z0-9_-]+$/);
  });

  test(`a 401 leads to sign-in and back to the page that asked at ${width} px`, async ({
    page,
  }) => {
    await page.setViewportSize({ width, height: 900 });
    const mock = newMock();
    await mockApi(page, mock);
    await virtualAuthenticator(page);
    await page.goto(`${ORIGIN}/setup`);
    await registerFirstPasskey(page);
    await expect(
      page.getByRole("heading", { level: 1, name: "Inbox" }),
    ).toBeVisible();
    mock.signedIn = false;

    await page.goto(`${ORIGIN}/delivery?state=blocked`);
    await expect(
      page.getByRole("heading", { level: 1, name: "Sign in" }),
    ).toBeVisible();
    expect(new URL(page.url()).searchParams.get("next")).toBe(
      "/delivery?state=blocked",
    );
    await noOverflow(page);
    await accessible(page);
    await page.getByRole("button", { name: "Sign in with a passkey" }).click();
    await expect(page).toHaveURL(/\/delivery\?state=blocked$/);
  });
}

for (const width of [375, 1280]) {
  test(`Settings, Devices lists, pairs and revokes at ${width} px`, async ({
    page,
  }) => {
    await page.setViewportSize({ width, height: 900 });
    const mock = newMock();
    mock.hasPasskey = true;
    mock.signedIn = true;
    await mockApi(page, mock);
    await page.goto(`${ORIGIN}/settings/devices`);

    await expect(
      page.getByRole("heading", { level: 1, name: "Devices and sign-in" }),
    ).toBeVisible();
    const passkeys = page.getByRole("region", { name: "Passkeys" });
    await expect(passkeys.getByText("Laptop", { exact: true })).toBeVisible();
    const apps = page.getByRole("region", { name: "CLI and apps" });
    await expect(apps.getByText("Pixel app", { exact: true })).toBeVisible();
    await expect(
      apps.getByRole("button", { name: "Revoke Local CLI" }),
    ).toHaveCount(0);
    await noOverflow(page);
    await accessible(page);

    await apps.getByRole("button", { name: "Pair a CLI or app" }).click();
    const dialog = page.getByRole("dialog", { name: "Pairing code" });
    await expect(dialog.getByText("WDJBMJHT12")).toBeVisible();
    await expect(dialog.getByText("shown once")).toBeVisible();
    await accessible(page);
    await dialog.getByRole("button", { name: "Done" }).click();
    await expect(page.getByText("WDJBMJHT12")).toHaveCount(0);

    await apps.getByRole("button", { name: "Revoke Pixel app" }).click();
    const confirm = page.getByRole("dialog", { name: "Revoke Pixel app?" });
    await expect(confirm).toBeVisible();
    await accessible(page);
    await confirm.getByRole("button", { name: "Revoke" }).click();
    await expect(apps.getByText("Pixel app", { exact: true })).toHaveCount(0);
  });
}

test("sign-in and Devices follow the chosen language", async ({ page }) => {
  await page.setViewportSize({ width: 375, height: 900 });
  const mock = newMock();
  mock.hasPasskey = true;
  await mockApi(page, mock);
  await page.goto(`${ORIGIN}/login`);
  await expect(
    page.getByRole("heading", { level: 1, name: "Sign in" }),
  ).toBeVisible();
  await page
    .getByRole("combobox", { name: "Language" })
    .selectOption("Bahasa Indonesia");
  await expect(
    page.getByRole("heading", { level: 1, name: "Masuk" }),
  ).toBeVisible();
  await expect(
    page.getByRole("button", { name: "Masuk dengan passkey" }),
  ).toBeVisible();
  await accessible(page);

  mock.signedIn = true;
  await page.goto(`${ORIGIN}/settings/devices`);
  await expect(
    page.getByRole("heading", { level: 1, name: "Perangkat dan masuk" }),
  ).toBeVisible();
  await expect(page.getByRole("region", { name: "Passkey" })).toBeVisible();
  await accessible(page);
});

test("the app opens offline from the service worker and says it is disconnected", async ({
  page,
  context,
}) => {
  await page.setViewportSize({ width: 375, height: 900 });
  const mock = newMock();
  mock.hasPasskey = true;
  mock.signedIn = true;
  await mockApi(page, mock);
  await page.goto(`${ORIGIN}/`);
  await expect(
    page.getByRole("heading", { level: 1, name: "Inbox" }),
  ).toBeVisible();
  await expect(page.getByText("You are offline")).toHaveCount(0);
  await page.waitForFunction(() => navigator.serviceWorker.controller !== null);

  await context.setOffline(true);
  await page.reload();
  await expect(page.getByText("You are offline")).toBeVisible();
  await expect(page.getByRole("link", { name: "Settings" })).toBeVisible();
  await accessible(page);

  await context.setOffline(false);
  await expect(page.getByText("You are offline")).toHaveCount(0);
});

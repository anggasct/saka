import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import { platform } from "@/platform";
import {
  AUTHORIZATION,
  AUTHORIZATIONS,
  AUTHORSHIP,
  authorization,
  botAuthorship,
  LINK,
  soon,
  USER_TOKEN,
  userLink,
} from "@/test/authorship-fixtures";
import { fakeApi, page } from "@/test/fake-api";
import type { Routes } from "@/test/fake-api";
import { project, PROJECT_ID } from "@/test/fixtures";
import { renderApp } from "@/test/render";
import {
  PROJECT,
  PROJECTS,
  problemBody,
  SECRETS,
} from "@/test/settings-fixtures";

import type { Authorship } from "./queries";

const SETTINGS = `/settings/${PROJECT_ID}`;

function routes(extra: Routes = {}): Routes {
  return {
    [PROJECTS]: { body: page([project]) },
    [PROJECT]: { body: project },
    [SECRETS]: { body: { items: [] } },
    [AUTHORSHIP]: { body: botAuthorship },
    [LINK]: problemBody(404, "user_link_not_found"),
    ...extra,
  };
}

async function author() {
  return within(await screen.findByRole("region", { name: "Author" }));
}

afterEach(() => {
  vi.useRealTimers();
  vi.restoreAllMocks();
});

describe("Author mode", () => {
  it("keeps Save off for the current mode and sends the chosen one", async () => {
    let current = botAuthorship;
    const { requests } = fakeApi(
      routes({
        [AUTHORSHIP]: (request) => {
          if (request.method === "PUT") {
            current = { ...botAuthorship, mode: "user" };
          }
          return { body: current };
        },
      }),
    );
    renderApp(SETTINGS);
    const section = await author();
    const save = await section.findByRole("button", { name: "Save mode" });
    expect(save).toBeDisabled();
    await userEvent.click(section.getByLabelText(/My linked account/));
    expect(save).toBeEnabled();
    await userEvent.click(save);
    await waitFor(() => {
      expect(requests.some((r) => r.method === "PUT")).toBe(true);
    });
    const put = requests.find((r) => r.method === "PUT");
    expect(put?.body).toEqual({ mode: "user" });
    expect(put?.headers.get("Idempotency-Key")).toBeTruthy();
    await waitFor(() => {
      expect(section.getByRole("button", { name: "Save mode" })).toBeDisabled();
    });
  });

  it("names the code hosts that still need an account", async () => {
    fakeApi(
      routes({
        [AUTHORSHIP]: (request) =>
          request.method === "PUT"
            ? problemBody(409, "user_link_missing", {
                integrations: [
                  { integration_id: "github", state: "needs_relink" },
                ],
              })
            : { body: botAuthorship },
      }),
    );
    renderApp(SETTINGS);
    const section = await author();
    await userEvent.click(await section.findByLabelText(/My linked account/));
    await userEvent.click(section.getByRole("button", { name: "Save mode" }));
    expect(await section.findByRole("alert")).toHaveTextContent(
      "github (Needs linking again)",
    );
  });

  it("explains a missing token", async () => {
    fakeApi(
      routes({
        [AUTHORSHIP]: (request) =>
          request.method === "PUT"
            ? problemBody(409, "user_token_missing")
            : { body: botAuthorship },
      }),
    );
    renderApp(SETTINGS);
    const section = await author();
    await userEvent.click(
      await section.findByLabelText(/My personal access token/),
    );
    await userEvent.click(section.getByRole("button", { name: "Save mode" }));
    expect(await section.findByRole("alert")).toHaveTextContent(
      "The project has no user token yet.",
    );
  });

  it("says when no author is available", async () => {
    fakeApi(
      routes({ [AUTHORSHIP]: { body: { ...botAuthorship, author: null } } }),
    );
    renderApp(SETTINGS);
    const section = await author();
    expect(
      await section.findByText("No author available for this mode."),
    ).toBeInTheDocument();
  });
});

describe("Account link", () => {
  it("shows an unlinked account without an error card", async () => {
    fakeApi(routes());
    renderApp(SETTINGS);
    const section = await author();
    expect(await section.findByText("Not linked")).toBeInTheDocument();
    expect(section.queryByRole("alert")).not.toBeInTheDocument();
  });

  it("links with a device code, copies it, and polls until completed", async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    const user = userEvent.setup({ advanceTimers: vi.advanceTimersByTime });
    const copy = vi.spyOn(platform, "copyText").mockResolvedValue(true);
    let polls = 0;
    const { requests } = fakeApi(
      routes({
        [AUTHORIZATIONS]: { status: 202, body: authorization("pending") },
        [AUTHORIZATION]: () => {
          polls += 1;
          return {
            body:
              polls < 2
                ? authorization("pending")
                : authorization("completed", { link: userLink }),
          };
        },
      }),
    );
    renderApp(SETTINGS);
    const section = await author();
    await user.click(
      await section.findByRole("button", { name: "Link account" }),
    );
    const post = requests.find((r) => r.method === "POST");
    expect(post?.path).toBe(AUTHORIZATIONS);
    expect(post?.headers.get("Idempotency-Key")).toBeTruthy();
    expect(await section.findByText("WDJB-MJHT")).toBeInTheDocument();
    const open = section.getByRole("link", {
      name: /github\.com\/login\/device/,
    });
    expect(open).toHaveAttribute("rel", "noopener noreferrer");
    expect(open).toHaveAttribute("target", "_blank");
    expect(section.getByText(/expires in/)).toBeInTheDocument();
    await user.click(section.getByRole("button", { name: "Copy code" }));
    expect(copy).toHaveBeenCalledWith("WDJB-MJHT");
    expect(await section.findByText("Copied")).toBeInTheDocument();

    const get = () => requests.filter((r) => r.path === AUTHORIZATION).length;
    expect(get()).toBe(0);
    await vi.advanceTimersByTimeAsync(5000);
    await waitFor(() => {
      expect(get()).toBe(1);
    });
    await vi.advanceTimersByTimeAsync(5000);
    expect(await section.findByText("Linked as octo")).toBeInTheDocument();
    expect(section.queryByText("WDJB-MJHT")).not.toBeInTheDocument();
    await waitFor(() => {
      expect(
        requests.filter((r) => r.path === AUTHORSHIP && r.method === "GET")
          .length,
      ).toBeGreaterThan(1);
    });
  });

  it("says copying failed", async () => {
    vi.spyOn(platform, "copyText").mockResolvedValue(false);
    fakeApi(
      routes({
        [AUTHORIZATIONS]: { status: 202, body: authorization("pending") },
      }),
    );
    renderApp(SETTINGS);
    const section = await author();
    await userEvent.click(
      await section.findByRole("button", { name: "Link account" }),
    );
    await userEvent.click(
      await section.findByRole("button", { name: "Copy code" }),
    );
    expect(await section.findByText("Copy failed")).toBeInTheDocument();
  });

  it.each([
    [authorization("expired"), "The code expired before it was entered."],
    [authorization("denied"), "The request was denied at the code host."],
    [
      authorization("failed", {
        failure: { code: "device_flow_disabled", params: {} },
      }),
      "Device flow is turned off in the GitHub App settings.",
    ],
  ])("ends with a sentence and Try again (%#)", async (ended, sentence) => {
    fakeApi(
      routes({
        [AUTHORIZATIONS]: { status: 202, body: ended },
      }),
    );
    renderApp(SETTINGS);
    const section = await author();
    await userEvent.click(
      await section.findByRole("button", { name: "Link account" }),
    );
    expect(await section.findByRole("alert")).toHaveTextContent(sentence);
    expect(
      section.getByRole("button", { name: "Try again" }),
    ).toBeInTheDocument();
  });

  it("explains an unconfigured device flow", async () => {
    fakeApi(
      routes({
        [AUTHORIZATIONS]: problemBody(409, "device_flow_unconfigured", {
          integration_id: "github",
        }),
      }),
    );
    renderApp(SETTINGS);
    const section = await author();
    await userEvent.click(
      await section.findByRole("button", { name: "Link account" }),
    );
    expect(await section.findByRole("alert")).toHaveTextContent("client_id");
  });

  it("shows a linked account and the projects that block unlinking", async () => {
    const { requests } = fakeApi(
      routes({
        [LINK]: (request) =>
          request.method === "DELETE"
            ? problemBody(409, "user_link_in_use", {
                projects: [{ project_id: PROJECT_ID, name: "tasks" }],
              })
            : {
                body: { ...userLink, refresh_expires_at: soon(3) },
              },
      }),
    );
    renderApp(SETTINGS);
    const section = await author();
    expect(await section.findByText("octo")).toBeInTheDocument();
    expect(section.getByText("Expires soon")).toBeInTheDocument();
    await userEvent.click(section.getByRole("button", { name: "Unlink" }));
    const dialog = await screen.findByRole("dialog");
    await userEvent.click(
      within(dialog).getByRole("button", { name: "Unlink" }),
    );
    expect(await within(dialog).findByRole("alert")).toHaveTextContent(
      "(tasks)",
    );
    expect(requests.some((r) => r.method === "DELETE")).toBe(true);
  });

  it("offers to link again when the link needs it", async () => {
    fakeApi(
      routes({
        [LINK]: {
          body: { ...userLink, state: "needs_relink", relink_cause: "revoked" },
        },
      }),
    );
    renderApp(SETTINGS);
    const section = await author();
    expect(
      await section.findByText("The link was revoked at the code host."),
    ).toBeInTheDocument();
    expect(
      section.getByRole("button", { name: "Link again" }),
    ).toBeInTheDocument();
  });
});

describe("Personal access token", () => {
  const withToken = (
    token: NonNullable<Authorship["user_token"]>,
  ): Authorship => ({ ...botAuthorship, user_token: token });
  const token = {
    integration_id: "github",
    login: "octo",
    provider_user_id: 1,
    expires_at: null,
    set_at: "2026-11-03T10:00:00.000Z",
    state: "active",
    invalid_cause: null,
  };

  it("stores a token write-only", async () => {
    const { requests } = fakeApi(
      routes({
        [USER_TOKEN]: { body: withToken(token) },
      }),
    );
    const { queryClient, container } = renderApp(SETTINGS);
    const section = await author();
    expect(await section.findByText("No token stored.")).toBeInTheDocument();
    await userEvent.click(section.getByRole("button", { name: "Set token" }));
    const dialog = await screen.findByRole("dialog");
    await userEvent.type(
      within(dialog).getByLabelText("Token"),
      "github_pat_x",
    );
    await userEvent.click(
      within(dialog).getByRole("button", { name: "Save token" }),
    );
    await waitFor(() => {
      expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
    });
    const put = requests.find((r) => r.method === "PUT");
    expect(put?.path).toBe(USER_TOKEN);
    expect(put?.body).toEqual({ value: "github_pat_x" });
    expect(container.ownerDocument.body.innerHTML).not.toContain(
      "github_pat_x",
    );
    expect(
      JSON.stringify(
        queryClient
          .getQueryCache()
          .getAll()
          .map((q) => [q.queryKey, q.state.data]),
      ),
    ).not.toContain("github_pat_x");
  });

  it.each([
    [
      problemBody(422, "user_token_not_fine_grained"),
      "Use a fine-grained personal access token",
    ],
    [
      problemBody(422, "user_token_rejected", { status: 401 }),
      "refused the token (HTTP 401)",
    ],
    [
      problemBody(422, "user_token_repo_inaccessible", {
        repos: [{ name: "web", full_name: "acme/web", status: 404 }],
      }),
      "acme/web (404)",
    ],
    [
      problemBody(409, "user_token_multi_integration", {
        integrations: ["github", "ghe"],
      }),
      "github, ghe",
    ],
  ])("explains a refused token (%#)", async (failure, sentence) => {
    fakeApi(routes({ [USER_TOKEN]: failure }));
    renderApp(SETTINGS);
    const section = await author();
    await userEvent.click(
      await section.findByRole("button", { name: "Set token" }),
    );
    const dialog = await screen.findByRole("dialog");
    await userEvent.type(within(dialog).getByLabelText("Token"), "nope");
    await userEvent.click(
      within(dialog).getByRole("button", { name: "Save token" }),
    );
    expect(await within(dialog).findByRole("alert")).toHaveTextContent(
      sentence,
    );
  });

  it("flags a token that expires soon and one without expiry", async () => {
    fakeApi(
      routes({
        [AUTHORSHIP]: { body: withToken({ ...token, expires_at: soon(3) }) },
      }),
    );
    renderApp(SETTINGS);
    const section = await author();
    expect(await section.findByText("Expires soon")).toBeInTheDocument();
    expect(
      section.getByRole("button", { name: "Replace token" }),
    ).toBeInTheDocument();
  });

  it("shows no expiry", async () => {
    fakeApi(routes({ [AUTHORSHIP]: { body: withToken(token) } }));
    renderApp(SETTINGS);
    const section = await author();
    expect(await section.findByText("No expiry")).toBeInTheDocument();
    expect(section.queryByText("Expires soon")).not.toBeInTheDocument();
  });

  it("explains a token that is still in use when removing it", async () => {
    fakeApi(
      routes({
        [AUTHORSHIP]: { body: withToken(token) },
        [USER_TOKEN]: problemBody(409, "user_token_in_use"),
      }),
    );
    renderApp(SETTINGS);
    const section = await author();
    await userEvent.click(
      await section.findByRole("button", { name: "Remove token" }),
    );
    const dialog = await screen.findByRole("dialog");
    await userEvent.click(
      within(dialog).getByRole("button", { name: "Remove" }),
    );
    expect(await within(dialog).findByRole("alert")).toHaveTextContent(
      "The project authors changes with this token.",
    );
  });
});

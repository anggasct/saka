import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it } from "vitest";

import { startEventStream } from "@/api/events";
import { fakeApi, page } from "@/test/fake-api";
import type { Recorded, Route, Routes } from "@/test/fake-api";
import { fakeStream } from "@/test/fake-stream";
import {
  blockedTask,
  detail,
  permission,
  project,
  PROJECT_ID,
  spec,
} from "@/test/fixtures";
import { renderWithProviders } from "@/test/render";

import { DeliveryPage } from "./delivery-page";
import type { PermissionRequest } from "./queries";
import { TaskPage } from "./task-page";

const PERMISSIONS = "/api/v1/permission-requests";
const SESSION_ID = "0193f0c4-1b2a-7c3d-9e8f-0011223344f1";
const OTHER_PROJECT = "0193f0c4-1b2a-7c3d-9e8f-0011223344ff";

const bash = permission(1);
const edit = permission(2, {
  tool: "Edit",
  summary: "src/store.ts",
  input: { file_path: "src/store.ts" },
});

function answerPath(request: PermissionRequest): string {
  return `${PERMISSIONS}/${request.id}/answer`;
}

function deliveryRoutes(
  pending: () => PermissionRequest[],
  extra: Routes = {},
): Routes {
  return {
    "/api/v1/projects": { body: page([project]) },
    [`/api/v1/projects/${PROJECT_ID}/specs`]: { body: page([spec]) },
    "/api/v1/work-items": { body: page([blockedTask]) },
    [PERMISSIONS]: () => ({ body: page(pending()) }),
    ...extra,
  };
}

function answers(requests: Recorded[]): unknown[] {
  return requests
    .filter((r) => r.method === "POST" && r.path.endsWith("/answer"))
    .map((r) => r.body);
}

function permissionReads(requests: Recorded[]): number {
  return requests.filter((r) => r.method === "GET" && r.path === PERMISSIONS)
    .length;
}

async function panel() {
  return screen.findByRole("region", { name: /^Waiting for you/ });
}

async function card(tool: string) {
  return within(await panel()).findByRole("article", {
    name: `${tool} asks for permission`,
  });
}

const OK: Route = { body: bash };

describe("permission panel", () => {
  it("lists pending requests oldest first with their project and task", async () => {
    fakeApi(deliveryRoutes(() => [edit, bash]));
    renderWithProviders(<DeliveryPage />);
    const region = await panel();
    expect(
      within(region).getByRole("heading", { name: "Waiting for you (2)" }),
    ).toBeInTheDocument();
    const cards = within(region).getAllByRole("article");
    expect(
      cards.map((c) => within(c).getByRole("heading").textContent),
    ).toEqual(["Bash asks for permission", "Edit asks for permission"]);
    const first = cards[0];
    if (first === undefined) return;
    expect(within(first).getByText("npm install left-pad")).toBeInTheDocument();
    expect(within(first).getByText("tasks")).toBeInTheDocument();
    expect(
      await within(first).findByRole("link", { name: "Step S1" }),
    ).toHaveAttribute("href", `/delivery/tasks/${blockedTask.id}`);
    expect(
      within(first).getByText(/"command": "npm install left-pad"/),
    ).toBeInTheDocument();
  });

  it("is not rendered without pending requests", async () => {
    fakeApi(deliveryRoutes(() => []));
    renderWithProviders(<DeliveryPage />);
    await screen.findByRole("heading", { name: "Mark tasks as done" });
    expect(
      screen.queryByRole("region", { name: /^Waiting for you/ }),
    ).not.toBeInTheDocument();
  });

  it("falls back to an open task link outside the listed projects", async () => {
    fakeApi(
      deliveryRoutes(() => [permission(1, { project_id: OTHER_PROJECT })]),
    );
    renderWithProviders(<DeliveryPage />);
    const found = await card("Bash");
    expect(
      within(found).getByRole("link", { name: "Open task" }),
    ).toBeVisible();
    expect(within(found).queryByText("tasks")).not.toBeInTheDocument();
  });

  it("allows once without a dialog", async () => {
    const { requests } = fakeApi(
      deliveryRoutes(() => [bash], { [answerPath(bash)]: OK }),
    );
    renderWithProviders(<DeliveryPage />);
    await userEvent.click(
      within(await card("Bash")).getByRole("button", { name: "Allow" }),
    );
    await waitFor(() => {
      expect(answers(requests)).toEqual([
        { decision: "allow", message: null, remember: false },
      ]);
    });
    const post = requests.find((r) => r.method === "POST");
    expect(post?.headers.get("Idempotency-Key")).toMatch(/^[0-9a-f-]{36}$/);
  });

  it("denies with an optional message", async () => {
    const { requests } = fakeApi(
      deliveryRoutes(() => [bash], { [answerPath(bash)]: OK }),
    );
    renderWithProviders(<DeliveryPage />);
    const deny = async (message: string) => {
      await userEvent.click(
        within(await card("Bash")).getByRole("button", { name: "Deny" }),
      );
      const dialog = screen.getByRole("dialog", { name: "Deny the request" });
      if (message !== "") {
        await userEvent.type(
          within(dialog).getByLabelText("Message to the agent (optional)"),
          message,
        );
      }
      await userEvent.click(
        within(dialog).getByRole("button", { name: "Deny" }),
      );
      await waitFor(() => {
        expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
      });
    };
    await deny("");
    await deny("use pnpm");
    expect(answers(requests)).toEqual([
      { decision: "deny", message: null, remember: false },
      { decision: "deny", message: "use pnpm", remember: false },
    ]);
  });

  it("shows the rule an always-allow creates", async () => {
    const { requests } = fakeApi(
      deliveryRoutes(() => [bash, edit], {
        [answerPath(bash)]: OK,
        [answerPath(edit)]: OK,
      }),
    );
    renderWithProviders(<DeliveryPage />);
    await userEvent.click(
      within(await card("Edit")).getByRole("button", {
        name: "Allow always for this repo",
      }),
    );
    let dialog = screen.getByRole("dialog");
    expect(
      within(dialog).getByText("Always allow every Edit request in tasks."),
    ).toBeInTheDocument();
    await userEvent.click(
      within(dialog).getByRole("button", { name: "Cancel" }),
    );
    await userEvent.click(
      within(await card("Bash")).getByRole("button", {
        name: "Allow always for this repo",
      }),
    );
    dialog = screen.getByRole("dialog");
    expect(
      within(dialog).getByText("Always allow npm install left-pad in tasks."),
    ).toBeInTheDocument();
    await userEvent.click(
      within(dialog).getByRole("button", { name: "Allow always" }),
    );
    await waitFor(() => {
      expect(answers(requests)).toEqual([
        { decision: "allow", message: null, remember: true },
      ]);
    });
  });

  it("offers to allow once a command that holds a secret", async () => {
    const { requests } = fakeApi(
      deliveryRoutes(() => [bash], {
        [answerPath(bash)]: (r) =>
          (r.body as { remember: boolean }).remember
            ? {
                status: 422,
                body: {
                  code: "validation_failed",
                  params: {
                    errors: [{ field: "remember", code: "contains_secret" }],
                  },
                },
              }
            : OK,
      }),
    );
    renderWithProviders(<DeliveryPage />);
    await userEvent.click(
      within(await card("Bash")).getByRole("button", {
        name: "Allow always for this repo",
      }),
    );
    const dialog = screen.getByRole("dialog");
    await userEvent.click(
      within(dialog).getByRole("button", { name: "Allow always" }),
    );
    expect(
      await within(dialog).findByText(/contains a secret/),
    ).toBeInTheDocument();
    await userEvent.click(
      within(dialog).getByRole("button", { name: "Allow once" }),
    );
    await waitFor(() => {
      expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
    });
    expect(answers(requests)).toEqual([
      { decision: "allow", message: null, remember: true },
      { decision: "allow", message: null, remember: false },
    ]);
    const keys = requests
      .filter((r) => r.method === "POST")
      .map((r) => r.headers.get("Idempotency-Key"));
    expect(new Set(keys).size).toBe(2);
  });

  it("says when another client answered first and refreshes", async () => {
    const { requests } = fakeApi(
      deliveryRoutes(() => [bash], {
        [answerPath(bash)]: {
          status: 409,
          body: { code: "permission_already_answered", params: {} },
        },
      }),
    );
    renderWithProviders(<DeliveryPage />);
    const found = await card("Bash");
    const reads = permissionReads(requests);
    await userEvent.click(within(found).getByRole("button", { name: "Allow" }));
    expect(
      await within(found).findByText("Already answered."),
    ).toBeInTheDocument();
    await waitFor(() => {
      expect(permissionReads(requests)).toBeGreaterThan(reads);
    });
  });

  it("refetches when the stream reports an answer", async () => {
    let pending = [bash];
    const { requests } = fakeApi(deliveryRoutes(() => pending));
    const fake = fakeStream();
    const { queryClient } = renderWithProviders(<DeliveryPage />);
    startEventStream(queryClient, () => fake.stream);
    await card("Bash");
    pending = [];
    const reads = permissionReads(requests);
    fake.emit("runtime.permission.answered.v1", {
      seq: 9,
      stream_type: "permission_request",
    });
    await waitFor(() => {
      expect(
        screen.queryByRole("region", { name: /^Waiting for you/ }),
      ).not.toBeInTheDocument();
    });
    expect(permissionReads(requests)).toBeGreaterThan(reads);
  });
});

describe("permission in the timeline", () => {
  it("renders the card while pending and the summary once answered", async () => {
    let pending = [bash];
    const item = detail({
      ...blockedTask,
      state: "in_stage",
      stage: "dev",
      blocker: null,
      pending_permission_count: 1,
    });
    const { requests } = fakeApi({
      [`/api/v1/work-items/${item.id}`]: { body: item },
      [`/api/v1/specs/${spec.id}`]: { body: spec },
      [`/api/v1/projects/${PROJECT_ID}`]: { body: project },
      "/api/v1/work-items": { body: page([item]) },
      "/api/v1/sessions": {
        body: page([
          {
            id: SESSION_ID,
            project_id: PROJECT_ID,
            work_item_id: item.id,
            stage_run_id: "0193f0c4-1b2a-7c3d-9e8f-0011223344c1",
            cli: "claude",
            cli_version: null,
            model: null,
            state: "waiting",
            turns: 1,
            running_ms: 1000,
            cost_usd: null,
            input_tokens: null,
            output_tokens: null,
            resumed_from_session_id: null,
            resume_cause: null,
            end_reason: null,
            available_actions: ["stop"],
            started_at: "2026-10-31T10:00:00.000Z",
            ended_at: null,
          },
        ]),
      },
      [`/api/v1/sessions/${SESSION_ID}/timeline`]: {
        body: {
          items: [
            {
              offset: 0,
              at: "2026-10-31T10:01:00.000Z",
              kind: "permission",
              summary: "npm install left-pad",
              data: { permission_request_id: bash.id, state: "pending" },
            },
          ],
          prev_before: null,
          next_after: null,
        },
      },
      [PERMISSIONS]: () => ({ body: page(pending) }),
      [answerPath(bash)]: () => {
        pending = [];
        return OK;
      },
    });
    renderWithProviders(<TaskPage workItemId={item.id} />);
    const log = await screen.findByRole("log");
    const found = await within(log).findByRole("article", {
      name: "Bash asks for permission",
    });
    expect(
      screen.getByRole("link", { name: "1 permission request waiting" }),
    ).toHaveAttribute("href", `/delivery/tasks/${item.id}#${found.id}`);
    const scoped = requests.find(
      (r) => r.path === PERMISSIONS && r.search.has("work_item_id"),
    );
    expect(scoped?.search.get("work_item_id")).toBe(item.id);
    expect(scoped?.search.get("state")).toBe("pending");
    await userEvent.click(within(found).getByRole("button", { name: "Allow" }));
    await waitFor(() => {
      expect(within(log).queryByRole("article")).not.toBeInTheDocument();
    });
    expect(within(log).getByText("npm install left-pad")).toBeInTheDocument();
    expect(within(log).getByText("Waiting for an answer")).toBeInTheDocument();
  });
});

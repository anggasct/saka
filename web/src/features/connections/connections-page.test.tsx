import { screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import { platform } from "@/platform";
import type { Connections, NewConnection, SavedConnection } from "@/platform";
import { ConnectionError } from "@/platform/connection-error";
import { renderWithProviders } from "@/test/render";

import { ConnectionsPage } from "./connections-page";

const OFFICE: SavedConnection = {
  id: "a",
  name: "Office",
  url: "https://saka.example.com",
  active: true,
};
const HOME: SavedConnection = {
  id: "b",
  name: "Home",
  url: "http://127.0.0.1:7420",
  active: false,
};

function shell(
  initial: SavedConnection[],
  overrides: Partial<Connections> = {},
) {
  let list = initial;
  const fake: Connections = {
    hasActive: () => list.some((c) => c.active),
    list: vi.fn(() => Promise.resolve(list)),
    add: vi.fn((input: NewConnection) => {
      const saved = {
        id: "n",
        name: input.name,
        url: input.url,
        active: list.length === 0,
      };
      list = [...list, saved];
      return Promise.resolve(saved);
    }),
    remove: vi.fn((id: string) => {
      list = list.filter((c) => c.id !== id);
      return Promise.resolve();
    }),
    select: vi.fn(() => Promise.resolve()),
    ...overrides,
  };
  vi.spyOn(platform, "connections").mockReturnValue(fake);
  return fake;
}

describe("Connections page", () => {
  afterEach(() => {
    vi.restoreAllMocks();
  });

  it("lists the saved servers and marks the active one", async () => {
    shell([OFFICE, HOME]);
    renderWithProviders(<ConnectionsPage mode="manage" />);
    expect(await screen.findByText("Office")).toBeVisible();
    expect(screen.getByText("Active")).toBeVisible();
    expect(screen.getByText("http://127.0.0.1:7420")).toBeVisible();
    expect(screen.getByRole("button", { name: "Use Home" })).toBeVisible();
    expect(screen.queryByRole("button", { name: "Use Office" })).toBeNull();
  });

  it("pairs a first server and selects it, which restarts the app", async () => {
    const fake = shell([]);
    renderWithProviders(<ConnectionsPage mode="setup" />);
    const user = userEvent.setup();
    expect(
      await screen.findByRole("heading", { name: "Connect to a Saka server" }),
    ).toBeVisible();
    await user.type(screen.getByLabelText("Name"), "Office");
    await user.type(
      screen.getByLabelText("Server address"),
      "https://saka.example.com",
    );
    await user.type(screen.getByLabelText("Pairing code"), "wdjb-mjht");
    await user.click(screen.getByRole("button", { name: "Pair and save" }));
    await waitFor(() => {
      expect(fake.select).toHaveBeenCalledWith("n");
    });
    expect(fake.add).toHaveBeenCalledWith({
      name: "Office",
      url: "https://saka.example.com",
      pairingCode: "wdjb-mjht",
    });
  });

  it("explains a refusal and keeps the form", async () => {
    shell([OFFICE], {
      add: vi.fn(() => Promise.reject(new ConnectionError("insecure_url"))),
    });
    renderWithProviders(<ConnectionsPage mode="manage" />);
    const user = userEvent.setup();
    await user.type(await screen.findByLabelText("Name"), "Lab");
    await user.type(screen.getByLabelText("Server address"), "http://lab.test");
    await user.type(screen.getByLabelText("Pairing code"), "CODE");
    await user.click(screen.getByRole("button", { name: "Pair and save" }));
    expect(await screen.findByRole("alert")).toHaveTextContent(
      "Plain http:// is only accepted for this machine",
    );
    expect(screen.getByLabelText("Server address")).toHaveValue(
      "http://lab.test",
    );
  });

  it("asks twice before removing a server", async () => {
    const fake = shell([OFFICE, HOME]);
    renderWithProviders(<ConnectionsPage mode="manage" />);
    const user = userEvent.setup();
    await user.click(
      await screen.findByRole("button", { name: "Remove Home" }),
    );
    expect(fake.remove).not.toHaveBeenCalled();
    await user.click(
      screen.getByRole("button", { name: "Confirm removing Home" }),
    );
    await waitFor(() => {
      expect(fake.remove).toHaveBeenCalledWith("b");
    });
    await waitFor(() => {
      expect(screen.queryByText("Home")).toBeNull();
    });
  });

  it("switches to another server with Use", async () => {
    const fake = shell([OFFICE, HOME]);
    renderWithProviders(<ConnectionsPage mode="manage" />);
    await userEvent
      .setup()
      .click(await screen.findByRole("button", { name: "Use Home" }));
    expect(fake.select).toHaveBeenCalledWith("b");
  });

  it("asks for a new code for the active server when the token was revoked", async () => {
    shell([OFFICE]);
    renderWithProviders(<ConnectionsPage mode="reauth" />);
    expect(
      await screen.findByRole("heading", { name: "Pair again" }),
    ).toBeVisible();
    await waitFor(() => {
      expect(screen.getByLabelText("Server address")).toHaveValue(
        "https://saka.example.com",
      );
    });
    expect(screen.getByLabelText("Name")).toHaveValue("Office");
  });

  it("renders nothing on the web platform", () => {
    const { container } = renderWithProviders(
      <ConnectionsPage mode="manage" />,
    );
    expect(container).toBeEmptyDOMElement();
  });
});

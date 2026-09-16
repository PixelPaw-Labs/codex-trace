import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { ApiClient } from "../../shared/types";

const invokeMock = vi.fn();
vi.mock("../lib/invoke", () => ({ invoke: (...args: unknown[]) => invokeMock(...args) }));

const { AcceptedClients } = await import("./AcceptedClients");

function client(overrides: Partial<ApiClient> = {}): ApiClient {
  return {
    id: "11111111-1111-4111-8111-111111111111",
    name: "web-ui",
    builtin: true,
    created_at: 1_757_000_000,
    issued_at: 1_757_000_000,
    revoked_at: null,
    ...overrides,
  };
}

function renderClients(clients: ApiClient[], props: Record<string, unknown> = {}) {
  const onClientsChanged = vi.fn();
  render(
    <AcceptedClients
      clients={clients}
      authEnabled
      authSource="file"
      onClientsChanged={onClientsChanged}
      {...props}
    />,
  );
  return { onClientsChanged };
}

describe("AcceptedClients", () => {
  beforeEach(() => {
    invokeMock.mockReset();
    invokeMock.mockResolvedValue([]);
  });

  it("lists each client with a named-month issue date", () => {
    renderClients([client()]);
    expect(screen.getByText("web-ui")).toBeInTheDocument();
    // A named month reads the same everywhere, whichever way round the locale
    // orders it; an all-numeric 08/09/2026 is two different dates.
    const issued = screen.getByText(/^Issued /).textContent ?? "";
    expect(issued).toMatch(/[A-Za-z]{3,}/);
    expect(issued).toMatch(/\d{4}/);
  });

  it("marks built-in and revoked clients", () => {
    renderClients([client(), client({ id: "b", name: "ci", builtin: false, revoked_at: 1 })]);
    expect(screen.getByText("built-in")).toBeInTheDocument();
    expect(screen.getByText("revoked")).toBeInTheDocument();
  });

  it("needs a second click before revoking", () => {
    renderClients([client({ builtin: false })]);

    fireEvent.click(screen.getByRole("button", { name: "Revoke" }));
    expect(invokeMock).not.toHaveBeenCalledWith("revoke_client", expect.anything());

    fireEvent.click(screen.getByRole("button", { name: "Confirm revoke" }));
    expect(invokeMock).toHaveBeenCalledWith("revoke_client", {
      id: "11111111-1111-4111-8111-111111111111",
    });
  });

  it("needs a second click before reissuing", () => {
    invokeMock.mockResolvedValue({ client: client(), credential: "a.b.c" });
    renderClients([client()]);

    fireEvent.click(screen.getByRole("button", { name: "Reissue" }));
    expect(invokeMock).not.toHaveBeenCalledWith("reissue_client", expect.anything());

    fireEvent.click(screen.getByRole("button", { name: "Confirm reissue" }));
    expect(invokeMock).toHaveBeenCalledWith("reissue_client", {
      id: "11111111-1111-4111-8111-111111111111",
    });
  });

  it("cannot revoke an already-revoked client", () => {
    renderClients([client({ builtin: false, revoked_at: 99 })]);
    expect(screen.getByRole("button", { name: "Revoke" })).toBeDisabled();
  });

  it("shows a newly registered credential exactly once", async () => {
    invokeMock.mockImplementation((cmd: string) => {
      if (cmd === "register_client") {
        return Promise.resolve({ client: client({ name: "ci" }), credential: "new.cred.value" });
      }
      return Promise.resolve([]);
    });
    renderClients([]);

    fireEvent.change(screen.getByLabelText("New client name"), { target: { value: "ci" } });
    fireEvent.click(screen.getByRole("button", { name: "Add client" }));

    expect(await screen.findByText("new.cred.value")).toBeInTheDocument();
    expect(screen.getByText(/only time it is shown/)).toBeInTheDocument();

    fireEvent.click(screen.getByRole("button", { name: "Done" }));
    expect(screen.queryByText("new.cred.value")).not.toBeInTheDocument();
  });

  it("refreshes the client list after a mutation", async () => {
    const refreshed = [client({ name: "ci", builtin: false })];
    invokeMock.mockImplementation((cmd: string) => {
      if (cmd === "register_client") {
        return Promise.resolve({ client: refreshed[0], credential: "c" });
      }
      return Promise.resolve(refreshed);
    });
    const { onClientsChanged } = renderClients([]);

    fireEvent.change(screen.getByLabelText("New client name"), { target: { value: "ci" } });
    fireEvent.click(screen.getByRole("button", { name: "Add client" }));

    await waitFor(() => expect(onClientsChanged).toHaveBeenCalledWith(refreshed));
  });

  it("surfaces a backend rejection", async () => {
    invokeMock.mockImplementation((cmd: string) => {
      if (cmd === "register_client") {
        return Promise.reject(new Error('a client named "ci" already exists'));
      }
      return Promise.resolve([]);
    });
    renderClients([]);

    fireEvent.change(screen.getByLabelText("New client name"), { target: { value: "ci" } });
    fireEvent.click(screen.getByRole("button", { name: "Add client" }));

    expect(await screen.findByText(/already exists/)).toBeInTheDocument();
  });

  it("does not register a blank name", () => {
    renderClients([]);
    fireEvent.change(screen.getByLabelText("New client name"), { target: { value: "  " } });
    expect(screen.getByRole("button", { name: "Add client" })).toBeDisabled();
    expect(invokeMock).not.toHaveBeenCalledWith("register_client", expect.anything());
  });

  it("explains itself instead of offering controls when verification is off", () => {
    renderClients([], { authEnabled: false, authSource: "disabled" });
    expect(screen.getByText(/CODEXTRACE_API_AUTH=off/)).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Add client" })).not.toBeInTheDocument();
  });

  it("warns that an ephemeral key does not survive a restart", () => {
    renderClients([client()], { authSource: "ephemeral" });
    expect(screen.getByText(/one-off signing key/)).toBeInTheDocument();
  });
});

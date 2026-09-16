import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { FONT_SCALE_PRESETS, formatFontScale } from "../lib/fontScale";
import type { SettingsResponse } from "../../shared/types";

const invokeMock = vi.fn();
vi.mock("../lib/invoke", () => ({ invoke: (...args: unknown[]) => invokeMock(...args) }));

const { SettingsModal } = await import("./SettingsModal");
type SettingsModalProps = Parameters<typeof SettingsModal>[0];

function settings(overrides: Partial<SettingsResponse> = {}): SettingsResponse {
  return {
    sessions_dir: "/home/user/.codex/sessions",
    default_dir: "/home/user/.codex/sessions",
    allowed_origins: [],
    api_auth_enabled: true,
    api_auth_source: "file",
    clients: [],
    ...overrides,
  };
}

function renderModal(props: Partial<SettingsModalProps> = {}) {
  render(
    <SettingsModal
      onClose={vi.fn()}
      onSaved={vi.fn()}
      fontScale={1}
      onFontScaleChange={vi.fn()}
      {...props}
    />,
  );
  return screen.findByPlaceholderText("https://example.com:8080");
}

describe("SettingsModal allowed origins", () => {
  beforeEach(() => {
    invokeMock.mockReset();
  });

  it("lists the origins already configured", async () => {
    invokeMock.mockResolvedValue(settings({ allowed_origins: ["https://a.example"] }));

    render(
      <SettingsModal
        onClose={vi.fn()}
        onSaved={vi.fn()}
        fontScale={1}
        onFontScaleChange={vi.fn()}
      />,
    );

    expect(await screen.findByText("https://a.example")).toBeInTheDocument();
  });

  it("persists a new origin and clears the input", async () => {
    invokeMock.mockImplementation((cmd: string, args: { origins?: string[] }) => {
      if (cmd === "get_settings") return Promise.resolve(settings());
      return Promise.resolve(settings({ allowed_origins: args.origins ?? [] }));
    });

    const input = await renderModal();
    fireEvent.change(input, { target: { value: "https://new.example" } });
    fireEvent.click(screen.getByRole("button", { name: "Add" }));

    await waitFor(() => {
      expect(invokeMock).toHaveBeenCalledWith("set_allowed_origins", {
        origins: ["https://new.example"],
      });
    });
    expect(await screen.findByText("https://new.example")).toBeInTheDocument();
    expect(input).toHaveValue("");
  });

  it("adds the origin on Enter without submitting the sessions directory", async () => {
    invokeMock.mockImplementation((cmd: string, args: { origins?: string[] }) => {
      if (cmd === "get_settings") return Promise.resolve(settings());
      return Promise.resolve(settings({ allowed_origins: args.origins ?? [] }));
    });

    const input = await renderModal();
    fireEvent.change(input, { target: { value: "https://enter.example" } });
    fireEvent.keyDown(input, { key: "Enter" });

    await waitFor(() => {
      expect(invokeMock).toHaveBeenCalledWith("set_allowed_origins", {
        origins: ["https://enter.example"],
      });
    });
    expect(invokeMock).not.toHaveBeenCalledWith("set_sessions_dir", expect.anything());
  });

  it("keeps the typed value and shows the error when the backend rejects it", async () => {
    invokeMock.mockImplementation((cmd: string) => {
      if (cmd === "get_settings") return Promise.resolve(settings());
      return Promise.reject(new Error("invalid origin: nope"));
    });

    const input = await renderModal();
    fireEvent.change(input, { target: { value: "nope" } });
    fireEvent.click(screen.getByRole("button", { name: "Add" }));

    expect(await screen.findByText(/invalid origin: nope/)).toBeInTheDocument();
    expect(input).toHaveValue("nope");
  });

  it("removes an origin by sending the remaining list", async () => {
    invokeMock.mockImplementation((cmd: string, args: { origins?: string[] }) => {
      if (cmd === "get_settings") {
        return Promise.resolve(
          settings({ allowed_origins: ["https://a.example", "https://b.example"] }),
        );
      }
      return Promise.resolve(settings({ allowed_origins: args.origins ?? [] }));
    });

    render(
      <SettingsModal
        onClose={vi.fn()}
        onSaved={vi.fn()}
        fontScale={1}
        onFontScaleChange={vi.fn()}
      />,
    );
    fireEvent.click(await screen.findByRole("button", { name: "Remove https://a.example" }));

    await waitFor(() => {
      expect(invokeMock).toHaveBeenCalledWith("set_allowed_origins", {
        origins: ["https://b.example"],
      });
    });
    expect(screen.queryByText("https://a.example")).not.toBeInTheDocument();
  });

  it("does not call the backend for a blank origin", async () => {
    invokeMock.mockResolvedValue(settings());

    const input = await renderModal();
    fireEvent.change(input, { target: { value: "   " } });

    expect(screen.getByRole("button", { name: "Add" })).toBeDisabled();
    expect(invokeMock).not.toHaveBeenCalledWith("set_allowed_origins", expect.anything());
  });
});

describe("SettingsModal font size", () => {
  beforeEach(() => {
    invokeMock.mockReset();
    invokeMock.mockResolvedValue(settings());
  });

  it("shows the current scale as a percentage", async () => {
    await renderModal({ fontScale: 1.25 });

    expect(screen.getByLabelText("Font Size")).toHaveValue("1.25");
    expect(screen.getByRole("option", { name: "125%" })).toBeInTheDocument();
  });

  it("offers every preset", async () => {
    await renderModal();

    expect(screen.getAllByRole("option").map((o) => o.textContent)).toEqual(
      FONT_SCALE_PRESETS.map(formatFontScale),
    );
  });

  it("reports a chosen scale as a number", async () => {
    const onFontScaleChange = vi.fn();
    await renderModal({ onFontScaleChange });

    fireEvent.change(screen.getByLabelText("Font Size"), { target: { value: "1.5" } });

    expect(onFontScaleChange).toHaveBeenCalledWith(1.5);
  });
});

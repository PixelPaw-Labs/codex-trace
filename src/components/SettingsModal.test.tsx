import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { SettingsModal } from "./SettingsModal";

const invokeMock = vi.fn();

vi.mock("../lib/invoke", () => ({
  invoke: (...args: unknown[]) => invokeMock(...args),
}));

describe("SettingsModal remote connections", () => {
  beforeEach(() => {
    invokeMock.mockReset();
    invokeMock.mockImplementation((command: string) =>
      Promise.resolve(
        command === "get_settings"
          ? { sessions_dir: null, default_dir: "/Users/test/.codex/sessions" }
          : {
              sessions_dir: "ssh://dev/~/.codex/sessions",
              default_dir: "/Users/test/.codex/sessions",
            },
      ),
    );
  });

  it("saves an SSH alias and remote sessions directory as an ssh URI", async () => {
    const onClose = vi.fn();
    const onSaved = vi.fn();
    render(<SettingsModal onClose={onClose} onSaved={onSaved} />);

    await waitFor(() =>
      expect(screen.getByDisplayValue("/Users/test/.codex/sessions")).toBeInTheDocument(),
    );
    fireEvent.click(screen.getByRole("button", { name: "SSH Remote" }));
    fireEvent.change(screen.getByLabelText("SSH Host"), { target: { value: "dev" } });
    fireEvent.change(screen.getByLabelText("Remote Sessions Directory"), {
      target: { value: "~/.codex/sessions" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Connect" }));

    await waitFor(() =>
      expect(invokeMock).toHaveBeenCalledWith("set_sessions_dir", {
        path: "ssh://dev/~/.codex/sessions",
      }),
    );
    expect(onSaved).toHaveBeenCalledWith("ssh://dev/~/.codex/sessions");
    expect(onClose).toHaveBeenCalledOnce();
  });
});

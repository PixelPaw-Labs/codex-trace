import { render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { KeybindBar } from "./KeybindBar";

const walking = {
  files_read: 120,
  total_files: 3375,
  bytes_read: 3_700_000,
  total_bytes: 10_000_000,
  done: false,
};

describe("KeybindBar", () => {
  it("shows the keybinds for the view it is on", () => {
    render(<KeybindBar index={{ ...walking, done: true }} view="picker" />);

    expect(screen.getByText("nav")).toBeInTheDocument();
    expect(screen.getByText("open")).toBeInTheDocument();
    expect(screen.getByText("search")).toBeInTheDocument();
  });

  it("hides the keybinds when asked, keeping the toggle", () => {
    render(
      <KeybindBar
        index={{ ...walking, done: true }}
        view="picker"
        showHints={false}
        onToggle={vi.fn()}
      />,
    );

    expect(screen.queryByText("nav")).not.toBeInTheDocument();
    expect(screen.getByTitle("Show keybinds")).toBeInTheDocument();
  });
});

describe("KeybindBar indexing progress", () => {
  it("shows how far the walk has got, in the strip along the bottom", () => {
    render(<KeybindBar index={walking} view="picker" />);

    const strip = document.querySelector(".keybind-bar") as HTMLElement;
    expect(strip.querySelector(".index-progress")).toBeInTheDocument();
    expect(screen.getByText("120 sessions · 3.7 MB / 10.0 MB")).toBeInTheDocument();
  });

  it("keeps the box once the walk has finished, so the strip does not change height", () => {
    const { container, rerender } = render(<KeybindBar index={walking} view="picker" />);
    const boxesWhileWalking = container.querySelectorAll(".index-progress").length;

    rerender(<KeybindBar index={{ ...walking, done: true }} view="picker" />);

    // A box that came and went would shift every row above it each time a walk started.
    expect(container.querySelectorAll(".index-progress")).toHaveLength(boxesWhileWalking);
    expect(screen.queryByRole("progressbar")).not.toBeInTheDocument();
  });

  it("still shows the keybinds while a walk is running", () => {
    render(<KeybindBar index={walking} view="picker" />);

    expect(screen.getByText("nav")).toBeInTheDocument();
  });
});

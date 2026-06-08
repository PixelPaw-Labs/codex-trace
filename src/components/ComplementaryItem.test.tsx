import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import type { AgentMessage } from "../../shared/types";
import { ComplementaryItem } from "./ComplementaryItem";

function makeMsg(overrides: Partial<AgentMessage> = {}): AgentMessage {
  return {
    text: "INLINE_PROSE",
    phase: "commentary",
    timestamp: "2026-04-26T10:00:00Z",
    is_reasoning: false,
    order: 0,
    ...overrides,
  };
}

describe("ComplementaryItem", () => {
  it("shows the assistant prose inline by default with no interaction", () => {
    render(<ComplementaryItem msg={makeMsg()} />);
    // Prose is visible immediately — never gated behind an expand/collapse chevron.
    expect(screen.getByText("INLINE_PROSE")).toBeInTheDocument();
    expect(screen.getByText("Complementary")).toBeInTheDocument();
    expect(document.querySelector(".complementary-item__chevron")).toBeNull();
  });

  it("renders markdown in the prose", () => {
    const { container } = render(<ComplementaryItem msg={makeMsg({ text: "**bold words**" })} />);
    expect(container.querySelector("strong")).toHaveTextContent("bold words");
  });

  it("omits the timestamp when none is present", () => {
    render(<ComplementaryItem msg={makeMsg({ timestamp: "" })} />);
    expect(document.querySelector(".complementary-item__time")).toBeNull();
  });
});

import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { MarkdownRenderer } from "./MarkdownRenderer";

/** Same DOM nodes, by identity — not merely the same markup. */
function sameNodes(a: Element[], b: Element[]): boolean {
  return a.length > 0 && a.length === b.length && a.every((node, i) => node === b[i]);
}

const MARKDOWN = ["Here is some code:", "", "```ts", "const a = 1;", "```"].join("\n");

describe("MarkdownRenderer", () => {
  it("highlights a fenced code block", async () => {
    render(<MarkdownRenderer content={MARKDOWN} />);
    expect(await screen.findByText(/const/)).toBeInTheDocument();
  });

  it("renders pure JSON as a formatted block", async () => {
    render(<MarkdownRenderer content='{"b":2,"a":1}' />);
    expect(await screen.findByText(/"a"/)).toBeInTheDocument();
  });

  it("keeps the highlighted block's DOM nodes across a re-render", async () => {
    const { container, rerender } = render(<MarkdownRenderer content={MARKDOWN} />);
    await screen.findByText(/const/);
    const before = [...container.querySelectorAll("code *")];
    expect(before.length).toBeGreaterThan(0);

    // Same content, new render pass. A code renderer defined during render gets
    // a fresh component identity each time, so React tears the block down and
    // rebuilds it — throwing away the highlighter's work and any scroll
    // position inside a wide block.
    rerender(<MarkdownRenderer content={MARKDOWN} />);
    const after = [...container.querySelectorAll("code *")];

    expect(sameNodes(before, after)).toBe(true);
  });

  it("keeps the block when an unrelated prop-driven re-render happens", async () => {
    function Wrapper({ label }: { label: string }) {
      return (
        <div>
          <span>{label}</span>
          <MarkdownRenderer content={MARKDOWN} />
        </div>
      );
    }
    const { container, rerender } = render(<Wrapper label="one" />);
    await screen.findByText(/const/);
    const before = [...container.querySelectorAll("code *")];

    rerender(<Wrapper label="two" />);

    expect(sameNodes(before, [...container.querySelectorAll("code *")])).toBe(true);
  });
});

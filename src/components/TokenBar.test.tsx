import { render } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import type { TokenInfo } from "../../shared/types";
import { TokenBar } from "./TokenBar";

// Figures from issue #154's real session: cumulative usage (228,286) is larger than the
// 190,000 window, while the latest request occupies only 26,984 of it.
function makeTokens(overrides: Partial<TokenInfo> = {}): TokenInfo {
  return {
    input_tokens: 226_616,
    cached_input_tokens: 176_640,
    output_tokens: 1_670,
    reasoning_output_tokens: 529,
    total_tokens: 228_286,
    context_window_tokens: 26_984,
    model_context_window: 190_000,
    rate_limits: null,
    ...overrides,
  };
}

function fillWidth(container: HTMLElement): string | null {
  const fill = container.querySelector<HTMLElement>(".token-bar__fill");
  return fill ? fill.style.width : null;
}

describe("TokenBar", () => {
  it("sizes the fill by context in use, not the cumulative session total", () => {
    const { container } = render(<TokenBar tokens={makeTokens()} />);
    // (26,984 - 12,000) / (190,000 - 12,000) = 8.4% used, rounded via the shared helper.
    expect(fillWidth(container)).toBe("8%");
  });

  it("does not peg to 100% when the cumulative total exceeds the window", () => {
    const { container } = render(<TokenBar tokens={makeTokens()} />);
    expect(fillWidth(container)).not.toBe("100%");
  });

  it("omits the fill when the latest-request context size is unknown", () => {
    const { container } = render(<TokenBar tokens={makeTokens({ context_window_tokens: null })} />);
    expect(fillWidth(container)).toBeNull();
    expect(container.querySelector(".token-bar")?.getAttribute("title")).toBeNull();
  });

  it("describes context in use in its title", () => {
    const { container } = render(<TokenBar tokens={makeTokens()} />);
    expect(container.querySelector(".token-bar")?.getAttribute("title")).toBe(
      "27.0k / 190.0k context tokens",
    );
  });

  it("shows cached input as its own figure", () => {
    const { getByText } = render(<TokenBar tokens={makeTokens()} />);
    expect(getByText(/cache/)).toBeInTheDocument();
  });
});

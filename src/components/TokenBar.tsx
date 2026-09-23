import type { TokenInfo } from "../../shared/types";
import { contextRemainingPercent, formatTokens } from "../../shared/format";

interface TokenBarProps {
  tokens: TokenInfo;
}

export function TokenBar({ tokens }: TokenBarProps) {
  const {
    input_tokens,
    cached_input_tokens,
    output_tokens,
    reasoning_output_tokens,
    context_window_tokens,
    model_context_window,
  } = tokens;

  // The fill shows how much of the context window the latest request occupies. That is
  // `context_window_tokens` (from `last_token_usage`), not `total_tokens`, which is the
  // cumulative session total and routinely exceeds the window. Same calculation as the
  // context meter in TurnDetail, so the two can never disagree.
  const remaining = contextRemainingPercent(context_window_tokens, model_context_window);
  const usedPct = remaining === null ? null : 100 - remaining;

  return (
    <div
      className="token-bar"
      title={
        context_window_tokens !== null
          ? `${formatTokens(context_window_tokens)} / ${formatTokens(model_context_window)} context tokens`
          : undefined
      }
    >
      <div className="token-bar__track">
        {usedPct !== null && <div className="token-bar__fill" style={{ width: `${usedPct}%` }} />}
      </div>
      <div className="token-bar__stats">
        <span style={{ color: "var(--token-input)" }}>in {formatTokens(input_tokens)}</span>
        {cached_input_tokens > 0 && (
          <span style={{ color: "var(--token-cached)" }}>
            cache {formatTokens(cached_input_tokens)}
          </span>
        )}
        <span style={{ color: "var(--token-output)" }}>out {formatTokens(output_tokens)}</span>
        {reasoning_output_tokens > 0 && (
          <span style={{ color: "var(--token-reasoning)" }}>
            think {formatTokens(reasoning_output_tokens)}
          </span>
        )}
      </div>
    </div>
  );
}

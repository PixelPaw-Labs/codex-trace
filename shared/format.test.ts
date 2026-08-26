import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  contextRemainingPercent,
  formatDuration,
  formatTokens,
  isLikelySandboxDenied,
  shortPath,
  timeAgo,
  truncate,
} from "./format";

describe("formatDuration", () => {
  it("returns '< 1ms' for zero", () => {
    expect(formatDuration(0)).toBe("< 1ms");
  });

  it("returns '< 1ms' for sub-millisecond values", () => {
    expect(formatDuration(0.5)).toBe("< 1ms");
    expect(formatDuration(0.99)).toBe("< 1ms");
  });

  it("rounds to integer ms for sub-second values", () => {
    expect(formatDuration(1)).toBe("1ms");
    expect(formatDuration(22.292)).toBe("22ms");
    expect(formatDuration(999)).toBe("999ms");
  });

  it("formats seconds with one decimal place", () => {
    expect(formatDuration(1000)).toBe("1.0s");
    expect(formatDuration(1500)).toBe("1.5s");
    expect(formatDuration(59900)).toBe("59.9s");
  });

  it("formats whole minutes without seconds", () => {
    expect(formatDuration(60000)).toBe("1m");
    expect(formatDuration(120000)).toBe("2m");
  });

  it("formats minutes with remaining seconds", () => {
    expect(formatDuration(90000)).toBe("1m 30s");
    expect(formatDuration(125000)).toBe("2m 5s");
  });
});

describe("formatTokens", () => {
  it("returns raw number for values under 1000", () => {
    expect(formatTokens(0)).toBe("0");
    expect(formatTokens(500)).toBe("500");
    expect(formatTokens(999)).toBe("999");
  });

  it("formats thousands with k suffix", () => {
    expect(formatTokens(1000)).toBe("1.0k");
    expect(formatTokens(1500)).toBe("1.5k");
  });

  it("formats millions with M suffix", () => {
    expect(formatTokens(1_000_000)).toBe("1.0M");
    expect(formatTokens(2_500_000)).toBe("2.5M");
  });
});

describe("contextRemainingPercent", () => {
  it("uses Codex's 12k baseline before calculating remaining context", () => {
    expect(contextRemainingPercent(26_000, 100_000)).toBe(84);
  });

  it("returns 100 when usage is below the fixed baseline", () => {
    expect(contextRemainingPercent(5_000, 100_000)).toBe(100);
  });

  it("clamps exhausted context to 0", () => {
    expect(contextRemainingPercent(150_000, 100_000)).toBe(0);
  });

  it("returns null when context usage is unknown", () => {
    expect(contextRemainingPercent(null, 100_000)).toBeNull();
  });

  it("returns null when the window cannot cover the fixed baseline", () => {
    expect(contextRemainingPercent(5_000, 12_000)).toBeNull();
  });
});

describe("timeAgo", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    vi.setSystemTime(new Date("2026-04-26T10:00:00Z"));
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  it("returns 'now' for timestamps less than 1 minute ago", () => {
    expect(timeAgo("2026-04-26T09:59:30Z")).toBe("now");
  });

  it("returns minutes ago", () => {
    expect(timeAgo("2026-04-26T09:57:00Z")).toBe("3m ago");
  });

  it("returns hours ago", () => {
    expect(timeAgo("2026-04-26T07:00:00Z")).toBe("3h ago");
  });

  it("returns days ago", () => {
    expect(timeAgo("2026-04-24T10:00:00Z")).toBe("2d ago");
  });
});

describe("truncate", () => {
  it("returns the string unchanged when under max length", () => {
    expect(truncate("hello", 10)).toBe("hello");
  });

  it("truncates with ellipsis when over max length", () => {
    expect(truncate("hello world", 8)).toBe("hello w…");
  });

  it("collapses newlines before truncating", () => {
    expect(truncate("hello\nworld", 20)).toBe("hello world");
  });

  it("trims leading/trailing whitespace", () => {
    expect(truncate("  hi  ", 10)).toBe("hi");
  });
});

describe("isLikelySandboxDenied", () => {
  it("returns false when there is no output", () => {
    expect(isLikelySandboxDenied(null)).toBe(false);
    expect(isLikelySandboxDenied("")).toBe(false);
  });

  it("returns false for unrelated output", () => {
    expect(isLikelySandboxDenied("file not found")).toBe(false);
  });

  // Codex v0.148.0 (issue #237): unreadable Linux glob patterns now fail the
  // sandbox construction itself instead of silently allowing the command.
  it("detects a Linux unreadable-glob denial (PR #38026)", () => {
    expect(
      isLikelySandboxDenied(
        "error building bubblewrap command: unreadable glob `/**/*.env` cannot be safely expanded",
      ),
    ).toBe(true);
  });

  it("detects a permission-denied message case-insensitively", () => {
    expect(isLikelySandboxDenied("Permission Denied")).toBe(true);
  });

  it("detects a read-only file system denial", () => {
    expect(isLikelySandboxDenied("touch: /etc/hosts: Read-only file system")).toBe(true);
  });

  it("detects a landlock/seccomp denial", () => {
    expect(isLikelySandboxDenied("process terminated by seccomp policy (landlock)")).toBe(true);
  });

  // Codex v0.148.0 PR #38416 (issue #237): a denied app file upload surfaces as
  // an MCP tool error with no exit code at all — only the output text signals
  // the sandbox denial.
  it("detects a permission-denied app file upload error", () => {
    expect(isLikelySandboxDenied("failed to upload `report.txt`: Permission denied")).toBe(true);
  });
});

describe("shortPath", () => {
  it("returns the last path segment", () => {
    expect(shortPath("/Users/foo/project")).toBe("project");
  });

  it("returns the only segment for a single-component path", () => {
    expect(shortPath("/project")).toBe("project");
  });

  it("returns empty string for empty input", () => {
    expect(shortPath("")).toBe("");
  });
});

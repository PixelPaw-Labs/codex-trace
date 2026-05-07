import { describe, expect, it } from "vitest";
import { API_BASE } from "./config";

describe("API_BASE", () => {
  it("uses HTTP transport, not a Unix socket path", () => {
    expect(API_BASE).toMatch(/^https?:\/\//);
  });

  it("defaults to localhost when no env override is set", () => {
    expect(API_BASE).toMatch(/127\.0\.0\.1|localhost/);
  });
});

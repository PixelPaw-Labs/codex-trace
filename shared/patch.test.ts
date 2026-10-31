import { describe, expect, it } from "vitest";
import { parseApplyPatch } from "./patch";

describe("parseApplyPatch", () => {
  it("returns null for non-patch text", () => {
    expect(parseApplyPatch("just some output text")).toBeNull();
    expect(parseApplyPatch("")).toBeNull();
  });

  it("parses an update with context, removed and added lines", () => {
    const patch = [
      "*** Begin Patch",
      "*** Update File: src/main.rs",
      "@@",
      " fn main() {",
      '-    println!("old");',
      '+    println!("new");',
      " }",
      "*** End Patch",
    ].join("\n");

    const files = parseApplyPatch(patch);
    expect(files).not.toBeNull();
    expect(files).toHaveLength(1);
    const file = files![0];
    expect(file.op).toBe("update");
    expect(file.path).toBe("src/main.rs");
    expect(file.movePath).toBeNull();
    expect(file.hunks).toHaveLength(1);

    const lines = file.hunks[0].lines;
    expect(lines.map((l) => l.kind)).toEqual(["context", "removed", "added", "context"]);
    expect(lines[0].segments.map((s) => s.text).join("")).toBe("fn main() {");
    // Removed/added share most tokens, so the changed word is highlighted.
    expect(lines[1].segments.some((s) => s.changed)).toBe(true);
    expect(lines[2].segments.some((s) => s.changed)).toBe(true);
    expect(lines[1].segments.map((s) => s.text).join("")).toBe('    println!("old");');
    expect(lines[2].segments.map((s) => s.text).join("")).toBe('    println!("new");');
  });

  it("parses an added file with all-added lines", () => {
    const patch = [
      "*** Begin Patch",
      "*** Add File: docs/README.md",
      "+# Title",
      "+",
      "+body",
      "*** End Patch",
    ].join("\n");

    const files = parseApplyPatch(patch)!;
    expect(files[0].op).toBe("add");
    expect(files[0].path).toBe("docs/README.md");
    const lines = files[0].hunks[0].lines;
    expect(lines.map((l) => l.kind)).toEqual(["added", "added", "added"]);
    expect(lines.map((l) => l.segments.map((s) => s.text).join(""))).toEqual([
      "# Title",
      "",
      "body",
    ]);
  });

  it("parses a deleted file header with no body", () => {
    const patch = ["*** Begin Patch", "*** Delete File: old/file.txt", "*** End Patch"].join("\n");
    const files = parseApplyPatch(patch)!;
    expect(files[0].op).toBe("delete");
    expect(files[0].path).toBe("old/file.txt");
    expect(files[0].hunks).toHaveLength(0);
  });

  it("captures a move/rename target", () => {
    const patch = [
      "*** Begin Patch",
      "*** Update File: a/old.ts",
      "*** Move to: a/new.ts",
      "@@",
      "-const x = 1;",
      "+const x = 2;",
      "*** End Patch",
    ].join("\n");
    const files = parseApplyPatch(patch)!;
    expect(files[0].movePath).toBe("a/new.ts");
  });

  it("splits multiple files and multiple hunks", () => {
    const patch = [
      "*** Begin Patch",
      "*** Update File: a.ts",
      "@@ first hunk",
      "-a",
      "+b",
      "@@ second hunk",
      " keep",
      "+added",
      "*** Update File: b.ts",
      "@@",
      "-gone",
      "*** End Patch",
    ].join("\n");
    const files = parseApplyPatch(patch)!;
    expect(files).toHaveLength(2);
    expect(files[0].path).toBe("a.ts");
    expect(files[0].hunks).toHaveLength(2);
    expect(files[0].hunks[0].header).toBe("first hunk");
    expect(files[0].hunks[1].header).toBe("second hunk");
    expect(files[1].path).toBe("b.ts");
    expect(files[1].hunks[0].lines.map((l) => l.kind)).toEqual(["removed"]);
  });

  it("does not emit a spurious blank line from a trailing newline", () => {
    const patch = ["*** Begin Patch", "*** Add File: f.txt", "+hi", "*** End Patch", ""].join("\n");
    const files = parseApplyPatch(patch)!;
    expect(files[0].hunks[0].lines.map((l) => l.kind)).toEqual(["added"]);
  });

  it("strips trailing \\r from a CRLF-preserved patch body (Codex v0.162.0+)", () => {
    const patch = [
      "*** Begin Patch",
      "*** Update File: src/main.rs",
      "@@",
      " fn main() {",
      '-    println!("old");',
      '+    println!("new");',
      " }",
      "*** End Patch",
    ].join("\r\n");

    const files = parseApplyPatch(patch)!;
    expect(files).toHaveLength(1);
    const file = files[0];
    expect(file.path).toBe("src/main.rs");

    const lines = file.hunks[0].lines;
    expect(lines.map((l) => l.kind)).toEqual(["context", "removed", "added", "context"]);
    for (const line of lines) {
      for (const seg of line.segments) {
        expect(seg.text).not.toContain("\r");
      }
    }
    expect(lines[0].segments.map((s) => s.text).join("")).toBe("fn main() {");
    expect(lines[1].segments.map((s) => s.text).join("")).toBe('    println!("old");');
    expect(lines[2].segments.map((s) => s.text).join("")).toBe('    println!("new");');
    expect(lines[3].segments.map((s) => s.text).join("")).toBe("}");
  });
});

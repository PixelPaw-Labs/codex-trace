import { readFileSync } from "node:fs";
import { join } from "node:path";
import { describe, expect, it } from "vitest";

/**
 * `tauri build` refuses to run when a Tauri npm package and its Rust crate are on
 * different major.minor releases. Dependabot bumps the two lockfiles separately and CI
 * never runs a full app build, so nothing else notices until a release build fails.
 */

function read(name: string): string {
  return readFileSync(join(process.cwd(), name), "utf8");
}

const npmLock = JSON.parse(read("package-lock.json")) as {
  packages: Record<string, { version?: string }>;
};
const cargoLock = read("src-tauri/Cargo.lock");

function majorMinor(version: string): string {
  return version.split(".").slice(0, 2).join(".");
}

function crateVersion(crate: string): string | undefined {
  return cargoLock.match(new RegExp(`name = "${crate}"\\nversion = "([^"]+)"`))?.[1];
}

/** npm package name → the Rust crate `tauri build` pairs it with. */
function crateFor(npmName: string): string | undefined {
  if (npmName === "@tauri-apps/api") return "tauri";
  const plugin = npmName.match(/^@tauri-apps\/plugin-(.+)$/)?.[1];
  return plugin ? `tauri-plugin-${plugin}` : undefined;
}

const pairs = Object.entries(npmLock.packages).flatMap(([path, pkg]) => {
  const npmName = path.replace(/^node_modules\//, "");
  const crate = crateFor(npmName);
  const rustVersion = crate && crateVersion(crate);
  return crate && rustVersion && pkg.version
    ? [{ npmName, npmVersion: pkg.version, crate, rustVersion }]
    : [];
});

describe("Tauri npm packages and Rust crates", () => {
  it("finds the core package and at least one plugin to compare", () => {
    const names = pairs.map((pair) => pair.npmName);

    expect(names).toContain("@tauri-apps/api");
    expect(names.some((name) => name.startsWith("@tauri-apps/plugin-"))).toBe(true);
  });

  it.each(pairs)(
    "$npmName $npmVersion is on the same major.minor as $crate $rustVersion",
    ({ npmVersion, rustVersion }) => {
      expect(majorMinor(npmVersion)).toBe(majorMinor(rustVersion));
    },
  );
});

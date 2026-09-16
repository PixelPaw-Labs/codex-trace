import { readFileSync } from "node:fs";
import { join } from "node:path";
import { describe, expect, it } from "vitest";

/**
 * The Dockerfile and docker-compose.yml describe the same container twice, and
 * nothing makes them agree. These checks read both and diff the parts that
 * silently break the image when they drift.
 */

function read(name: string): string {
  return readFileSync(join(process.cwd(), name), "utf8");
}

const dockerfile = read("Dockerfile");
const compose = read("docker-compose.yml");
const viteConfig = read("vite.config.ts");

/** Every `VOLUME ["..."]` path declared in the Dockerfile. */
function declaredVolumes(): string[] {
  return [...dockerfile.matchAll(/^VOLUME\s+\[([^\]]+)\]/gm)].flatMap((m) =>
    [...m[1].matchAll(/"([^"]+)"/g)].map((q) => q[1]),
  );
}

/** Container-side paths the compose service mounts something at. */
function composeMountTargets(): string[] {
  return [...compose.matchAll(/^\s+-\s+"?[^"\n]*?:(\/[^":\n]+)(?::ro)?"?$/gm)].map((m) => m[1]);
}

/** Relative paths `vite.config.ts` imports, resolved against the repo root. */
function viteConfigLocalImports(): string[] {
  return [...viteConfig.matchAll(/from\s+"(\.\/[^"]+)"/g)].map((m) => m[1].replace(/^\.\//, ""));
}

/** Top-level paths the frontend build stage copies into the image. */
function frontendCopiedPaths(): string[] {
  const stage = dockerfile.split(/^FROM /m).find((s) => s.includes("AS frontend-builder"));
  expect(stage, "frontend-builder stage present").toBeDefined();
  return [...stage!.matchAll(/^COPY\s+(.+?)\s+\S+$/gm)].flatMap((m) => m[1].split(/\s+/));
}

/** Paths passed to `mkdir -p` anywhere in the Dockerfile. */
function madeDirs(): string[] {
  return [...dockerfile.matchAll(/mkdir -p ((?:\/\S+\s*)+)/g)].flatMap((m) =>
    m[1].trim().split(/\s+/),
  );
}

/** Paths handed to the app user by `chown`. */
function chownedDirs(): string[] {
  return [...dockerfile.matchAll(/chown (?:-R )?app:app ((?:\/\S+\s*)+)/g)].flatMap((m) =>
    m[1].trim().split(/\s+/),
  );
}

describe("healthcheck", () => {
  it("runs the /dev/tcp probe under bash in the Dockerfile", () => {
    const healthcheck = dockerfile.match(/^HEALTHCHECK[\s\S]*?\n(?!\s)/m)?.[0] ?? "";
    expect(healthcheck).toContain("/dev/tcp/");
    // /bin/sh is dash on debian-slim and cannot open /dev/tcp, so every check
    // would fail and the container would report unhealthy forever.
    expect(healthcheck).toContain("/bin/bash");
    expect(healthcheck).not.toMatch(/\/bin\/sh\b/);
  });

  it("runs the /dev/tcp probe under bash in compose", () => {
    const test = compose.match(/test:\s*(\[.*\])/)?.[1] ?? "";
    expect(test).toContain("/dev/tcp/");
    expect(test).toContain("/bin/bash");
    // CMD-SHELL always goes through /bin/sh.
    expect(test).not.toContain("CMD-SHELL");
  });

  it("probes the port the image actually listens on", () => {
    const exposed = dockerfile.match(/^EXPOSE\s+(\d+)/m)?.[1];
    expect(exposed).toBeDefined();
    expect(dockerfile).toMatch(new RegExp(`CODEXTRACE_HTTP_PORT=${exposed}\\b`));
    expect(dockerfile).toContain(`/dev/tcp/127.0.0.1/\${CODEXTRACE_HTTP_PORT:-${exposed}}`);
    expect(compose).toContain(`/dev/tcp/127.0.0.1/${exposed}`);
  });
});

describe("volumes", () => {
  it("persists the config directory the app writes to", () => {
    const configHome = dockerfile.match(/XDG_CONFIG_HOME=(\S+)/)?.[1];
    expect(configHome).toBeDefined();
    // settings.json, the signing secret and the issued client credentials all
    // live here; without a volume they reset on every container recreate.
    expect(declaredVolumes()).toContain(configHome);
    expect(composeMountTargets()).toContain(configHome);
  });

  it("creates every volume mountpoint as the app user", () => {
    const user = dockerfile.match(/^USER\s+(\S+)/m)?.[1];
    expect(user).toBe("app");
    for (const path of declaredVolumes()) {
      // Docker initialises a volume at a mountpoint that does not exist in the
      // image as root, and the non-root user then gets EACCES writing to it.
      expect(madeDirs(), `${path} is created in the image before USER ${user}`).toContain(path);
      expect(
        chownedDirs().some((owned) => path === owned || path.startsWith(`${owned}/`)),
        `${path} is chowned to ${user}`,
      ).toBe(true);
    }
  });

  it("gives compose a named volume rather than an anonymous one", () => {
    const named = [...compose.matchAll(/^\s+-\s+([a-z][\w-]*):(\/\S+)$/gm)].map((m) => m[1]);
    expect(named.length).toBeGreaterThan(0);
    for (const name of named) {
      expect(compose).toMatch(new RegExp(`^volumes:[\\s\\S]*^\\s+${name}:`, "m"));
    }
  });
});

describe("build context", () => {
  it("copies everything vite.config.ts imports into the frontend stage", () => {
    const copied = frontendCopiedPaths();
    for (const imported of viteConfigLocalImports()) {
      const top = imported.split("/")[0];
      // A missing COPY here does not fail until `npm run build` runs inside the
      // image, which only happens on a Docker build.
      expect(copied, `vite.config.ts imports ${imported}`).toContain(top);
    }
  });
});

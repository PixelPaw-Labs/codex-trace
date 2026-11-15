import { spawnSync } from "node:child_process";
import { join } from "node:path";
import { describe, expect, it } from "vitest";

const redeployScript = join(process.cwd(), "script/redeploy.sh");

function listContainersPublishingPort(port: string, dockerBody: string) {
  const source = `
    source "$REDEPLOY_SCRIPT"
    docker() {
      ${dockerBody}
    }
    container_ids_publishing_port ${port}
  `;
  return spawnSync("bash", ["-c", source], {
    env: { ...process.env, REDEPLOY_SCRIPT: redeployScript },
    encoding: "utf8",
  });
}

describe("Docker redeploy port cleanup", () => {
  it("finds containers publishing the port without the engine-specific publish filter", () => {
    const result = listContainersPublishingPort(
      "1422",
      `
        printf '%s\\n' "$*" >&2
        printf '%s\\n' \\
          'aaa111 0.0.0.0:1422->1422/tcp, [::]:1422->1422/tcp' \\
          'bbb222 0.0.0.0:11422->80/tcp' \\
          'ccc333 0.0.0.0:8080->1422/tcp' \\
          'ddd444 '
      `,
    );

    expect(result.status).toBe(0);
    expect(result.stdout).toBe("aaa111\n");
    expect(result.stderr).not.toContain("--filter");
  });

  it("returns nothing when no container publishes the port", () => {
    const result = listContainersPublishingPort("1422", "printf '%s\\n' 'ddd444 '");

    expect(result.status).toBe(0);
    expect(result.stdout).toBe("");
  });

  it("fails loudly when the engine rejects the listing", () => {
    const result = listContainersPublishingPort(
      "1422",
      `
        echo 'Error response from daemon: boom' >&2
        return 1
      `,
    );

    expect(result.status).not.toBe(0);
    expect(result.stderr).toContain("Error response from daemon: boom");
  });

  it("does not run the redeploy when the script is only sourced", () => {
    const result = spawnSync("bash", ["-c", 'source "$REDEPLOY_SCRIPT"; echo sourced'], {
      env: { ...process.env, REDEPLOY_SCRIPT: redeployScript },
      encoding: "utf8",
    });

    expect(result.status).toBe(0);
    expect(result.stdout).toBe("sourced\n");
  });
});

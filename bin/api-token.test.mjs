import { mkdtempSync, mkdirSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { describe, expect, it } from "vitest";
import {
  appConfigRoot,
  configDir,
  readWebUiCredential,
  webUiCredentialPath,
  WEB_UI_CLIENT,
} from "./api-token.mjs";

function credentialRoot(credential) {
  const root = mkdtempSync(join(tmpdir(), "codex-trace-token-"));
  if (credential !== undefined) {
    mkdirSync(join(root, "clients"), { recursive: true });
    writeFileSync(join(root, "clients", `${WEB_UI_CLIENT}.jwt`), credential);
  }
  return root;
}

describe("configDir", () => {
  it("mirrors the Rust dirs crate per platform", () => {
    const home = "/home/u";
    expect(configDir({ platform: "darwin", env: {}, home })).toBe(
      "/home/u/Library/Application Support",
    );
    expect(configDir({ platform: "linux", env: {}, home })).toBe("/home/u/.config");
    expect(configDir({ platform: "win32", env: {}, home })).toBe("/home/u/AppData/Roaming");
  });

  it("honours XDG_CONFIG_HOME and APPDATA", () => {
    expect(configDir({ platform: "linux", env: { XDG_CONFIG_HOME: "/xdg" }, home: "/h" })).toBe(
      "/xdg",
    );
    expect(configDir({ platform: "win32", env: { APPDATA: "/appdata" }, home: "/h" })).toBe(
      "/appdata",
    );
  });
});

describe("appConfigRoot", () => {
  it("uses CODEXTRACE_CONFIG_DIR when set", () => {
    expect(appConfigRoot({ env: { CODEXTRACE_CONFIG_DIR: "/custom" } })).toBe("/custom");
  });

  it("ignores a blank override", () => {
    const root = appConfigRoot({
      platform: "linux",
      env: { CODEXTRACE_CONFIG_DIR: "  ", XDG_CONFIG_HOME: "/xdg" },
      home: "/h",
    });
    expect(root).toBe("/xdg/codex-trace");
  });
});

describe("webUiCredentialPath", () => {
  it("points at clients/web-ui.jwt under the config root", () => {
    expect(webUiCredentialPath({ env: { CODEXTRACE_CONFIG_DIR: "/c" } })).toBe(
      "/c/clients/web-ui.jwt",
    );
  });
});

describe("readWebUiCredential", () => {
  it("reads and trims the credential the backend wrote", () => {
    const root = credentialRoot("aaa.bbb.ccc\n");
    expect(readWebUiCredential({ env: { CODEXTRACE_CONFIG_DIR: root } })).toBe("aaa.bbb.ccc");
  });

  it("returns null when the backend has not written the file yet", () => {
    const root = credentialRoot(undefined);
    expect(readWebUiCredential({ env: { CODEXTRACE_CONFIG_DIR: root } })).toBeNull();
  });

  it("returns null for an empty file rather than an empty credential", () => {
    const root = credentialRoot("   \n");
    expect(readWebUiCredential({ env: { CODEXTRACE_CONFIG_DIR: root } })).toBeNull();
  });

  it("returns null when verification is switched off", () => {
    const root = credentialRoot("aaa.bbb.ccc");
    expect(
      readWebUiCredential({
        env: { CODEXTRACE_CONFIG_DIR: root, CODEXTRACE_API_AUTH: "OFF" },
      }),
    ).toBeNull();
  });
});

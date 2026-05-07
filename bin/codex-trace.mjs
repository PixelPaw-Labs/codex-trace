#!/usr/bin/env node
import { execSync, spawn } from "node:child_process";
import { createInterface } from "node:readline";
import { createConnection } from "node:net";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { platform } from "node:os";

const currentDir = dirname(fileURLToPath(import.meta.url));
const root = resolve(currentDir, "..");

const args = process.argv.slice(2);
const mode = args.find((a) => ["--app", "--web"].includes(a)) ?? "--app";
const noOpen = args.includes("--no-open");

function run(cmd, cmdArgs, opts = {}) {
  const child = spawn(cmd, cmdArgs, {
    stdio: "inherit",
    cwd: root,
    ...opts,
  });
  child.on("exit", (code) => process.exit(code ?? 0));
  return child;
}

function ask(question) {
  const rl = createInterface({ input: process.stdin, output: process.stdout });
  return new Promise((res) => {
    rl.question(question, (answer) => {
      rl.close();
      res(answer.trim().toLowerCase());
    });
  });
}

function isPortInUse(port) {
  return new Promise((res) => {
    const sock = createConnection({ port, host: "127.0.0.1" });
    sock.once("connect", () => {
      sock.destroy();
      res(true);
    });
    sock.once("error", () => res(false));
  });
}

async function waitForPort(port, timeoutMs = 30_000) {
  const start = Date.now();
  while (Date.now() - start < timeoutMs) {
    // eslint-disable-next-line no-await-in-loop
    if (await isPortInUse(port)) return;
    // eslint-disable-next-line no-await-in-loop
    await new Promise((r) => setTimeout(r, 300));
  }
}

function openBrowser(url) {
  try {
    const os = platform();
    if (os === "darwin") execSync(`open "${url}"`);
    else if (os === "win32") execSync(`cmd.exe /c start "" "${url}"`);
    else execSync(`xdg-open "${url}" 2>/dev/null`);
  } catch {}
}

switch (mode) {
  case "--app":
    run("npx", ["tauri", "dev"]);
    break;

  case "--web": {
    const frontendRunning = await isPortInUse(1420);
    const backendRunning = await isPortInUse(11424);

    if (noOpen) {
      if (frontendRunning) {
        console.error("Port 1420 already in use, exiting.");
        process.exit(0);
      }
      if (backendRunning) {
        run("npx", ["vite"]);
      } else {
        run("npx", ["tauri", "dev", "--", "--", "--web", "--no-open"]);
      }
    } else {
      if (frontendRunning) {
        console.log("codex-trace web server is already running on http://localhost:1420");
        openBrowser("http://localhost:1420");
        process.exit(0);
      }

      if (backendRunning) {
        const answer = await ask(
          "codex-trace backend is already running. Start web frontend only? [Y/n] ",
        );
        if (answer === "n") process.exit(0);
        run("npx", ["vite"]);
        await waitForPort(1420);
        openBrowser("http://localhost:1420");
      } else {
        run("npx", ["tauri", "dev", "--", "--", "--web"]);
        await waitForPort(11424);
        await waitForPort(1420);
        openBrowser("http://localhost:1420");
      }
    }
    break;
  }

  default:
    console.error(`Unknown mode: ${mode}`);
    process.exit(1);
}

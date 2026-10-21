import { spawn, type ChildProcess } from "node:child_process";
import { existsSync, readdirSync, rmSync, statSync } from "node:fs";
import { join } from "node:path";
import { setTimeout as sleep } from "node:timers/promises";
import { ADMIN_PASSWORD, API_TOKEN, REPO_DIR, freePort } from "./helpers";

type Serve = { url: string; stop: () => Promise<void> };

/** The server this run drives, started once by global-setup. */
let serve: Serve | null = null;

/**
 * Build the UI and the debug binary once, then start one `drawer serve` on a
 * throwaway store. Specs read its URL from DRAWER_BASE_URL.
 */
async function globalSetup(): Promise<void> {
  const started = Date.now();
  // Ask the OS for a port nobody is using, so two runs (or a run and a dev server) never
  // collide, and hand it to both the child and the Playwright baseURL below.
  const port = await freePort();
  process.env.DRAWER_PORT = String(port);
  if (needsBuild()) {
    console.log("[e2e] building the UI (ui/dist is missing or stale)");
    await run(npmCommand(), join(REPO_DIR, "ui"));
  } else {
    console.log("[e2e] ui/dist is up to date");
  }
  const binary = join(REPO_DIR, "target", "debug", "drawer");
  if (!existsSync(binary) || statSync(binary).mtimeMs < sourceMtime()) {
    console.log("[e2e] cargo build (debug)");
    await run(["cargo", "build"], REPO_DIR);
  } else {
    console.log("[e2e] target/debug/drawer is up to date");
  }

  const dataDir = join(REPO_DIR, "target", "e2e-data");
  rmSync(dataDir, { recursive: true, force: true });
  const child = spawn(binary, ["serve"], {
    cwd: REPO_DIR,
    env: {
      ...process.env,
      DRAWER_ADDR: `127.0.0.1:${port}`,
      DRAWER_DATA_DIR: dataDir,
      // Password mode is the interesting one: the browser holds the session cookie,
      // and the machine token stays available for API-only checks.
      DRAWER_ADMIN_PASSWORD: process.env.DRAWER_ADMIN_PASSWORD ?? ADMIN_PASSWORD,
      DRAWER_TOKEN: process.env.DRAWER_TOKEN ?? API_TOKEN,
      DRAWER_UI_DIR: join(REPO_DIR, "ui", "dist"),
      DRAWER_AUTHOR: "e2e",
    },
    stdio: ["ignore", "pipe", "pipe"],
  });
  serve = await waitForServer(child, port);
  // The config file is loaded before global-setup, so hand the URL to the workers too.
  process.env.DRAWER_BASE_URL = serve.url;
  process.env.DRAWER_BIN = binary;
  console.log(`[e2e] drawer serve ready at ${serve.url} (${Math.round((Date.now() - started) / 1000)}s)`);
}

/** Stop the server and drop its store, unless the caller wants to look at them. */
async function globalTeardown(): Promise<void> {
  await serve?.stop();
  serve = null;
  if (process.env.E2E_KEEP_DATA) {
    console.log("[e2e] E2E_KEEP_DATA is set, leaving target/e2e-data in place");
    return;
  }
  rmSync(join(REPO_DIR, "target", "e2e-data"), { recursive: true, force: true });
}

/** The newest mtime below `dir`, so a stale build is detected. */
function newestMtime(dir: string): number {
  let newest = 0;
  for (const entry of readdirSync(dir, { withFileTypes: true })) {
    const path = join(dir, entry.name);
    const stamp = statSync(path).mtimeMs;
    if (entry.isDirectory()) newest = Math.max(newest, newestMtime(path));
    else newest = Math.max(newest, stamp);
  }
  return newest;
}

function needsBuild(): boolean {
  const dist = join(REPO_DIR, "ui", "dist", "index.html");
  if (!existsSync(dist)) return true;
  return newestMtime(join(REPO_DIR, "ui", "src")) > statSync(dist).mtimeMs;
}

/** Newest mtime of anything that can change the binary. */
function sourceMtime(): number {
  let newest = 0;
  const walk = (dir: string): void => {
    for (const entry of readdirSync(dir, { withFileTypes: true })) {
      const path = join(dir, entry.name);
      if (entry.isDirectory()) {
        if (entry.name === "target") continue;
        walk(path);

      } else newest = Math.max(newest, statSync(path).mtimeMs);
    }
  };
  walk(join(REPO_DIR, "src"));
  walk(join(REPO_DIR, "tests"));
  const manifest = join(REPO_DIR, "Cargo.toml");
  if (existsSync(manifest)) newest = Math.max(newest, statSync(manifest).mtimeMs);
  return newest;
}

/** Run a command and fail loudly with its output when it does not exit 0. */
async function run(cmd: string[], cwd: string): Promise<void> {
  const [exe, ...args] = cmd;
  const child = spawn(exe, args, { cwd, env: process.env });
  let out = "";
  child.stdout?.on("data", (chunk) => (out += chunk));
  child.stderr?.on("data", (chunk) => (out += chunk));
  const { promise, resolve, reject } = Promise.withResolvers<number>();
  child.on("error", (e) => { console.log("[e2e] run error event:", (e as NodeJS.ErrnoException).code, exe); reject(e); });
  child.on("close", resolve);
  const code = await promise;
  if (code !== 0) throw new Error(`\`${cmd.join(" ")}\` failed (${code}):\n${out}`);
}

/** Wait for /health, and hand back the URL plus a stopper. */
async function waitForServer(child: ChildProcess, port: number): Promise<Serve> {
  const url = `http://127.0.0.1:${port}`;
  let log = "";
  child.stdout?.on("data", (chunk) => (log += chunk));
  child.stderr?.on("data", (chunk) => (log += chunk));
  const deadline = Date.now() + 120_000;
  while (Date.now() < deadline) {
    if (child.exitCode !== null && child.exitCode !== undefined) {
      throw new Error(`drawer serve exited with ${child.exitCode}:\n${log}`);
    }
    try {
      const response = await fetch(`${url}/health`);
      if (response.ok) return { url, stop: stopServe(child, log) };
    } catch {
      // not listening yet
    }
    await sleep(150);
  }
  child.kill("SIGKILL");
  throw new Error(`drawer serve never answered /health at ${url}:\n${log}`);
}

function stopServe(child: ChildProcess, log: string): () => Promise<void> {
  return async () => {
    child.kill("SIGTERM");
    const { promise, resolve } = Promise.withResolvers<void>();
    child.once("exit", () => resolve());
    setTimeout(resolve, 5_000).unref();
    await promise;
    if (log) process.stdout.write(`drawer serve output:\n${log}\n`);
  };
}

export { globalTeardown };
export default globalSetup;

/**
 * `npm` is not always on PATH for a test runner (pi-node here, for example), and a
 * non-executable sibling file is worse than useless. `sh -c` uses PATH, which is
 * what a person typing `npm run build` actually gets.
 */
function npmCommand(): string[] {
  return process.platform === "win32" ? ["npm.cmd", "run", "build"] : ["sh", "-c", "npm run build"];
}

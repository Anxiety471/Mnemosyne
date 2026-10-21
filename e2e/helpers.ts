import { existsSync, mkdtempSync, readdirSync, rmSync, statSync } from "node:fs";
import { createServer } from "node:net";
import { spawn, type ChildProcess } from "node:child_process";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { setTimeout as sleep } from "node:timers/promises";

/** This file lives in <repo>/e2e, so the repository root is its own directory. */
export const E2E_DIR = resolve(__dirname);
/** The repository root, which holds Cargo.toml, ui/ and target/. */
export const REPO_DIR = resolve(E2E_DIR, "..");

export const ADMIN_PASSWORD = "e2e-admin-password";
/** The machine token the browser is seeded with, for token-mode flows. */
export const API_TOKEN = "e2e-machine-token";

/** A directory that only exists for this run, so specs never see each other's files. */
export function freshDir(prefix: string): string {
  return mkdtempSync(join(tmpdir(), `drawer-e2e-${prefix}-`));
}

export function cleanupDir(path: string): void {
  if (!path.startsWith(tmpdir())) throw new Error(`refusing to remove ${path}`);
  rmSync(path, { recursive: true, force: true });
}

/** Ask the OS for a port nobody is listening on. */
export async function freePort(): Promise<number> {
  return await new Promise((ok, fail) => {
    const server = createServer();
    server.unref();
    server.on("error", fail);
    server.listen(0, "127.0.0.1", () => {
      const address = server.address();
      if (address === null || typeof address === "string") {
        server.close(() => fail(new Error("no port allocated")));
        return;
      }
      const port = address.port;
      server.close(() => ok(port));
    });
  });
}

/** Run a command and fail loudly with its output when it does not exit 0. */
export async function run(cmd: string[], cwd: string): Promise<string> {
  const [exe, ...args] = cmd;
  const child = spawn(exe, args, { cwd, env: process.env });
  let out = "";
  let err = "";
  child.stdout?.on("data", (chunk) => (out += chunk));
  child.stderr?.on("data", (chunk) => (err += chunk));
  const { promise, resolve, reject } = Promise.withResolvers<number>();
  child.on("error", reject);
  child.on("close", resolve);
  const code = await promise;
  if (code !== 0) throw new Error(`\`${cmd.join(" ")}\` failed (${code}):\n${out}\n${err}`);
  return out;
}

/** The binary built by global-setup, so specs never start their own. */
export function drawerBinary(): string {
  const binary = process.env.DRAWER_BIN;
  if (!binary) throw new Error("DRAWER_BIN is not set: run through `npm test` in e2e/");
  return binary;
}

export type Serve = {
  url: string;
  dataDir: string;
  child: ChildProcess;
  stop: () => Promise<void>;
};

/** Start `drawer serve` with its own store, address and auth settings. */
export async function startServe(options: {
  dataDir: string;
  /** DRAWER_ADMIN_PASSWORD enables the password sign-in flow. */
  password?: string;
  /** DRAWER_TOKEN keeps a machine credential for the token-mode flows. */
  token?: string;
  uiDir?: string;
  author?: string;
}): Promise<Serve> {
  const port = await freePort();
  const env = {
    ...process.env,
    DRAWER_ADDR: `127.0.0.1:${port}`,
    DRAWER_DATA_DIR: options.dataDir,
    ...(options.password ? { DRAWER_ADMIN_PASSWORD: options.password } : { DRAWER_ADMIN_PASSWORD: "" }),
    ...(options.token ? { DRAWER_TOKEN: options.token } : {}),
    ...(options.uiDir ? { DRAWER_UI_DIR: options.uiDir } : {}),
    ...(options.author ? { DRAWER_AUTHOR: options.author } : {}),
  };
  const child = spawn(drawerBinary(), ["serve"], { cwd: REPO_DIR, env, stdio: ["ignore", "pipe", "pipe"] });
  let log = "";
  child.stdout?.on("data", (chunk) => (log += chunk));
  child.stderr?.on("data", (chunk) => (log += chunk));
  const url = `http://127.0.0.1:${port}`;
  await waitForHealth(url, child);
  return {
    url,
    dataDir: options.dataDir,
    child,
    stop: async () => {
      child.kill("SIGTERM");
      const { promise, resolve } = Promise.withResolvers<void>();
      child.once("exit", () => resolve());
      // `serve` has no signal handler: if it is wedged, do not hang the suite.
      setTimeout(resolve, 5_000).unref();
      await promise;
      if (log) process.stdout.write(`drawer serve (${url}) output:\n${log}\n`);
    },
  };
}

/** Poll /health until the server answers or the process gives up. */
export async function waitForHealth(url: string, child?: ChildProcess): Promise<void> {
  const deadline = Date.now() + 60_000;
  let last = "";
  while (Date.now() < deadline) {
    if (child?.exitCode !== null && child?.exitCode !== undefined) {
      throw new Error(`drawer serve exited with ${child.exitCode}`);
    }
    try {
      const response = await fetch(`${url}/health`);
      if (response.ok) return;
      last = `HTTP ${response.status}`;
    } catch (error) {
      last = error instanceof Error ? error.message : String(error);
    }
    await sleep(150);
  }
  throw new Error(`drawer serve never became healthy at ${url}: ${last}`);
}

/** The newest mtime below `dir`, so a spec can wait for an upload that writes in the background. */
export function newestMtime(dir: string): number {
  let newest = 0;
  for (const entry of readdirSync(dir, { withFileTypes: true })) {
    const path = join(dir, entry.name);
    const stamp = statSync(path).mtimeMs;
    if (entry.isDirectory()) newest = Math.max(newest, newestMtime(path));
    else newest = Math.max(newest, stamp);
  }
  return newest;
}

/** `ui/dist/index.html` is rebuilt when a UI source file is newer than it. */
export function uiNeedsBuild(): boolean {
  const dist = join(REPO_DIR, "ui", "dist", "index.html");
  if (!existsSync(dist)) return true;
  const built = statSync(dist).mtimeMs;
  return newestMtime(join(REPO_DIR, "ui", "src")) > built;
}

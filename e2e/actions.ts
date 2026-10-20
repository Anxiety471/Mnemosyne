import { expect, type APIRequestContext, type Locator, type Page } from "@playwright/test";
import { API_TOKEN, ADMIN_PASSWORD } from "./helpers";

/**
 * Sign in through the UI's own prompt, exactly the way a person does: the app
 * asks for a password once and keeps the session in an HttpOnly cookie.
 */
export async function signIn(page: Page, password = ADMIN_PASSWORD): Promise<void> {
  await page.goto("/");
  // The prompt only appears after the first 401, and a previous flow may have left the
  // session cookie behind, so wait for either the prompt or a signed-in app.
  await page.locator(".toast, button:has-text('Sign out')").first().waitFor({ timeout: 20_000 });
  const prompt = page.getByRole("dialog", { name: "Sign in to Drawer" });
  if (await prompt.isVisible()) {
    await prompt.getByLabel("Username").fill("admin");
    await prompt.getByLabel("Password").fill(password);
    await prompt.getByRole("button", { name: "Continue" }).click();
    await prompt.waitFor({ state: "detached" });
  }
  await expect(page.getByRole("button", { name: "Sign out" })).toBeVisible();
}

/** Sign out through the UI, so the next flow really starts unauthenticated. */
export async function signOut(page: Page): Promise<void> {
  await page.getByRole("button", { name: "Sign out" }).click();
  await expect(page.getByRole("button", { name: "Sign out" })).toHaveCount(0);
}

/** Machine-token request options, for checks that must bypass the browser session. */
export function tokenAuth(token = API_TOKEN): { headers: Record<string, string> } {
  return { headers: { authorization: `Bearer ${token}` } };
}

/**
 * Authenticated REST for a precondition the UI does not expose. The suite's server runs in
 * password mode *and* still accepts the machine bearer token, so every request needs the
 * header or the API answers 401.
 */
export function auth(extra: Record<string, string> = {}): Record<string, string> {
  return { authorization: `Bearer ${API_TOKEN}`, ...extra };
}

/** REST request bound to the machine token, so a precondition cannot fail on auth. */
export async function apiPut(
  request: APIRequestContext,
  path: string,
  body?: unknown,
  init: { headers?: Record<string, string> } = {},
): Promise<unknown> {
  const response = await request.put(`/drawer/v1${path}`, {
    data: body,
    headers: { "x-drawer-author": "e2e-api", ...auth(init.headers) },
  });
  if (!response.ok()) throw new Error(`PUT ${path}: ${response.status()} ${await response.text()}`);
  return await response.json();
}

/** Create a folder through the API, which is faster and quieter than the dialog. */
export async function makeFolder(request: APIRequestContext, path: string): Promise<void> {
  const response = await request.post("/drawer/v1/folders", { data: { path }, headers: auth() });
  if (!response.ok()) throw new Error(`mkdir ${path}: ${response.status()} ${await response.text()}`);
}

/**
 * The app routes on the URL hash (`#/drive/...`), and a plain `goto` to a path that is not
 * `/` is answered by the server's SPA fallback, which keeps the hash the app had. So land on
 * the app first, then move the hash — the same way a person opening a link does.
 */
export async function openDrive(page: Page, path: string, file?: string): Promise<void> {
  await page.goto("/#/drive");
  const query = file ? `?file=${encodeURIComponent(file)}` : "";
  await page.evaluate(([p, q]) => { window.location.hash = `#/drive${p}${q}`; }, [path, query] as const);
  await page.waitForURL(new RegExp(`#/drive${escapeRegExp(path.replace(/\//g, "/"))}`));
}

/** Regex-escape a path, so a folder name is matched literally in `waitForURL`. */
export function escapeRegExp(value: string): string {
  return value.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

export interface NodeMeta {
  id: string;
  path: string;
  kind: string;
  head_revision: number | null;
}

/** The server's own view of a node, used to assert what the UI must be showing. */
export async function nodeMeta(request: APIRequestContext, path: string): Promise<NodeMeta> {
  const response = await request.get(`/drawer/v1/files/meta?path=${encodeURIComponent(path)}`, tokenAuth());
  if (!response.ok()) throw new Error(`meta ${path}: ${response.status()} ${await response.text()}`);
  return (await response.json()) as NodeMeta;
}


/** Read a file's text as the server stores it, through the machine token. */
export async function readText(request: APIRequestContext, path: string, revision?: number): Promise<string> {
  const extra = revision ? `&revision=${revision}` : "";
  const response = await request.get(`/drawer/v1/files?path=${encodeURIComponent(path)}${extra}`, tokenAuth());
  if (!response.ok()) throw new Error(`read ${path}: ${response.status()} ${await response.text()}`);
  return await response.text();
}

/** Write a file's text through the machine token, so a precondition cannot fail on auth. */
export async function writeFile(
  request: APIRequestContext,
  path: string,
  data: string,
  comment?: string,
): Promise<void> {
  const response = await request.put(`/drawer/v1/files?path=${encodeURIComponent(path)}`, {
    data,
    headers: { ...auth(), ...(comment === undefined ? {} : { "x-drawer-comment": comment }) },
  });
  if (!response.ok()) throw new Error(`write ${path}: ${response.status()} ${await response.text()}`);
}

/** The upload-only link URL the dialog shows, read out of its read-only input. */
export function uploadLinkValue(dialog: Locator): Promise<string> {
  return dialog.locator("input[aria-label='New upload-only link']").inputValue();
}

/** A row's menu, opened by role so the test does not depend on the icon markup. */
export function rowMenu(page: Page, name: string): Locator {
  return page.getByRole("button", { name: `More actions for ${name}` });
}

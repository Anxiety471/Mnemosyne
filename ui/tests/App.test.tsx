// The app renders against a mocked API: one empty folder in DRAWER_TOKEN mode.
import { render, screen, waitFor } from "@testing-library/react";
import { afterEach, expect, test, vi } from "vitest";
import { App } from "../src/App";

/** Answers the three requests the app makes on start with an empty workspace. */
function mockApi() {
  return vi.fn(async (input: RequestInfo | URL) => {
    const url = String(input);
    if (url.startsWith("/auth/me")) return new Response("", { status: 401 });
    if (url.startsWith("/drawer/v1/update")) {
      return Response.json({ current: "0.1.0", latest: "0.1.0", update_available: false });
    }
    if (url.includes("/tree?")) return Response.json({ entries: [] });
    throw new Error(`unexpected request: ${url}`);
  });
}

afterEach(() => vi.unstubAllGlobals());

test("renders the sidebar, the main region and the empty folder state", async () => {
  vi.stubGlobal("fetch", mockApi());
  render(<App />);

  expect(screen.getByRole("navigation", { name: "Drawer" })).toBeTruthy();
  const main = screen.getByRole("main", { name: "Files" });
  expect(main).toBeTruthy();
  expect(screen.getByRole("link", { name: /My Drawer/ })).toBeTruthy();

  await waitFor(() =>
    expect(screen.getByText("This folder is empty. Drop files here, or use New to upload or create one.")).toBeTruthy(),
  );
  expect(main.textContent).toContain("This folder is empty");
});

test("asks for the token when the server answers 401", async () => {
  vi.stubGlobal("fetch", mockApi());
  render(<App />);

  // The tree request is unauthorized, so the app raises the access-token dialog.
  window.dispatchEvent(new Event("drawer:unauthorized"));
  await waitFor(() => expect(screen.getByRole("dialog")).toBeTruthy());
  expect(screen.getByText("Access token")).toBeTruthy();
});

// Pause/Resume is the one part of the upload flow that the browser e2e covers, and it is also the
// part that has already broken once: pausing cleared the progress bar, which unmounted the only
// Resume button there was. These tests pin the flow without the browser.
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, expect, test, vi } from "vitest";
import { App } from "../src/App";
import { hashFile } from "../src/checksum";

// jsdom has no Worker, and the upload path hashes the file in one. The hash value is irrelevant
// here — the mock server accepts anything — so hashing runs on the main thread instead.
class HashWorker {
  onmessage: ((event: MessageEvent) => void) | null = null;
  onerror: ((event: ErrorEvent) => void) | null = null;
  terminate() {}
  postMessage(file: Blob) {
    const worker = this;
    void hashFile(file).then((hash) => worker.onmessage?.({ data: { hash } } as MessageEvent));
  }
}

/**
 * One 8 MiB file means two 4 MiB chunks. The mock accepts a chunk on the second PUT only after
 * `release`, so the first chunk is in flight when the test pauses — the real "paused mid-batch"
 * state rather than a batch that finished before the pause arrived.
 */
const FILE_SIZE = 8 * 1024 * 1024;
const release = { current: null as null | (() => void) };

function transferLog() {
  return vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
    const url = String(input);
    if (url.startsWith("/auth/me")) return new Response("", { status: 401 });
    if (url.startsWith("/drawer/v1/update")) return Response.json({ current: "0.1.0", latest: "0.1.0", update_available: false });
    if (url.includes("/tree?")) return Response.json({ entries: [] });
    if (url === "/drawer/v1/transfers") return Response.json({ id: "tr-1" });
    if (url.endsWith("/transfers/tr-1/complete")) return Response.json({ node: { hash: "deadbeef" }, revision: 1 });
    if (url.endsWith("/transfers/tr-1")) {
      if (init?.method === "PUT") {
        // A real fetch rejects with AbortError the moment the signal fires, even mid-body; that
        // rejection is what the caller turns into a paused batch.
        await new Promise<Response>((_resolve, reject) => {
          const abort = () => reject(new DOMException("Upload paused", "AbortError"));
          if (init.signal?.aborted) return abort();
          init.signal?.addEventListener("abort", abort, { once: true });
          release.current = abort;
        });
        return Response.json({ received: FILE_SIZE });
      }
      return Response.json({ id: "tr-1", path: "/a.bin", size: FILE_SIZE, sha256: "deadbeef", received: 0 });
    }
    throw new Error(`unexpected request: ${init?.method ?? "GET"} ${url}`);
  });
}

afterEach(() => {
  release.current = null;
  vi.unstubAllGlobals();
});

test("pausing a batch keeps the progress bar and its Resume button on screen", async () => {
  vi.stubGlobal("fetch", transferLog());
  vi.stubGlobal("Worker", HashWorker);
  render(<App />);

  const input = document.querySelector<HTMLInputElement>("input[type=file][multiple]")!;
  fireEvent.change(input, { target: { files: [new File(["x".repeat(FILE_SIZE)], "a.bin")] } });

  await waitFor(() => expect(screen.getByRole("button", { name: "Pause" })).toBeTruthy());
  expect(screen.queryByRole("button", { name: "Resume" })).toBeNull();

  fireEvent.click(screen.getByRole("button", { name: "Pause" }));

  // The bug: the bar unmounted, so Resume never appeared even though `paused` was set.
  await waitFor(() => expect(screen.getByRole("button", { name: "Resume" })).toBeTruthy());
  expect(screen.getByRole("button", { name: "Pause" })).toBeNull();
  expect(screen.getByLabelText("Upload progress")).toBeTruthy();
  expect(screen.getByText(/Paused at/)).toBeTruthy();

  // The batch is still resumable, and resuming drops the bar again.
  release.current?.();
  fireEvent.click(screen.getByRole("button", { name: "Resume" }));
  await waitFor(() => expect(screen.getByRole("button", { name: "Pause" })).toBeTruthy());
  expect(screen.queryByRole("button", { name: "Resume" })).toBeNull();
});

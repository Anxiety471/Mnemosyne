import { test, expect, type Locator, type Page } from "@playwright/test";
import { signIn, openDrive, makeFolder, tokenAuth } from "../actions";
import { flowFolder } from "../flows";

/** The batch progress line, which is the only thing that names the file being uploaded. */
const batch = (page: Page): Locator => page.locator(".transfer-progress");

test.describe("uploads", () => {
  test("a batch upload reports progress, pauses, and resumes", async ({ page, request }) => {
    const folder = flowFolder("uploads");
    await makeFolder(request, folder);

    await signIn(page);
    await openDrive(page, folder);

    // Slow the chunk requests down so the batch is still in flight when it is paused.
    await page.route("**/drawer/v1/transfers/**", async (route) => {
      if (route.request().method() === "PUT") await new Promise((r) => setTimeout(r, 4000));
      await route.continue();
    });

    await page.getByRole("button", { name: "New" }).click();
    await page.getByRole("menuitem", { name: "Upload files" }).click();
    await page.locator("input[type=file]").first().setInputFiles([
      { name: "first.bin", mimeType: "application/octet-stream", buffer: Buffer.alloc(4 * 1024 * 1024, 1) },
      { name: "second.bin", mimeType: "application/octet-stream", buffer: Buffer.alloc(4 * 1024 * 1024, 2) },
    ]);

    // The batch line names the file, its position in the batch and the total bytes.
    await expect(batch(page)).toContainText("Uploading 1/2: first.bin");
    await expect(batch(page)).toContainText("of 8.0 MB");
    await expect(batch(page).getByRole("button", { name: "Pause" })).toBeVisible();

    // Pausing aborts the in-flight request; the line stays and offers Resume.
    await batch(page).getByRole("button", { name: "Pause" }).click();
    await expect(page.locator(".toast")).toContainText("Upload paused. Use Resume to continue.");
    await expect(batch(page).getByRole("button", { name: "Resume" })).toBeVisible();

    // Resuming continues the same batch; the second file is the one that completes.
    await batch(page).getByRole("button", { name: "Resume" }).click();
    await expect(page.locator(".toast")).toContainText("Uploaded 2 files");
    await expect(batch(page)).toHaveCount(0);
    await expect(page.getByRole("button", { name: "first.bin" })).toBeVisible();
    await expect(page.getByRole("button", { name: "second.bin" })).toBeVisible();
  });

  test("the Transfers panel shows the finished upload and its byte count", async ({ page, request }) => {
    const folder = flowFolder("transfers");
    await makeFolder(request, folder);
    await signIn(page);
    await openDrive(page, folder);

    await page.getByRole("button", { name: "New" }).click();
    await page.getByRole("menuitem", { name: "Upload files" }).click();
    await page.locator("input[type=file]").first().setInputFiles({
      name: "counted.txt",
      mimeType: "text/plain",
      buffer: Buffer.from("uploaded through the browser\n"),
    });
    await expect(page.locator(".toast")).toContainText("Uploaded 1 file");

    await page.getByRole("button", { name: "Transfers" }).click();
    const dialog = page.getByRole("dialog", { name: "Transfers" });
    await dialog.waitFor();
    await expect(dialog.locator("li", { hasText: "counted.txt" }).first()).toBeVisible();
    // `completed` is the server-side status for a transfer that reached its full size.
    await expect(dialog.getByText(/upload · completed ·/).first()).toBeVisible();
    await dialog.getByRole("button", { name: "Done" }).click();
    await expect(dialog).toHaveCount(0);

    // And the API agrees the transfer completed, which is what the panel read.
    const transfers = (await (await request.get("/drawer/v1/transfers", tokenAuth())).json()) as {
      transfers: { path: string; status: string; received: number; size: number }[];
    };
    const record = transfers.transfers.find((t) => t.path.endsWith("counted.txt"));
    expect(record).toBeTruthy();
    expect(record!.status).toBe("completed");
    expect(record!.received).toBe(record!.size);
  });
});

import { test, expect, type BrowserContext } from "@playwright/test";
import { signIn, openDrive, makeFolder, nodeMeta, uploadLinkValue } from "../actions";
import { flowFolder } from "../flows";

test.describe("deposit", () => {
  test("an upload-only link accepts files anonymously, in chunks, into the workspace", async ({
    page,
    request,
    browser,
  }) => {
    const folder = flowFolder("deposit");
    await makeFolder(request, folder);

    // Create the upload-only link through the UI, exactly as a person would.
    await signIn(page);
    await openDrive(page, folder);
    await page.getByRole("button", { name: `More actions for ${folder.split("/").pop()}` }).click();
    await page.getByRole("menuitem", { name: "Create upload-only link" }).click();
    const dialog = page.getByRole("dialog", { name: `Upload-only links for ${folder.split("/").pop()}` });
    await dialog.waitFor();
    await dialog.getByLabel("Upload link days").fill("2");
    await dialog.getByRole("button", { name: "Create upload-only link" }).click();
    const url = await uploadLinkValue(dialog);
    expect(url).toContain("/drop/");
    await dialog.getByRole("button", { name: "Done" }).click();
    await expect(dialog).toHaveCount(0);

    // A fresh context holds no session cookie and no token, so the link alone is the credential.
    const guest: BrowserContext = await browser.newContext();
    const deposit = await guest.newPage();
    await deposit.goto(url);
    await expect(deposit.getByRole("heading", { name: "Upload to Drawer" })).toBeVisible();

    // Two files at once; the page uploads them one after another, in chunks.
    await deposit.locator("#files").setInputFiles([
      { name: "one.txt", mimeType: "text/plain", buffer: Buffer.from("first deposit\n") },
      { name: "two.txt", mimeType: "text/plain", buffer: Buffer.from("second deposit\n") },
    ]);
    await deposit.getByRole("button", { name: "Upload files" }).click();
    await expect(deposit.getByRole("status")).toContainText("All files processed.", { timeout: 30_000 });
    await expect(deposit.locator("li.row")).toHaveCount(2);

    // Each row reports its own completion, and both files landed in the linked folder.
    for (const name of ["one.txt", "two.txt"]) {
      const row = deposit.locator("li.row", { hasText: name });
      await expect(row).toHaveClass(/done/);
      await expect(row.locator(".status")).toContainText("done");
    }
    await expect(nodeMeta(request, `${folder}/one.txt`)).resolves.toBeTruthy();
    await expect(nodeMeta(request, `${folder}/two.txt`)).resolves.toBeTruthy();

    // ... and the signed-in workspace shows them.
    await page.reload();
    await openDrive(page, folder);
    await expect(page.getByRole("button", { name: "one.txt" })).toBeVisible();
    await expect(page.getByRole("button", { name: "two.txt" })).toBeVisible();

    await guest.close();
  });
});

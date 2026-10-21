import { test, expect } from "@playwright/test";
import { signIn, openDrive, makeFolder, writeFile, nodeMeta } from "../actions";
import { flowFolder } from "../flows";

test.describe("trash", () => {
  test("a file can be trashed, undone from the toast, and purged after confirming", async ({
    page,
    request,
  }) => {
    const folder = flowFolder("trash");
    await makeFolder(request, folder);
    const file = `${folder}/gone-soon.txt`;
    await writeFile(request, file, "temporary\n", "trash me");

    await signIn(page);
    await openDrive(page, folder);
    const row = page.getByRole("button", { name: "More actions for gone-soon.txt" });
    await row.click();
    await page.getByRole("menuitem", { name: "Move to trash" }).click();

    // The toast offers an undo, and the row disappears from the folder.
    const toast = page.locator(".toast");
    await expect(toast).toContainText("Moved “gone-soon.txt” to the trash");
    await expect(toast.getByRole("button", { name: "Undo" })).toBeVisible();
    await expect(row).toHaveCount(0);

    // The node really is trashed server-side, not just hidden.
    expect((await nodeMeta(request, file)).trashed).toBe(true);

    // Undo puts it back in the folder.
    await toast.getByRole("button", { name: "Undo" }).click();
    await expect(page.getByRole("button", { name: "More actions for gone-soon.txt" })).toBeVisible();

    // Trash it again, then purge it from the Trash view with the confirmation dialog.
    await page.getByRole("button", { name: "More actions for gone-soon.txt" }).click();
    await page.getByRole("menuitem", { name: "Move to trash" }).click();
    await expect(toast).toContainText("Moved “gone-soon.txt” to the trash");

    await page.getByRole("link", { name: "Trash" }).click();
    await expect(page.locator("main")).toContainText("Items in the trash keep their history");
    await page.getByRole("button", { name: "Delete gone-soon.txt forever" }).click();
    const confirm = page.getByRole("dialog", { name: "Delete “gone-soon.txt” forever?" });
    await confirm.waitFor();
    await confirm.getByRole("button", { name: "Delete forever" }).click();

    await expect(page.locator(".empty")).toContainText("Trash is empty.");
    // Purging removed the node outright: the API no longer knows the path.
    const meta = await request.get(`/drawer/v1/files/meta?path=${encodeURIComponent(file)}`);
    expect(meta.status()).toBe(404);
  });
});

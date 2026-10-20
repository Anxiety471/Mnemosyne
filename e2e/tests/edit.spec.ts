import { test, expect } from "@playwright/test";
import { signIn, openDrive, makeFolder, nodeMeta, readText, writeFile } from "../actions";
import { flowFolder } from "../flows";

test.describe("edit", () => {
  test("saving a text file keeps a non-ASCII comment, shows a diff and restores an older revision", async ({
    page,
    request,
  }) => {
    const folder = flowFolder("edit");
    await makeFolder(request, folder);
    const file = `${folder}/notes.md`;
    await writeFile(request, file, "first line\n", "first");

    await signIn(page);
    await openDrive(page, folder, file);

    // Markdown opens on the rendered preview, so switch to the editor first.
    await page.getByRole("tab", { name: "Edit" }).click();
    const editor = page.getByLabel("File contents");
    await expect(editor).toHaveValue("first line\n");
    await editor.fill("first line\nsecond line\n");
    await page.getByPlaceholder("Describe your change (optional)").fill("café ☕");
    await page.getByRole("button", { name: "Save revision" }).click();
    await expect(page.getByRole("status")).toContainText("Saved revision 2");

    // The comment comes back exactly as typed, including the non-ASCII bytes.
    const history = page.locator("aside.history");
    await expect(history.getByText("café ☕")).toBeVisible();

    // The diff of revision 2 shows the added line.
    await page.getByRole("button", { name: "Changes in revision 2" }).click();
    const changes = page.getByRole("table", { name: "Changes" });
    await expect(changes.getByText("second line")).toBeVisible();
    await page.getByRole("button", { name: "Close" }).click();

    // Opening revision 1 is read-only and offers a restore.
    await history.getByText("Revision 1").click();
    await expect(page.getByRole("status")).toContainText("Viewing revision 1");
    await expect(editor).toHaveValue("first line\n");
    await page.getByRole("button", { name: "Restore this version" }).click();
    const confirm = page.getByRole("dialog", { name: "Restore revision 1?" });
    await confirm.getByRole("button", { name: "Restore" }).click();
    await expect(page.getByRole("status")).toContainText("Restored revision 1");

    // The restore added revision 3 with revision 1's bytes.
    expect((await nodeMeta(request, file)).head_revision).toBe(3);
    expect(await readText(request, file)).toBe("first line\n");
  });
});

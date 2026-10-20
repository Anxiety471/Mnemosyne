import { test, expect, type Page } from "@playwright/test";
import { signIn, openDrive, makeFolder } from "../actions";
import { flowFolder } from "../flows";

test.describe("browse", () => {
  test("a folder can be created and files uploaded by chooser and by drop, then opened", async ({
    page,
    request,
  }) => {
    const folder = flowFolder("browse");
    await makeFolder(request, folder);
    await signIn(page);
    await openDrive(page, folder);

    // New folder, created from the sidebar menu. The "create in" label is the field's
    // accessible name, so the test never depends on the input's position.
    await page.getByRole("button", { name: "New" }).click();
    await page.getByRole("menuitem", { name: "New folder" }).click();
    const newFolder = page.getByRole("dialog", { name: "New folder" });
    await newFolder.waitFor();
    await newFolder.getByLabel(`Create in ${folder}`).fill("notes");
    await newFolder.getByRole("button", { name: "Create" }).click();
    await newFolder.waitFor({ state: "detached" });
    await expect(page.getByRole("button", { name: "notes" })).toBeVisible();

    // Upload through the file chooser. The hidden inputs live in the body, not in <main>.
    await page.getByRole("button", { name: "New" }).click();
    await page.getByRole("menuitem", { name: "Upload files" }).click();
    await page.locator("input[type=file]").first().setInputFiles({
      name: "chooser.txt",
      mimeType: "text/plain",
      buffer: Buffer.from("hello from a chooser\n"),
    });
    await expect(page.getByRole("status")).toContainText("Uploaded 1 file");
    await expect(page.getByRole("button", { name: "chooser.txt" })).toBeVisible();

    // Upload by dropping a File onto the content pane. The app listens for the real drag
    // events on <main>, so the test drives those events with a populated DataTransfer.
    await dropFile(page, "second.txt", "second file\n");
    await expect(page.locator("main")).toHaveClass(/dropping/);
    await expect(page.getByRole("status")).toContainText("Uploaded 1 file");
    await expect(page.getByRole("button", { name: "second.txt" })).toBeVisible();

    // The file opens and shows its text.
    await page.getByRole("button", { name: "second.txt" }).click();
    await expect(page.getByLabel("File contents")).toHaveValue("second file\n");
  });
});

/**
 * Drop one file onto the content pane the way a browser does: `dragover` first, which the
 * app uses to arm its drop zone, then `drop` with the file in the DataTransfer.
 */
export async function dropFile(page: Page, name: string, contents: string): Promise<void> {
  await page.evaluate(
    ([n, c]) => {
      const main = document.querySelector("main")!;
      const data = new DataTransfer();
      data.items.add(new File([new TextEncoder().encode(c)], n, { type: "text/plain" }));
      main.dispatchEvent(new DragEvent("dragover", { bubbles: true, cancelable: true, dataTransfer: data }));
      main.dispatchEvent(new DragEvent("drop", { bubbles: true, cancelable: true, dataTransfer: data }));
    },
    [name, contents] as const,
  );
}

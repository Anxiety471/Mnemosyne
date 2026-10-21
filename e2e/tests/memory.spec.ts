import { test, expect } from "@playwright/test";
import { signIn, openDrive, makeFolder, writeFile, tokenAuth } from "../actions";
import { flowFolder } from "../flows";

test.describe("memory", () => {
  test("a note can be saved with tags, its tags edited, and opened from the graph list by keyboard", async ({
    page,
    request,
  }) => {
    // A second note, so the graph has something to draw an edge from.
    await request.put("/drawer/v1/memory/note", {
      data: { name: "Linked Note", content: "linked\n", tags: ["gamma"] },
      headers: tokenAuth(),
    });

    await signIn(page);
    await page.getByRole("button", { name: "Memories" }).click();
    const dialog = page.getByRole("dialog", { name: "Memories" });
    await dialog.waitFor();

    // A note with tags, created through the panel.
    await dialog.getByRole("button", { name: "+ New memory" }).click();
    await dialog.locator("input").first().fill("Probe Note");
    await dialog.locator("textarea").fill("Body mentioning [[Linked Note]] for the graph.\n");
    await dialog.locator("input").nth(1).fill("alpha, beta");
    await dialog.getByRole("button", { name: "Save" }).click();

    // The note opens and shows both tags as chips.
    await expect(dialog.getByText("Probe Note")).toBeVisible();
    await expect(dialog.getByRole("button", { name: "#alpha" })).toBeVisible();
    await expect(dialog.getByRole("button", { name: "#beta" })).toBeVisible();

    // Editing an existing note's tags: drop beta, keep alpha, add gamma.
    await dialog.getByRole("button", { name: "Edit" }).click();
    await dialog.getByLabel("Tags").fill("alpha, gamma");
    await dialog.getByRole("button", { name: "Save" }).click();
    await expect(dialog.getByRole("button", { name: "#alpha" })).toBeVisible();
    await expect(dialog.getByRole("button", { name: "#gamma" })).toBeVisible();
    await expect(dialog.getByRole("button", { name: "#beta" })).toHaveCount(0);

    // The graph view lists notes; opening one by keyboard works without a pointer.
    await dialog.getByRole("button", { name: "Back to list" }).click();
    await dialog.getByRole("button", { name: "Graph" }).click();
    const noteButton = dialog.getByRole("button", { name: /Probe Note/ });
    await expect(noteButton).toBeVisible();
    await noteButton.focus();
    await page.keyboard.press("Enter");
    await expect(dialog.getByText("Probe Note")).toBeVisible();
    await expect(dialog.getByRole("button", { name: "#gamma" })).toBeVisible();
  });

  test("a note written by an agent is listed and opens from the graph list by keyboard", async ({
    page,
    request,
  }) => {
    const folder = flowFolder("memory");
    await makeFolder(request, folder);
    // Seeding through the REST API, the way an agent's memory_remember tool does.
    await writeFile(request, "/memory/Linked Note.md", "linked\n", "graph");

    await signIn(page);
    await page.getByRole("button", { name: "Memories" }).click();
    const dialog = page.getByRole("dialog", { name: "Memories" });
    await dialog.waitFor();
    await dialog.getByRole("button", { name: "Show as list" }).click();
    const row = dialog.getByRole("button", { name: /Linked Note/ });
    await expect(row).toBeVisible();
    await row.focus();
    await page.keyboard.press("Enter");
    await expect(dialog.getByText("Linked Note")).toBeVisible();
    await expect(dialog.getByText("/memory/Linked Note.md")).toBeVisible();
  });
});

import AxeBuilder from "@axe-core/playwright";
import { test, expect } from "@playwright/test";
import { signIn, openDrive, makeFolder, writeFile } from "../actions";
import { flowFolder } from "../flows";

test.describe("accessibility", () => {
  test("the main views and a dialog pass axe with no critical violations", async ({ page, request }) => {
    const folder = flowFolder("a11y");
    await makeFolder(request, folder);
    await writeFile(request, `${folder}/a11y.txt`, "accessible content\n", "a11y");
    await signIn(page);

    // The empty drive, the folder listing and the open file all get scanned.
    await expectAxeClean(page);
    await openDrive(page, folder);
    await expectAxeClean(page);
    await page.getByRole("button", { name: "a11y.txt" }).click();
    await expect(page.getByLabel("File contents")).toBeVisible();
    await expectAxeClean(page);

    // And a modal, which is where a focus trap and inert background matter most.
    await page.getByRole("button", { name: "Back to folder" }).click();
    await page.getByRole("button", { name: "More actions for a11y.txt" }).click();
    await page.getByRole("menuitem", { name: "Share download link" }).click();
    await expect(page.getByRole("dialog", { name: "Share “a11y.txt”" })).toBeVisible();
    await expectAxeClean(page);
  });

  test("Escape closes a panel and returns focus to the trigger", async ({ page }) => {
    await signIn(page);
    const trigger = page.getByRole("button", { name: "Memories" });
    await trigger.focus();
    await trigger.click();
    const dialog = page.getByRole("dialog", { name: "Memories" });
    await dialog.waitFor();

    await page.keyboard.press("Escape");
    await expect(dialog).toHaveCount(0);
    // Focus goes back to whatever opened the modal, not to the top of the page.
    await expect(trigger).toBeFocused();
  });

  test("Tab is trapped inside an open dialog", async ({ page }) => {
    await signIn(page);
    await page.getByRole("button", { name: "Memories" }).click();
    const dialog = page.getByRole("dialog", { name: "Memories" });
    await dialog.waitFor();

    // The first Tab from the dialog's own first control stays inside the dialog.
    const first = dialog.locator("button, input, a[href]").first();
    await first.focus();
    await page.keyboard.press("Tab");
    await expect(dialog).toContainText(await page.evaluate(() => document.activeElement?.textContent ?? ""));
    expect(await page.evaluate(() => document.activeElement?.closest('[role="dialog"]') !== null)).toBe(true);

    // Walking off the last control wraps back to the first instead of escaping.
    const last = dialog.locator("button, input, a[href]").last();
    await last.focus();
    await page.keyboard.press("Tab");
    expect(await page.evaluate(() => document.activeElement?.closest('[role="dialog"]') !== null)).toBe(true);
    await expect(first).toBeFocused();
  });

  test("a row menu is reachable and navigable with the arrow keys", async ({ page, request }) => {
    const folder = flowFolder("a11y-menu");
    await makeFolder(request, folder);
    await writeFile(request, `${folder}/menu.txt`, "menu\n", "menu");
    await signIn(page);
    await openDrive(page, folder);

    const trigger = page.getByRole("button", { name: "More actions for menu.txt" });
    await trigger.focus();
    await trigger.click();
    const menu = page.getByRole("menu", { name: "Actions for menu.txt" });
    await menu.waitFor();

    // The first item is focused on open, and ArrowDown moves to the next.
    await expect(menu.getByRole("menuitem").first()).toBeFocused();
    await page.keyboard.press("ArrowDown");
    await expect(menu.getByRole("menuitem").nth(1)).toBeFocused();
    await page.keyboard.press("End");
    await expect(menu.getByRole("menuitem").last()).toBeFocused();
    await page.keyboard.press("Home");
    await expect(menu.getByRole("menuitem").first()).toBeFocused();

    // Escape closes the menu and hands focus back to the trigger.
    await page.keyboard.press("Escape");
    await expect(menu).toHaveCount(0);
    await expect(trigger).toBeFocused();
  });

  test("the skip link is the first thing in the tab order and reaches main", async ({ page }) => {
    await signIn(page);
    const skip = page.getByRole("link", { name: "Skip to main content" });
    await expect(skip).toHaveCount(1);

    // Focus the body, then Tab: the skip link is the first stop.
    await page.evaluate(() => document.body.focus());
    await page.keyboard.press("Tab");
    await expect(skip).toBeFocused();
    await page.keyboard.press("Enter");
    await expect(page.getByRole("main")).toBeFocused();
  });
});

/** axe-core's own verdict, so a failure reports the rule ids and the offending nodes. */
async function expectAxeClean(page: Page): Promise<void> {
  const results = await new AxeBuilder({ page }).analyze();
  const critical = results.violations.filter((v) => v.impact === "critical");
  expect(critical, JSON.stringify(critical, null, 2)).toEqual([]);
}

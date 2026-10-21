import { test, expect } from "@playwright/test";
import { signIn } from "../actions";

test.describe.configure({ mode: "serial" });

test("a wrong password is refused and the right one signs in", async ({ page }) => {
  await page.goto("/");
  const prompt = page.getByRole("dialog", { name: "Sign in to Drawer" });
  await prompt.waitFor({ state: "visible" });

  await prompt.getByLabel("Username").fill("admin");
  await prompt.getByLabel("Password").fill("not-the-password");
  await prompt.getByRole("button", { name: "Continue" }).click();
  await expect(page.getByRole("status")).toContainText(/incorrect/i);
  await expect(prompt).toBeVisible();

  await prompt.getByLabel("Password").fill("e2e-admin-password");
  await prompt.getByRole("button", { name: "Continue" }).click();
  await prompt.waitFor({ state: "detached" });
  await expect(page.getByRole("button", { name: "Sign out" })).toBeVisible();
});

test("signing out clears the session and the app asks for the password again", async ({ page }) => {
  await signIn(page);
  await expect(page.getByRole("button", { name: "Sign out" })).toBeVisible();

  await page.getByRole("button", { name: "Sign out" }).click();
  await expect(page.getByRole("button", { name: "Sign out" })).toHaveCount(0);

  // The cookie is gone, so the API refuses and the app asks again.
  await page.reload();
  await expect(page.getByRole("dialog", { name: "Sign in to Drawer" })).toBeVisible();
});

import { test, expect } from "@playwright/test";
import { signIn, openDrive, makeFolder, nodeMeta, writeFile, tokenAuth } from "../actions";
import { flowFolder } from "../flows";

test.describe("share", () => {
  test("a link with a custom expiry can be used anonymously from /s/", async ({ page, request, browser }) => {
    const folder = flowFolder("share");
    await makeFolder(request, folder);
    const file = `${folder}/public.txt`;
    await writeFile(request, file, "shared contents\n", "shared");

    await signIn(page);
    await openDrive(page, folder);
    await page.getByRole("button", { name: "More actions for public.txt" }).click();
    await page.getByRole("menuitem", { name: "Share download link" }).click();

    const share = page.getByRole("dialog", { name: "Share “public.txt”" });
    await share.waitFor();
    await share.getByLabel("Link days").fill("3");
    await share.getByRole("button", { name: "Create 3-day link" }).click();
    const url = await share.locator("input[readonly]").inputValue();
    expect(url).toContain("/s/");

    // A fresh browser context holds neither the session cookie nor the token.
    const anonymous = await browser.newContext();
    const download = await anonymous.request.get(url);
    expect(download.ok()).toBe(true);
    expect(await download.text()).toBe("shared contents\n");
    expect((await download.headers())["content-disposition"]).toContain("public.txt");
    await anonymous.close();

    await share.getByRole("button", { name: "Done" }).click();
  });

  test("grants can be added and removed in the permissions dialog", async ({ page, request }) => {
    const name = `perms-${Date.now().toString(36)}`;
    const folder = `/e2e/${name}`;
    await makeFolder(request, folder);

    // A second account, so the grant names a real user.
    const username = `reader${Date.now().toString(36)}`;
    await request.post("/drawer/v1/users", {
      data: { username, password: "reader-password-123", quota_bytes: 10 * 1024 * 1024 },
      headers: tokenAuth(),
    });

    await signIn(page);
    await openDrive(page, folder);
    await page.getByRole("button", { name: `More actions for ${name}` }).click();
    await page.getByRole("menuitem", { name: "Manage access" }).click();

    const dialog = page.getByRole("dialog", { name: `Sharing for “${name}”` });
    await dialog.waitFor();
    await dialog.getByLabel("Principal").fill(username);
    await dialog.getByLabel("Role").selectOption("writer");
    await dialog.getByRole("button", { name: "Add grant" }).click();
    await expect(dialog.getByText(`${username} · writer`)).toBeVisible();

    // The grant is really stored, not just drawn.
    const id = (await nodeMeta(request, folder)).id;
    const list = (await (await request.get(`/drawer/v1/nodes/${id}/permissions`, tokenAuth())).json()) as {
      permissions: { principal: string; role: string }[];
    };
    expect(list.permissions).toContainEqual({ principal: username, role: "writer" });

    await dialog.getByRole("button", { name: "Remove" }).click();
    await expect(dialog.getByText("No grants.")).toBeVisible();
  });
});

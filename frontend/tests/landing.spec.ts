import AxeBuilder from "@axe-core/playwright";
import { expect, test } from "@playwright/test";

test.skip(!process.env.LANDING_URL, "Set LANDING_URL to a Rust-generated landing fixture.");
const landingUrl = process.env.LANDING_URL || "http://127.0.0.1:4175";
const summary = {
  version: 1,
  generatedAt: "2026-09-24T09:30:00Z",
  suites: 3,
  total: 10,
  passed: 8,
  failed: 2,
  errors: 1,
};

test("landing prerender contains safe Markdown, links and branding without JavaScript", async ({
  browser,
}) => {
  const context = await browser.newContext({ javaScriptEnabled: false });
  const page = await context.newPage();
  await page.goto(landingUrl);
  await expect(page.getByRole("heading", { level: 1 })).toHaveText("Customer visual sites");
  await expect(page.locator(".report-subheading strong").first()).toHaveText("Published reports");
  await expect(
    page.getByRole("link", { name: "Commerce storefront", exact: true }),
  ).toHaveAttribute("href", "https://commerce.report.test/suites/shop");
  await expect(page.locator(".brand img")).toBeVisible();
  // Site headings and URLs share the report's text link style (light theme here).
  for (const link of [
    page.locator(".landing-site h2 a").first(),
    page.locator(".landing-url").first(),
  ]) {
    await expect(link).toHaveCSS("cursor", "pointer");
    await expect(link).toHaveCSS("text-decoration-line", "underline");
    await expect(link).toHaveCSS("color", "rgb(7, 119, 95)");
    await link.hover();
    await expect(link).toHaveCSS("color", "rgb(5, 89, 74)");
    await page.mouse.move(0, 0);
  }
  await expect(page.locator(":root")).toHaveCSS("scrollbar-gutter", "stable");
  await expect(page.locator(":root")).not.toHaveCSS("scrollbar-color", "auto");
  expect(
    await page.locator(".brand img").evaluate((image: HTMLImageElement) => image.naturalWidth),
  ).toBeGreaterThan(0);
  await expect(page.locator('a[href^="javascript:"]')).toHaveCount(0);
  await expect(page.locator(".report-subheading script, .report-subheading img")).toHaveCount(0);
  await expect(page.locator('link[rel="icon"]')).toHaveAttribute("href", /icon\.svg\?v=/);
  await context.close();
});

test("landing fetches remote status anonymously and keeps failed links usable", async ({
  page,
  context,
}, info) => {
  let retry = false;
  let requests = 0;
  await context.addCookies([
    {
      name: "unrelated-login",
      value: "must-not-be-sent",
      domain: "commerce.report.test",
      path: "/",
      secure: true,
      sameSite: "None",
    },
  ]);
  await page.route("https://*.report.test/_snapshot/status.json", async (route) => {
    requests += 1;
    const headers = await route.request().allHeaders();
    expect(headers.cookie).toBeUndefined();
    expect(headers.authorization).toBeUndefined();
    expect(headers.referer).toBeUndefined();
    const hostname = new URL(route.request().url()).hostname;
    if (hostname === "unavailable.report.test" && !retry) {
      await route.fulfill({ status: 404, headers: { "Access-Control-Allow-Origin": "*" } });
      return;
    }
    const data =
      hostname === "long.report.test"
        ? { ...summary, suites: 0, total: 0, passed: 0, failed: 0, errors: 0 }
        : hostname === "support.report.test"
          ? { ...summary, passed: 10, failed: 0, errors: 0 }
          : summary;
    await route.fulfill({ json: data, headers: { "Access-Control-Allow-Origin": "*" } });
  });
  await page.goto(landingUrl);
  const commerce = page
    .locator(".landing-site")
    .filter({ has: page.getByRole("heading", { name: "Commerce storefront" }) });
  await expect(commerce).toContainText("80%");
  await expect(
    page.locator(".landing-site-status").filter({ hasText: "Status unavailable" }),
  ).toHaveCount(1);
  await expect(page.getByRole("link", { name: "Unavailable report", exact: true })).toBeVisible();
  await expect(page.getByText("No captures", { exact: true })).toBeVisible();
  await expect(page.locator(".landing-page")).toHaveCSS("padding-top", "48px");
  expect(requests).toBe(4);
  expect(await page.evaluate(() => "landingUnsafe" in window)).toBe(false);
  const copy = await commerce.locator(".landing-site-copy").boundingBox();
  const status = await commerce.locator(".landing-site-status").boundingBox();
  if (!copy || !status) throw new Error("Landing site layout is missing.");
  if (info.project.name === "desktop") expect(status.x).toBeGreaterThan(copy.x + copy.width);
  else expect(status.y).toBeGreaterThan(copy.y);
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
  await page.getByRole("button", { name: "Dark theme", exact: true }).click();
  await expect(page.locator("html")).toHaveAttribute("data-theme", "dark");
  await page.screenshot({
    path: `.fixtures/screenshots/landing-${info.project.name}-dark.png`,
    fullPage: true,
  });
  await page.getByRole("button", { name: "Light theme", exact: true }).click();
  await expect(page.locator("html")).toHaveAttribute("data-theme", "light");
  await page.screenshot({
    path: `.fixtures/screenshots/landing-${info.project.name}-light.png`,
    fullPage: true,
  });
  expect((await new AxeBuilder({ page }).analyze()).violations).toEqual([]);
  retry = true;
  await page
    .getByRole("button", { name: "Refresh status for Unavailable report", exact: true })
    .click();
  await expect(page.getByText("Status unavailable", { exact: true })).toHaveCount(0);
  expect(requests).toBe(5);
});

test("landing isolates invalid and unreachable summaries", async ({ page }) => {
  await page.route("https://*.report.test/_snapshot/status.json", async (route) => {
    if (route.request().url().includes("commerce"))
      await route.fulfill({
        json: { ...summary, passed: 99 },
        headers: { "Access-Control-Allow-Origin": "*" },
      });
    else await route.abort("failed");
  });
  await page.goto(landingUrl);
  await expect(page.getByText("Status unavailable", { exact: true })).toHaveCount(4);
  await expect(page.locator(".landing-site h2 a")).toHaveCount(4);
});

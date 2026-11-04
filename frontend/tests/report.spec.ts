import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import AxeBuilder from "@axe-core/playwright";
import { expect, type Page, type Request, test } from "@playwright/test";
import { PNG } from "pngjs";
import { browserExecutable } from "../scripts/create-fixture";
import { getAssetVersion } from "../scripts/version-assets";

async function mockCaptureLogs(page: Page, state = "complete", timestampUtcMs = 1790812800000) {
  const records = (operationId: string, message: string, legacy: boolean) => [
    {
      schemaVersion: 1,
      operationId,
      operationKind: "capture",
      captureRunId: "run",
      execution: {
        executionId: "44444444-4444-4444-8444-444444444444",
        suiteId: legacy ? `sha256:${"a".repeat(64)}` : "log-suite",
        scenarioId: legacy ? `sha256:${"b".repeat(64)}` : "keyword-search-button",
        viewport: {
          name: legacy ? `sha256:${"c".repeat(64)}` : "desktop",
          width: 1920,
          height: 1080,
        },
        captureRole: "test",
        attempt: 1,
      },
      sequence: 0,
      timestampUtcMs,
      elapsedMs: 12,
      severity: "info",
      stage: "navigation",
      eventName: "decision",
      message,
      fields: {
        target: { type: "identity", value: legacy ? `sha256:${"d".repeat(64)}` : "#hero-search" },
        artifact: {
          type: "identity",
          value: legacy ? `sha256:${"e".repeat(64)}` : "common/_search-ready.js",
        },
      },
      spanId: "22222222-2222-4222-8222-222222222222",
      parentSpan: null,
      durationMs: 12,
      outcome: "success",
      errorCause: null,
    },
  ];
  const operation = [
    "11111111-1111-4111-8111-111111111111",
    "33333333-3333-4333-8333-333333333333",
  ];
  let requests = 0;
  const logs = operation.map((operationId, index) => {
    const bytes = JSON.stringify({
      version: 1,
      operationId,
      records: records(
        operationId,
        `Capture ${index + 1} <script>private markup</script> ${"long-word-".repeat(50)}`,
        index === 1,
      ),
    });
    const path = `_snapshot/data/logs/${operationId}/chunk-000000.json?v=${getAssetVersion(bytes)}`;
    return {
      bytes,
      path,
      data: {
        state,
        operationId,
        executionId: null,
        chunks: ["complete", "partial"].includes(state) ? [path] : [],
        records: 1,
        missingRecords: state === "complete" ? 0 : null,
        build: null,
      },
    };
  });
  await page.route("**/_snapshot/data/manifest.json?*", (route) =>
    route.fulfill({
      json: {
        version: 2,
        title: "Log fixture",
        heading: "Log fixture",
        subheading: "",
        generatedAt: "2026-10-01T00:00:00Z",
        suites: [
          {
            id: "log-suite",
            total: 2,
            passed: 1,
            failed: 1,
            errors: 0,
            report: `_snapshot/data/suites/log-suite.json?v=${"a".repeat(20)}`,
          },
        ],
      },
    }),
  );
  await page.route("**/_snapshot/data/suites/log-suite.json?*", (route) =>
    route.fulfill({
      json: {
        id: "log-suite",
        runId: "run",
        tests: logs.map((log, index) => ({
          id: `capture-${index}.png`,
          label: `Capture ${index + 1}`,
          scenario: `Scenario ${index + 1}`,
          selector: "document",
          status: index === 0 ? "pass" : "fail",
          threshold: 0.1,
          mismatch: index,
          error: null,
          reference: null,
          test: null,
          diff: null,
          referenceUrl: null,
          testUrl: null,
          dimensions: null,
          logs: { reference: log.data, test: log.data },
        })),
      },
    }),
  );
  await page.route("**/_snapshot/data/logs/**", (route) => {
    requests += 1;
    const log = logs.find((log) => route.request().url().endsWith(log.path));
    return route.fulfill({
      status: log ? 200 : 404,
      contentType: "application/json",
      body: log?.bytes || "{}",
    });
  });
  return { requests: () => requests };
}

test("capture log is lazy, deep linked, refreshable and usable on desktop and mobile", async ({
  page,
}, info) => {
  const fixture = await mockCaptureLogs(page);
  await page.goto("/suites/log-suite/images/capture-0.png");
  await expect(page.getByRole("heading", { level: 1 })).toHaveText("Capture 1");
  expect(fixture.requests()).toBe(0);
  await page.getByRole("button", { name: "Capture Log", exact: true }).click();
  await expect(page.locator(".log-records")).toContainText(
    "Capture 1 <script>private markup</script>",
  );
  await expect(page.locator(".log-records")).toContainText("suite=log-suite");
  await expect(page.locator(".log-records")).toContainText("scenario=keyword-search-button");
  await expect(page.locator(".log-records")).toContainText("#hero-search");
  await expect(page.locator(".log-records")).not.toContainText("sha256:");
  expect(fixture.requests()).toBe(1);
  await expect(page.locator("[data-log-state]")).toHaveAttribute("data-log-state", "complete");
  expect(await page.locator(".log-records script").count()).toBe(0);
  await page.getByRole("combobox", { name: "Log severity" }).selectOption("error");
  await expect(page.locator(".log-records li")).toHaveCount(0);
  await page.getByRole("combobox", { name: "Log severity" }).selectOption("info");
  await page.getByRole("combobox", { name: "Log stage" }).selectOption("navigation");
  for (const query of ["keyword-search-button", "#hero-search", "common/_search-ready.js"]) {
    await page.getByRole("searchbox", { name: "Search capture log" }).fill(query);
    await expect(page.locator(".log-records li")).toHaveCount(1);
  }
  await page.getByRole("searchbox", { name: "Search capture log" }).fill("Capture 1");
  await expect(page.locator(".log-records li")).toHaveCount(1);
  const downloading = page.waitForEvent("download");
  await page.getByRole("button", { name: "Download filtered capture log" }).click();
  const download = await downloading;
  expect(download.suggestedFilename()).toContain("capture-0.png-test-log");
  await page.goto("/suites/log-suite/images/capture-1.png?view=log&logRole=reference&status=fail");
  await expect(page.locator(".log-records")).toContainText("Capture 2");
  await expect(page.locator(".log-records")).not.toContainText("Capture 1");
  await expect(page.locator(".log-records")).toContainText("sha256:");
  await page.reload();
  await expect(page.locator(".log-records")).toContainText("Capture 2");
  await expect(page.getByRole("button", { name: "Reference", exact: true })).toHaveAttribute(
    "aria-pressed",
    "true",
  );
  await page.setViewportSize(
    info.project.name === "mobile" ? { width: 375, height: 667 } : { width: 1280, height: 800 },
  );
  await expect(page.getByRole("searchbox", { name: "Search capture log" })).toBeVisible();
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(
    true,
  );
  expect((await new AxeBuilder({ page }).analyze()).violations).toEqual([]);
  await screenshot(page, info.project.name, "capture-log");
});

test("capture log archive states remain distinct and incomplete exports are disabled", async ({
  page,
}) => {
  for (const state of ["partial", "missing", "legacyUnavailable", "loadingError"]) {
    await page.unrouteAll({ behavior: "wait" });
    await mockCaptureLogs(page, state);
    await page.goto("/suites/log-suite/images/capture-0.png?view=log");
    await expect(page.locator("[data-log-state]")).toHaveAttribute("data-log-state", state);
    await expect(page.getByRole("button", { name: /Export unavailable:/ })).toBeDisabled();
    await expect(page.getByText("Passed", { exact: true })).toBeVisible();
  }
});

test("capture log rejects invalid timestamps without crashing the report", async ({ page }) => {
  const errors: string[] = [];
  page.on("pageerror", (error) => errors.push(error.message));
  await mockCaptureLogs(page, "complete", 8640000000000001);
  await page.goto("/suites/log-suite/images/capture-0.png?view=log");
  await expect(page.locator("[data-log-state]")).toHaveAttribute("data-log-state", "loadingError");
  await expect(page.getByRole("button", { name: "Retry capture log" })).toBeVisible();
  await expect(page.getByRole("button", { name: /Export unavailable:/ })).toBeDisabled();
  await expect(page.getByRole("heading", { level: 1 })).toHaveText("Capture 1");
  expect(errors).toEqual([]);
});

const overviewSuites = [
  { displayName: "All passing", passed: 12, failed: 0, errors: 0 },
  { displayName: "Mixed results", passed: 9, failed: 1, errors: 0 },
  { displayName: "Rounded pass rate", passed: 9999, failed: 1, errors: 0 },
  { displayName: "All failing", passed: 0, failed: 4, errors: 0 },
  { displayName: "Engine error", passed: 3, failed: 1, errors: 1 },
  { displayName: "Empty suite", passed: 0, failed: 0, errors: 0 },
].map((suite) => ({ ...suite, total: suite.passed + suite.failed }));

async function mockOverview(page: Page, suites = overviewSuites) {
  await page.route("**/_snapshot/data/manifest.json?*", async (route) => {
    const response = await route.fetch();
    const manifest = await response.json();
    manifest.generatedAt = "2026-10-13T09:42:00Z";
    manifest.suites = suites.map((suite, index) => ({
      ...manifest.suites[index],
      ...suite,
      id: `summary-${index}`,
    }));
    await route.fulfill({ response, json: manifest });
  });
  await page.goto("/");
}

async function assertLayout(page: Page) {
  await expect
    .poll(() => page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth))
    .toBe(true);
  expect(await page.evaluate(() => document.fonts.check("14px Roboto"))).toBe(true);
}

async function screenshot(page: Page, project: string, name: string, fullPage = true) {
  await assertLayout(page);
  await page.screenshot({
    path: resolve(import.meta.dirname, `../.fixtures/screenshots/${project}/${name}.png`),
    fullPage,
    animations: "disabled",
  });
}

async function chooseTheme(page: Page, theme: "Light" | "Dark" | "System") {
  await page.getByRole("button", { name: "Settings", exact: true }).click();
  const popup = page.getByRole("dialog", { name: "Settings", exact: true });
  await popup.getByRole("button", { name: `${theme} theme`, exact: true }).click();
  await popup.getByRole("button", { name: "Close settings" }).click();
}

async function sortOverview(page: Page, key: string, label: string) {
  const select = page.getByRole("combobox", { name: "Sort suites by" });
  if (await select.isVisible()) {
    if ((await select.inputValue()) !== key) await select.selectOption(key);
    else await page.getByRole("button", { name: /^Sort (ascending|descending)$/ }).click();
  } else {
    await page.getByRole("columnheader", { name: label, exact: true }).getByRole("button").click();
  }
}

test("overview puts the timestamp below its heading without a breadcrumb", async ({ page }) => {
  await mockOverview(page);
  const heading = page.getByRole("heading", { name: "Snapshot Overview", exact: true });
  await expect(heading).toBeVisible();
  await expect(page.getByRole("navigation", { name: "Breadcrumb" })).toHaveCount(0);
  await expect(page.locator(".breadcrumb-bar")).toHaveCount(0);
  const date = page.locator(".overview-page .page-heading time");
  await expect(date).toHaveAttribute("datetime", "2026-10-13T09:42:00Z");
  await expect(date).toHaveText(
    await page.evaluate(() =>
      new Date("2026-10-13T09:42:00Z").toLocaleString(undefined, {
        dateStyle: "medium",
        timeStyle: "short",
      }),
    ),
  );
  const headingBounds = await heading.boundingBox();
  const dateBounds = await date.boundingBox();
  if (!headingBounds || !dateBounds) throw new Error("Overview heading is not visible");
  expect(dateBounds.y).toBeGreaterThanOrEqual(headingBounds.y + headingBounds.height);
  expect(
    await page
      .locator(".overview-page")
      .evaluate((element) => element.getBoundingClientRect().width),
  ).toBeLessThanOrEqual(1080);
});

test("overview shows engine error counts inline with hover and focus details", async ({
  page,
}, info) => {
  await mockOverview(page, [
    ...overviewSuites,
    { displayName: "Multiple errors", total: 3, passed: 0, failed: 3, errors: 3 },
  ]);
  for (const [name, count] of [
    ["Engine error", 1],
    ["Multiple errors", 3],
  ] as const) {
    const row = page.locator(".results-table tbody tr").filter({
      has: page.getByRole("link", { name, exact: true }),
    });
    const label = `${count} engine ${count === 1 ? "error" : "errors"}`;
    const badge = row.getByRole("button", { name: label, exact: true });
    await expect(badge).toHaveText(String(count));
    const linkBounds = await row.getByRole("link", { name, exact: true }).boundingBox();
    const badgeBounds = await badge.boundingBox();
    if (!linkBounds || !badgeBounds) throw new Error("Engine error badge is not visible");
    expect(badgeBounds.x).toBeGreaterThanOrEqual(linkBounds.x + linkBounds.width);
    expect(
      Math.abs(badgeBounds.y + badgeBounds.height / 2 - linkBounds.y - linkBounds.height / 2),
    ).toBeLessThanOrEqual(1);
    if (info.project.name === "mobile") await badge.tap();
    else await badge.hover();
    const tooltip = page.getByRole("tooltip");
    await expect(tooltip).toHaveText(label);
    await page.keyboard.press("Escape");
    await expect(tooltip).toHaveCount(0);
    await page.mouse.move(0, 0);
    await badge.blur();
    await badge.focus();
    await expect(tooltip).toHaveText(label);
    await page.keyboard.press("Escape");
    await expect(tooltip).toHaveCount(0);
    await badge.blur();
  }
  await expect(
    page
      .locator(".results-table tbody tr")
      .filter({
        has: page.getByRole("link", { name: "All passing", exact: true }),
      })
      .locator(".table-error"),
  ).toHaveCount(0);
});

test("overview stacks all suite metrics on narrow screens", async ({ page }, info) => {
  const suites = overviewSuites.map((suite) =>
    suite.errors ? { ...suite, displayName: `Long suite ${"unbroken".repeat(16)}` } : suite,
  );
  await mockOverview(page, suites);
  await expect(page.locator(".results-table tbody tr")).toHaveCount(suites.length);
  await page.evaluate(() => document.fonts.ready);
  for (const width of [320, 390, 600, 760]) {
    await page.setViewportSize({ width, height: 844 });
    await assertLayout(page);
    const table = page.locator(".results-table-wrap");
    expect(await table.evaluate((element) => element.scrollWidth <= element.clientWidth)).toBe(
      true,
    );
    const row = page.locator(".results-table tbody tr").first();
    await expect(row.locator("td")).toHaveCount(5);
    const cells = await row
      .locator("td")
      .evaluateAll((elements) =>
        elements.map((element) => element.getBoundingClientRect().toJSON()),
      );
    for (const [index, cell] of cells.entries()) {
      expect(cell.right).toBeLessThanOrEqual(width);
      if (index) expect(cell.top).toBeGreaterThanOrEqual(cells[index - 1]?.bottom ?? 0);
    }
    const numberOffsets = await page.locator(".results-table tbody tr").evaluateAll((rows) =>
      rows.map((row) => {
        const numbers = Array.from(row.children)
          .slice(1, 4)
          .map((element) => {
            const range = document.createRange();
            range.selectNodeContents(element.querySelector("span") || element);
            const text = range.getBoundingClientRect();
            const cell = element.getBoundingClientRect();
            const border = parseFloat(getComputedStyle(element).borderBottomWidth);
            return {
              center: text.x + text.width / 2,
              vertical: Math.abs(text.y + text.height / 2 - cell.y - (cell.height - border) / 2),
            };
          });
        const centers = numbers.map((number) => number.center);
        return {
          column: Math.max(...centers) - Math.min(...centers),
          vertical: Math.max(...numbers.map((number) => number.vertical)),
        };
      }),
    );
    for (const offsets of numberOffsets) {
      expect.soft(offsets.column, `Number column alignment at ${width}px`).toBeLessThanOrEqual(1);
      expect
        .soft(offsets.vertical, `Number vertical alignment at ${width}px`)
        .toBeLessThanOrEqual(1);
    }
    const summary = await page.locator(".metrics > div").evaluateAll((elements) =>
      elements.map((element) => ({
        top: element.getBoundingClientRect().top,
        border: getComputedStyle(element).borderTopWidth,
      })),
    );
    if ((summary[2]?.top ?? 0) > (summary[0]?.top ?? 0)) {
      expect
        .soft(
          summary.map((metric) => metric.border),
          `Summary row divider at ${width}px`,
        )
        .toEqual(["0px", "0px", "1px", "1px"]);
    }
    await expect(page.getByRole("combobox", { name: "Sort suites by" })).toBeVisible();
  }
  await page.setViewportSize({ width: 761, height: 844 });
  await expect(page.getByRole("combobox", { name: "Sort suites by" })).toBeHidden();
  expect(
    await page
      .locator(".results-table-wrap")
      .evaluate((element) => element.scrollWidth <= element.clientWidth),
  ).toBe(true);
  await page.setViewportSize({ width: 390, height: 844 });
  expect((await new AxeBuilder({ page }).analyze()).violations).toEqual([]);
  await screenshot(page, info.project.name, "overview-stacked");
});

test("report header keeps the configured heading and subheading above each page", async ({
  page,
}, info) => {
  await page.route("**/_snapshot/data/manifest.json?*", async (route) => {
    const response = await route.fetch();
    const manifest = await response.json();
    manifest.heading = "Release <summary>";
    manifest.subheading = "**Staging** / [Release notes](https://example.com/releases)";
    await route.fulfill({ response, json: manifest });
  });
  await page.goto("/");
  const header = page.locator(".app-header");
  await expect(header.locator(".report-title")).toHaveText("Release <summary>");
  await expect(header.locator(".report-subheading strong")).toHaveText("Staging");
  await expect(header.getByRole("link", { name: "Release notes" })).toHaveAttribute(
    "href",
    "https://example.com/releases",
  );
  await expect(header.getByRole("link", { name: "Release notes" })).toHaveAttribute(
    "rel",
    "noopener noreferrer",
  );
  await expect(header.getByText("Snapshot", { exact: true })).toHaveCount(0);
  await expect(page.getByRole("heading", { level: 1 })).toHaveText("Snapshot Overview");
  await expect(page.locator("#main .report-subheading")).toHaveCount(0);
  await page.evaluate(() => document.fonts.ready);
  expect(
    await header.evaluate((element) => element.getBoundingClientRect().height),
  ).toBeLessThanOrEqual(64);
  expect(
    await page
      .locator(".page-heading")
      .evaluate((element) => element.getBoundingClientRect().height),
  ).toBeLessThanOrEqual(info.project.name === "mobile" ? 92 : 52);
  expect(
    await page
      .locator(".overview-page")
      .evaluate((element) => getComputedStyle(element).paddingTop),
  ).toBe("48px");
  const titleBottom = await header
    .locator(".report-title")
    .evaluate((element) => element.getBoundingClientRect().bottom);
  expect(
    await header
      .locator(".report-subheading")
      .evaluate((element) => element.getBoundingClientRect().top),
  ).toBeGreaterThanOrEqual(titleBottom);
  await page.getByRole("link", { name: "workspace-overview", exact: true }).click();
  await expect(page.getByRole("navigation", { name: "Breadcrumb" })).toContainText(
    "workspace-overview",
  );
  await expect(page.locator(".breadcrumb-bar .snapshot-date")).toHaveCount(1);
  await expect(header.locator(".report-title")).toHaveText("Release <summary>");
  await expect(header.locator(".report-subheading strong")).toHaveText("Staging");
  await header.getByRole("link", { name: "Regressify overview" }).click();
  await expect(page.getByRole("heading", { level: 1 })).toHaveText("Snapshot Overview");
  await expect(page.getByRole("navigation", { name: "Breadcrumb" })).toHaveCount(0);
  await assertLayout(page);
});

test("settings popup contains theme choices and restores focus on dismissal", async ({
  page,
}, info) => {
  await page.emulateMedia({ colorScheme: "light" });
  await page.goto("/");
  const trigger = page.getByRole("button", { name: "Settings", exact: true, includeHidden: true });
  const popup = page.getByRole("dialog", { name: "Settings", exact: true });
  await expect(popup).toBeHidden();
  await expect(page.getByRole("group", { name: "Color theme" })).toBeHidden();
  await expect(trigger).toHaveAttribute("aria-haspopup", "dialog");
  await trigger.press("Enter");
  await expect(trigger).toHaveAttribute("aria-expanded", "true");
  await expect(popup.getByRole("heading", { name: "Settings", exact: true })).toBeVisible();
  await expect(popup.getByRole("heading", { name: "Appearance", exact: true })).toBeVisible();
  await expect(popup.getByRole("button", { name: "System theme" })).toHaveAttribute(
    "aria-pressed",
    "true",
  );
  expect(await page.evaluate(() => localStorage.getItem("regressify-theme"))).toBeNull();
  await expect(popup.getByRole("button", { name: "Close settings" })).toBeFocused();
  await page.keyboard.press("Tab");
  await expect(popup.getByRole("button", { name: "Light theme" })).toBeFocused();
  await page.keyboard.press("Shift+Tab");
  await expect(popup.getByRole("button", { name: "Close settings" })).toBeFocused();
  await popup.getByRole("button", { name: "System theme" }).focus();
  await page.keyboard.press("Tab");
  expect(
    await popup.evaluate(
      (element) =>
        document.activeElement === document.body || element.contains(document.activeElement),
    ),
  ).toBe(true);
  await popup.click({ position: { x: 8, y: 8 } });
  await expect(popup).toBeVisible();
  expect((await new AxeBuilder({ page }).analyze()).violations).toEqual([]);
  await screenshot(page, info.project.name, "settings-light");
  await popup.getByRole("button", { name: "Dark theme" }).click();
  await expect(page.locator("html")).toHaveAttribute("data-theme", "dark");
  await screenshot(page, info.project.name, "settings-dark");
  await popup.getByRole("button", { name: "Close settings" }).click();
  await expect(popup).toBeHidden();
  await expect(trigger).toBeFocused();
  await expect(trigger).toHaveAttribute("aria-expanded", "false");
  await trigger.click();
  await expect(popup.getByRole("button", { name: "Dark theme" })).toHaveAttribute(
    "aria-pressed",
    "true",
  );
  await popup.getByRole("button", { name: "System theme" }).click();
  await page.keyboard.press("Escape");
  await expect(popup).toBeHidden();
  await expect(trigger).toBeFocused();
  await page.emulateMedia({ colorScheme: "dark" });
  await expect(page.locator("html")).toHaveAttribute("data-theme", "dark");
  await page.emulateMedia({ colorScheme: "light" });
  await expect(page.locator("html")).toHaveAttribute("data-theme", "light");
  await trigger.click();
  await page.setViewportSize({ width: 320, height: 360 });
  const content = popup.locator(".settings-content");
  await content.evaluate((element) => {
    const section = document.createElement("section");
    section.className = "settings-section";
    section.style.minHeight = "600px";
    section.textContent = "Additional settings";
    element.append(section);
  });
  expect(await content.evaluate((element) => element.scrollHeight > element.clientHeight)).toBe(
    true,
  );
  await content.evaluate((element) => element.scrollTo(0, element.scrollHeight));
  await expect(popup.getByRole("button", { name: "Close settings" })).toBeVisible();
  const bounds = await popup.boundingBox();
  if (!bounds) throw new Error("Settings is not visible");
  expect(bounds.x).toBeGreaterThanOrEqual(0);
  expect(bounds.y).toBeGreaterThanOrEqual(0);
  expect(bounds.x + bounds.width).toBeLessThanOrEqual(320);
  expect(bounds.y + bounds.height).toBeLessThanOrEqual(360);
  await page.mouse.click(1, 1);
  await expect(popup).toBeHidden();
  await expect(trigger).toBeFocused();
});

for (const system of ["light", "dark"] as const) {
  for (const preference of ["light", "dark", "system", "invalid", null, "blocked"] as const) {
    test(`theme is applied before the app bundle loads (${preference ?? "unset"}, system ${system})`, async ({
      page,
    }) => {
      const errors: string[] = [];
      page.on("pageerror", (error) => errors.push(error.message));
      await page.emulateMedia({ colorScheme: system });
      await page.addInitScript((saved) => {
        if (saved === "blocked") {
          Object.defineProperty(window, "localStorage", {
            get() {
              throw new DOMException("Storage is blocked", "SecurityError");
            },
          });
        } else if (saved === null) {
          localStorage.removeItem("regressify-theme");
        } else {
          localStorage.setItem("regressify-theme", saved);
        }
      }, preference);
      const selected = preference === "light" || preference === "dark" ? preference : "system";
      const resolved = selected === "system" ? system : selected;
      const appBundle = /\/(?:src\/main\.tsx|_snapshot\/assets\/app\.js)(?:\?|$)/;
      await page.route(appBundle, (route) => route.abort());
      await page.goto("/");
      await expect(page.locator("#root")).toBeEmpty();
      await expect(page.locator("html")).toHaveAttribute("data-theme", resolved);
      await expect(page.locator("html")).toHaveCSS("color-scheme", resolved);
      const firstPaint = PNG.sync.read(
        await page.screenshot({ clip: { x: 32, y: 32, width: 1, height: 1 }, scale: "css" }),
      );
      const background = [...firstPaint.data.subarray(0, 3)].join(", ");
      expect(`rgb(${background})`).toBe(
        resolved === "dark" ? "rgb(21, 25, 25)" : "rgb(247, 248, 248)",
      );
      await page.unroute(appBundle);
      await page.reload();
      await page.getByRole("button", { name: "Settings", exact: true }).click();
      await expect(
        page.getByRole("button", { name: `${selected} theme`, exact: false }),
      ).toHaveAttribute("aria-pressed", "true");
      await expect(page.locator("html")).toHaveAttribute("data-theme", resolved);
      if (preference === "blocked") {
        const next = resolved === "dark" ? "light" : "dark";
        await page.getByRole("button", { name: `${next} theme`, exact: false }).click();
        await expect(page.locator("html")).toHaveAttribute("data-theme", next);
      } else {
        expect(await page.evaluate(() => localStorage.getItem("regressify-theme"))).toBe(
          preference,
        );
      }
      expect(errors).toEqual([]);
    });
  }
}

test("theme choices sync across tabs and only system mode follows OS changes", async ({
  page,
  context,
}) => {
  const other = await context.newPage();
  await page.emulateMedia({ colorScheme: "light" });
  await other.emulateMedia({ colorScheme: "dark" });
  await page.goto("/");
  await other.goto("/");
  await expect(
    page.getByRole("button", { name: "System theme", includeHidden: true }),
  ).toHaveAttribute("aria-pressed", "true");
  await expect(
    other.getByRole("button", { name: "System theme", includeHidden: true }),
  ).toHaveAttribute("aria-pressed", "true");
  await chooseTheme(page, "Dark");
  await expect(
    other.getByRole("button", { name: "Dark theme", includeHidden: true }),
  ).toHaveAttribute("aria-pressed", "true");
  await other.emulateMedia({ colorScheme: "light" });
  await expect(other.locator("html")).toHaveAttribute("data-theme", "dark");
  await chooseTheme(other, "Light");
  await expect(
    page.getByRole("button", { name: "Light theme", includeHidden: true }),
  ).toHaveAttribute("aria-pressed", "true");
  await page.emulateMedia({ colorScheme: "dark" });
  await expect(page.locator("html")).toHaveAttribute("data-theme", "light");
  await chooseTheme(other, "System");
  await expect(
    page.getByRole("button", { name: "System theme", includeHidden: true }),
  ).toHaveAttribute("aria-pressed", "true");
  await expect(page.locator("html")).toHaveAttribute("data-theme", "dark");
  await expect(other.locator("html")).toHaveAttribute("data-theme", "light");
  await page.emulateMedia({ colorScheme: "light" });
  await expect(page.locator("html")).toHaveAttribute("data-theme", "light");
});

test("theme storage resets restore system mode without being overwritten", async ({
  page,
  context,
}) => {
  await page.emulateMedia({ colorScheme: "light" });
  await page.goto("/");
  const other = await context.newPage();
  await other.goto("/");
  for (const reset of ["invalid", "remove", "clear"] as const) {
    await chooseTheme(page, "Dark");
    await expect(
      other.getByRole("button", { name: "Dark theme", includeHidden: true }),
    ).toHaveAttribute("aria-pressed", "true");
    await other.evaluate((mode) => {
      if (mode === "clear") localStorage.clear();
      else if (mode === "remove") localStorage.removeItem("regressify-theme");
      else localStorage.setItem("regressify-theme", "invalid");
    }, reset);
    await expect(
      page.getByRole("button", { name: "System theme", includeHidden: true }),
    ).toHaveAttribute("aria-pressed", "true");
    await expect(page.locator("html")).toHaveAttribute("data-theme", "light");
    expect(await page.evaluate(() => localStorage.getItem("regressify-theme"))).toBe(
      reset === "invalid" ? "invalid" : null,
    );
  }
});

test("overview is responsive, accessible, sortable, and theme-aware", async ({ page }, info) => {
  const errors: string[] = [];
  page.on("pageerror", (error) => errors.push(error.message));
  await page.goto("/index.html");
  await expect(page).toHaveURL(/\/$/);
  await expect(page.getByRole("heading", { name: "Snapshot Overview", exact: true })).toBeVisible();
  await expect(page.locator(".app-header .report-title")).toHaveText(
    "A clear view of every change.",
  );
  await expect(page.locator(".results-table tbody tr")).toHaveCount(7);
  await expect(page.locator(".metrics > div > span")).toHaveText([
    "TOTAL TESTS",
    "PASSED",
    "FAILED",
    "PASS RATE",
  ]);
  await expect(page.locator(".metrics svg")).toHaveCount(0);
  for (const label of ["Test suite", "Total tests", "Passed", "Failed", "Pass rate"]) {
    await expect(page.getByRole("columnheader", { name: label, exact: true })).toHaveCount(1);
  }
  await sortOverview(page, "name", "Test suite");
  await expect(page.locator(".results-table th").first()).toHaveAttribute(
    "aria-sort",
    "descending",
  );
  await screenshot(page, info.project.name, "overview-light");
  expect((await new AxeBuilder({ page }).analyze()).violations).toEqual([]);
  await chooseTheme(page, "Dark");
  await expect(page.locator("html")).toHaveAttribute("data-theme", "dark");
  await screenshot(page, info.project.name, "overview-dark");
  await page.reload();
  await expect(page.locator("html")).toHaveAttribute("data-theme", "dark");
  await chooseTheme(page, "System");
  await page.emulateMedia({ colorScheme: "light" });
  await expect(page.locator("html")).toHaveAttribute("data-theme", "light");
  expect(errors).toEqual([]);
});

test("pass-rate bars distinguish failing suites in light and dark themes", async ({
  page,
}, info) => {
  await mockOverview(page);
  await expect(page.locator(".metrics strong")).toHaveText(["10030", "10023", "7", "99.9%"]);
  for (const theme of ["Light", "Dark"] as const) {
    await chooseTheme(page, theme);
    const neutralColor = await page
      .locator(".section-title .muted")
      .evaluate((element) => getComputedStyle(element).color);
    for (const suite of overviewSuites) {
      const row = page.locator(".results-table tbody tr").filter({
        has: page.getByRole("link", { name: suite.displayName, exact: true }),
      });
      await expect(row.locator("td").nth(1)).toHaveText(String(suite.total));
      await expect(row.locator("td").nth(2)).toHaveText(String(suite.passed));
      await expect(row.locator("td").nth(3)).toHaveText(String(suite.failed));
      const fullPass = suite.total > 0 && suite.failed === 0;
      await expect(row.locator(".result-count.pass")).toHaveCount(fullPass ? 1 : 0);
      await expect(row.locator(".result-count.fail")).toHaveCount(suite.failed ? 1 : 0);
      const progress = row.locator("progress");
      const statusColor = await row
        .locator(".status-dot")
        .evaluate((element) => getComputedStyle(element).backgroundColor);
      await expect(row.locator(".result-count").first()).toHaveCSS(
        "color",
        fullPass ? statusColor : neutralColor,
      );
      await expect(row.locator(".result-count").last()).toHaveCSS(
        "color",
        suite.failed ? statusColor : neutralColor,
      );
      await expect(progress).toHaveAttribute("value", String(suite.passed));
      await expect(progress).toHaveAttribute("max", String(suite.total || 1));
      const image = PNG.sync.read(await progress.screenshot());
      const sampleWidth = suite.passed ? (image.width * suite.passed) / suite.total : image.width;
      const offset = (Math.floor(image.height / 2) * image.width + Math.floor(sampleWidth / 2)) * 4;
      const color = [...image.data.subarray(offset, offset + 3)].join(", ");
      const expectedColor = suite.passed
        ? suite.failed
          ? neutralColor
          : statusColor
        : await row
            .locator("td")
            .first()
            .evaluate((element) => getComputedStyle(element).borderBottomColor);
      expect(`rgb(${color})`, `${suite.displayName} bar in ${theme} theme`).toBe(expectedColor);
    }
    await page.locator(".results-table-wrap").evaluate((element) => element.scrollTo(0, 0));
    await screenshot(page, info.project.name, `pass-rates-${theme.toLowerCase()}`);
  }
});

test("overview sorts every column in both directions and preserves filters", async ({
  page,
}, info) => {
  await mockOverview(page);
  const links = page.locator(".results-table .suite-name");
  for (const { key, label } of [
    { key: "name", label: "Test suite" },
    { key: "total", label: "Total tests" },
    { key: "passed", label: "Passed" },
    { key: "failed", label: "Failed" },
    { key: "rate", label: "Pass rate" },
  ] as const) {
    const header = page.getByRole("columnheader", { name: label, exact: true });
    for (const direction of ["ascending", "descending"] as const) {
      if (key !== "name" || direction !== "ascending") await sortOverview(page, key, label);
      await expect(header).toHaveAttribute("aria-sort", direction);
      await expect(page.locator('.results-table th:not([aria-sort="none"])')).toHaveCount(1);
      const expected = [...overviewSuites].sort((first, second) => {
        const order =
          key === "name"
            ? first.displayName.localeCompare(second.displayName)
            : key === "rate"
              ? first.passed / (first.total || 1) - second.passed / (second.total || 1)
              : first[key] - second[key];
        return direction === "ascending" ? order : -order;
      });
      await expect(links).toHaveText(expected.map((suite) => suite.displayName));
    }
  }
  await page.getByRole("searchbox", { name: "Search suites" }).fill("e");
  await page
    .getByRole("group", { name: "Filter results" })
    .getByRole("button", { name: /^Failed/ })
    .click();
  const passedHeader = page.getByRole("columnheader", { name: "Passed", exact: true });
  await sortOverview(page, "passed", "Passed");
  await expect(passedHeader).toHaveAttribute("aria-sort", "ascending");
  await expect(links).toHaveText(["Engine error", "Mixed results", "Rounded pass rate"]);
  const directionControl =
    info.project.name === "mobile"
      ? page.getByRole("button", { name: /^Sort (ascending|descending)$/ })
      : passedHeader.getByRole("button");
  await directionControl.press("Enter");
  await expect(passedHeader).toHaveAttribute("aria-sort", "descending");
  await page.reload();
  await expect(links).toHaveText(["Rounded pass rate", "Mixed results", "Engine error"]);
  const params = new URL(page.url()).searchParams;
  expect(params.get("q")).toBe("e");
  expect(params.get("status")).toBe("fail");
  expect(params.get("sort")).toBe("passed");
  expect(params.get("direction")).toBe("desc");
  await expect(page.locator(".metrics strong")).toHaveText(["10030", "10023", "7", "99.9%"]);
  await directionControl.press("Enter");
  await expect(passedHeader).toHaveAttribute("aria-sort", "ascending");
  await expect(links).toHaveText(["Engine error", "Mixed results", "Rounded pass rate"]);
  await assertLayout(page);
});

test("page scrollbar gutters stay stable on every report route", async ({ page }) => {
  await page.emulateMedia({ reducedMotion: "reduce" });
  for (const { path, content } of [
    { path: "/", content: ".metrics" },
    { path: "/suites/workspace-overview", content: ".capture-card" },
    {
      path: "/suites/workspace-overview/images/overview__desktop.png",
      content: ".comparison-viewer",
    },
    {
      path: "/suites/workspace-overview/images/overview__desktop.png?view=log",
      content: "[data-log-state]",
    },
    { path: "/suites/not-a-suite", content: ".state-panel" },
    { path: "/suites/workspace-overview/not-found", content: ".state-panel" },
  ]) {
    await page.goto(path);
    await expect(page.locator(content).first()).toBeVisible();
    await page.evaluate(() => document.fonts.ready);
    await expect(page.locator(":root")).toHaveCSS("scrollbar-gutter", "stable");
    await expect(page.locator(":root")).not.toHaveCSS("scrollbar-color", "auto");
    const review = page.locator(".review-scroll");
    if (await review.count()) {
      await expect(review).toHaveCSS("scrollbar-gutter", "auto");
      await expect(review).toHaveCSS(
        "overflow-y",
        content === ".comparison-viewer" ? "hidden" : "visible",
      );
    }
    const scroller = page.locator(":root");
    const main = page.locator("#main");
    const initial = await main.boundingBox();
    if (!initial) throw new Error(`Page content is not visible: ${path}`);
    for (const overflow of ["hidden", "scroll", "auto"]) {
      await scroller.evaluate((element, value) => {
        (element as HTMLElement).style.overflowY = value;
      }, overflow);
      const bounds = await main.boundingBox();
      if (!bounds) throw new Error(`Page content is not visible: ${path}`);
      expect(Math.abs(bounds.x - initial.x), path).toBeLessThanOrEqual(1);
      expect(Math.abs(bounds.width - initial.width), path).toBeLessThanOrEqual(1);
      await assertLayout(page);
    }
  }
});

test.describe("page scrollbar tracks", () => {
  test("empty gutters remain distinct from short pages in both themes", async ({
    playwright,
    isMobile,
    baseURL,
  }, info) => {
    test.skip(isMobile, "Touch browsers use overlay scrollbars without reserved gutters.");
    const browser = await playwright.chromium.launch({
      executablePath: browserExecutable(),
      ignoreDefaultArgs: ["--hide-scrollbars"],
      args: ["--disable-features=OverlayScrollbar,FluentOverlayScrollbar"],
    });
    try {
      const page = await browser.newPage({ baseURL });
      await page.emulateMedia({ reducedMotion: "reduce" });
      await page.setViewportSize({ width: 1280, height: 1000 });
      for (const path of ["/", "/suites/workspace-overview"]) {
        await page.goto(path);
        await expect(
          page.locator(path === "/" ? ".results-table" : ".capture-card").first(),
        ).toBeVisible();
        await page.setViewportSize({ width: 1280, height: 480 });
        const review = page.locator(".review-scroll");
        if (await review.count()) {
          expect(
            await review.evaluate((element) =>
              Math.abs(element.getBoundingClientRect().width - element.clientWidth),
            ),
          ).toBeLessThanOrEqual(1);
        }
        await page.mouse.move(800, 350);
        await page.mouse.wheel(0, 200);
        await expect.poll(() => page.evaluate(() => window.scrollY)).toBeGreaterThan(0);
        expect((await page.locator(".app-header").boundingBox())?.y).toBe(0);
        await page.screenshot({
          path: resolve(
            import.meta.dirname,
            `../.fixtures/screenshots/desktop/root-scroll-${path === "/" ? "overview" : "suite"}.png`,
          ),
          scale: "css",
          animations: "disabled",
        });
        await page.evaluate(() => window.scrollTo(0, 0));
        await page.setViewportSize({ width: 1280, height: 1000 });
        await page.locator("#main").getByRole("searchbox").fill("scrollbar-empty-results");
        await expect(page.locator("#main .state-panel")).toBeVisible();
        await page.evaluate(() => document.fonts.ready);
        for (const theme of ["Light", "Dark"] as const) {
          await chooseTheme(page, theme);
          const gutters = await page.locator(":root").evaluateAll((elements) =>
            elements.map((element) => {
              const bounds = element.getBoundingClientRect();
              const root = element === document.documentElement;
              const width = root ? innerWidth - bounds.width : bounds.width - element.clientWidth;
              return {
                width,
                x: (root ? innerWidth : bounds.right) - width / 2,
                y: bounds.top + 100,
                overflowing: element.scrollHeight > element.clientHeight,
                trackColor: getComputedStyle(document.documentElement).backgroundColor,
                pageColor: getComputedStyle(document.body).backgroundColor,
              };
            }),
          );
          const image = PNG.sync.read(
            await page.screenshot({ scale: "css", animations: "disabled" }),
          );
          for (const gutter of gutters) {
            expect(gutter.width, `${path} reserved gutter width`).toBeGreaterThan(0);
            expect(gutter.overflowing, `${path} should not need a scrollbar`).toBe(false);
            expect(gutter.trackColor).not.toBe(gutter.pageColor);
            const offset = (Math.floor(gutter.y) * image.width + Math.floor(gutter.x)) * 4;
            const color = [...image.data.subarray(offset, offset + 3)].join(", ");
            expect(`rgb(${color})`, `${path} empty gutter in ${theme}`).toBe(gutter.trackColor);
          }
          await screenshot(
            page,
            info.project.name,
            `scrollbar-gutter-${path === "/" ? "overview" : "suite"}-${theme.toLowerCase()}`,
          );
        }
      }
    } finally {
      await browser.close();
    }
  });
});

test("footer stays at the bottom on short and empty pages", async ({ page }, info) => {
  const viewport = page.viewportSize();
  if (!viewport) throw new Error("The viewport is not configured");
  await page.setViewportSize({ ...viewport, height: 1200 });
  for (const { path, name, content } of [
    { path: "/", name: "overview", content: ".results-table tbody tr" },
    { path: "/suites/workspace-overview", name: "suite", content: ".capture-card" },
    { path: "/suites/workspace-overview/not-found", name: "not-found", content: null },
    { path: "/suites/not-a-suite", name: "suite-not-found", content: null },
  ]) {
    await page.goto(path);
    if (content) {
      await expect(page.locator(content).first()).toBeVisible();
      await page.locator("#main").getByRole("searchbox").fill("footer-empty-results");
      await expect(page.locator(content)).toHaveCount(0);
    } else {
      await expect(page.locator("#main .state-panel")).toBeVisible();
    }
    await page.evaluate(() => document.fonts.ready);
    const footer = page.getByRole("contentinfo");
    await expect(footer).toBeVisible();
    await expect
      .poll(() =>
        footer.evaluate((element) =>
          Math.abs(element.getBoundingClientRect().bottom - window.innerHeight),
        ),
      )
      .toBeLessThanOrEqual(1);
    const contentBottom = await page
      .locator("#main")
      .evaluate((element) => element.getBoundingClientRect().bottom);
    expect(
      await footer.evaluate((element) => element.getBoundingClientRect().top),
    ).toBeGreaterThanOrEqual(contentBottom - 1);
    await screenshot(page, info.project.name, `footer-${name}`);
  }
});

test("footer follows long overview and suite content in document scrolling", async ({ page }) => {
  const viewport = page.viewportSize();
  if (!viewport) throw new Error("The viewport is not configured");
  await page.setViewportSize({ ...viewport, height: 480 });
  for (const { path, content } of [
    { path: "/", content: ".results-table tbody tr" },
    { path: "/suites/workspace-overview", content: ".capture-card" },
  ]) {
    await page.goto(path);
    await expect(page.locator(content).first()).toBeVisible();
    await page.evaluate(() => document.fonts.ready);
    const footer = page.getByRole("contentinfo");
    await expect
      .poll(() => footer.evaluate((element) => element.getBoundingClientRect().top))
      .toBeGreaterThanOrEqual(480);
    await footer.scrollIntoViewIfNeeded();
    await expect
      .poll(() => footer.evaluate((element) => element.getBoundingClientRect().bottom))
      .toBeLessThanOrEqual(481);
    const contentBottom = await page
      .locator("#main")
      .evaluate((element) => element.getBoundingClientRect().bottom);
    expect(
      await footer.evaluate((element) => element.getBoundingClientRect().top),
    ).toBeGreaterThanOrEqual(contentBottom - 1);
    expect(await page.evaluate(() => window.scrollY)).toBeGreaterThan(0);
  }
});

test("logo and favicon use the shared desktop artwork", async ({ page }) => {
  await page.goto("/");
  const logo = page.locator(".brand img");
  await expect(logo).toBeVisible();
  const source = await logo.getAttribute("src");
  if (!source) throw new Error("The application logo is missing");
  const artwork = readFileSync(resolve(import.meta.dirname, "../../assets/icon.svg"), "utf8");
  expect(new URL(source, page.url()).searchParams.get("v")).toBe(getAssetVersion(artwork));
  await expect(page.locator('head link[rel="icon"]')).toHaveAttribute("href", source);
  const response = await page.request.get(source);
  expect(response.ok()).toBe(true);
  expect(await response.text()).toBe(artwork);
  await expect
    .poll(() => logo.evaluate((element: HTMLImageElement) => element.naturalWidth))
    .toBeGreaterThan(0);
});

test("page titles follow suite navigation, deep links, and the overview", async ({
  page,
  isMobile,
}) => {
  await page.goto("/");
  await expect(page.locator(".results-table tbody tr")).toHaveCount(7);
  const reportTitle = await page.title();
  await page.locator(".suite-name").filter({ hasText: "workspace-overview" }).click();
  await expect(page).toHaveTitle(`workspace-overview | ${reportTitle}`);
  await page.locator(".capture-card").first().click();
  await expect(page).toHaveTitle(`workspace-overview | ${reportTitle}`);
  if (isMobile) await page.getByRole("button", { name: "Toggle suite index" }).click();
  await page.getByRole("button", { name: /^Test suite:/ }).click();
  await page
    .getByRole("navigation", { name: "Test suites", exact: true })
    .getByRole("link", { name: /orders-and-checkout/ })
    .click();
  await expect(page).toHaveTitle(`orders-and-checkout | ${reportTitle}`);
  await page.reload();
  await expect(page).toHaveTitle(`orders-and-checkout | ${reportTitle}`);
  await page.getByRole("link", { name: "Regressify overview" }).click();
  await expect(page).toHaveTitle(reportTitle);
});

test("suite display names label navigation without changing routes", async ({
  page,
  isMobile,
}, info) => {
  const displayName = "Customer checkout / desktop <review>";
  await page.route("**/_snapshot/data/manifest.json?*", async (route) => {
    const response = await route.fetch();
    const manifest = await response.json();
    for (const suite of manifest.suites) {
      if (suite.id === "orders-and-checkout") suite.displayName = displayName;
    }
    await route.fulfill({ response, json: manifest });
  });
  await page.goto("/");
  await expect(page.locator(".results-table tbody tr")).toHaveCount(7);
  const reportTitle = await page.title();
  await page.getByRole("searchbox", { name: "Search suites" }).fill("Customer checkout");
  await expect(page.locator(".suite-name")).toHaveCount(1);
  await expect(page.locator(".suite-name")).toHaveText(displayName);
  await page.locator(".suite-name").click();
  await expect(page).toHaveURL(/\/suites\/orders-and-checkout$/);
  await expect(page.getByRole("heading", { name: displayName, exact: true })).toBeVisible();
  await expect(page).toHaveTitle(`${displayName} | ${reportTitle}`);
  await screenshot(page, info.project.name, "suite-display-name");
  await page.locator(".capture-card").first().click();
  await expect(page).toHaveURL(/\/suites\/orders-and-checkout\/images\//);
  await page.getByRole("button", { name: "Capture information", exact: true }).click();
  await expect(
    page
      .getByRole("dialog", { name: "Capture information" })
      .locator("dd[title='orders-and-checkout']"),
  ).toHaveText(displayName);
  await page.getByRole("button", { name: "Close capture information" }).click();
  await expect(
    page.getByRole("navigation", { name: "Breadcrumb" }).getByRole("link", { name: displayName }),
  ).toBeVisible();
  await expect(page).toHaveTitle(`${displayName} | ${reportTitle}`);
  await page.reload();
  await expect(page).toHaveTitle(`${displayName} | ${reportTitle}`);
  if (isMobile) await page.getByRole("button", { name: "Toggle suite index" }).click();
  const selector = page.getByRole("button", { name: `Test suite: ${displayName}` });
  await expect(selector).toContainText(displayName);
  await selector.click();
  await page.getByRole("searchbox", { name: "Find a suite" }).fill("Customer checkout");
  const navigation = page.getByRole("navigation", { name: "Test suites", exact: true });
  await expect(navigation.getByRole("link")).toHaveCount(1);
  await expect(navigation.getByRole("link")).toContainText(displayName);
  await expect(navigation.getByRole("link")).toHaveAttribute("href", "/suites/orders-and-checkout");
  await expect(page.locator("review")).toHaveCount(0);
});

test("overview centers metrics and result columns with aligned suite labels", async ({
  page,
}, info) => {
  await page.goto("/");
  await expect(page.locator(".results-table tbody tr")).toHaveCount(7);
  await page.evaluate(() => document.fonts.ready);
  expect
    .soft(
      await page
        .locator(".metrics strong")
        .evaluateAll((elements) => elements.map((element) => getComputedStyle(element).fontWeight)),
    )
    .toEqual(["700", "700", "700", "700"]);
  const offsets = await page.evaluate(() => {
    const center = (bounds: DOMRect) => bounds.left + bounds.width / 2;
    const stacked = window.matchMedia("(max-width: 760px)").matches;
    const metrics = Array.from(document.querySelectorAll(".metrics > div")).flatMap((column) =>
      Array.from(column.children).map((content) =>
        Math.abs(center(content.getBoundingClientRect()) - center(column.getBoundingClientRect())),
      ),
    );
    const cells = Array.from(
      document.querySelectorAll(stacked ? ".results-table tbody tr" : ".results-table tr"),
    ).flatMap((row) =>
      Array.from(row.children)
        .slice(1, 4)
        .filter((cell) => cell.getBoundingClientRect().width > 0)
        .map((cell) => {
          const content = cell.querySelector("button, .result-count");
          const range = document.createRange();
          range.selectNodeContents(cell);
          const bounds = content?.getBoundingClientRect() || range.getBoundingClientRect();
          if (stacked) {
            const style = getComputedStyle(cell);
            const columnWidth = parseFloat(style.gridTemplateColumns.split(" ").at(-1) || "0");
            return Math.abs(
              center(bounds) -
                cell.getBoundingClientRect().right +
                parseFloat(style.paddingRight) +
                columnWidth / 2,
            );
          }
          return Math.abs(center(bounds) - center(cell.getBoundingClientRect()));
        }),
    );
    const heading = document.querySelector(
      stacked ? ".suite-label" : ".results-table th:first-child button",
    );
    const names = Array.from(document.querySelectorAll(".suite-label")).map((name) => {
      if (!heading) throw new Error("Suite label is missing");
      return Math.abs(name.getBoundingClientRect().left - heading.getBoundingClientRect().left);
    });
    return { metrics, cells, names };
  });
  for (const [group, values] of Object.entries(offsets)) {
    expect(values.length).toBeGreaterThan(0);
    for (const offset of values) expect(offset, `${group} alignment offset`).toBeLessThan(1);
  }
  await screenshot(page, info.project.name, "overview-alignment");
});

test("navigation stays in the SPA, preserves filters, and refreshes deep links", async ({
  page,
  isMobile,
}, info) => {
  await page.goto("/");
  let navigations = 0;
  page.on("request", (request) => {
    if (request.resourceType() === "document") navigations++;
  });
  await page.locator(".suite-name").filter({ hasText: "workspace-overview" }).click();
  await expect(page).toHaveURL(/\/suites\/workspace-overview$/);
  await expect(page.locator(".capture-card")).toHaveCount(4);
  await page.getByRole("searchbox", { name: "Search captures" }).fill("Workspace");
  await expect(page.locator(".capture-card")).toHaveCount(1);
  await page.locator(".capture-card").click();
  await expect(page).toHaveURL(/\/images\/overview__desktop.png\?q=Workspace$/);
  await expect(page.getByRole("region", { name: "Zoomable comparison images" })).toBeVisible();
  expect(navigations).toBe(0);
  await page.locator(".back-link").click();
  await expect(page.getByRole("searchbox", { name: "Search captures" })).toHaveValue("Workspace");
  await page.getByRole("searchbox", { name: "Search captures" }).fill("");
  await screenshot(page, info.project.name, "suite-grid");
  await page.getByRole("button", { name: "List view" }).click();
  await expect(page.locator(".capture-list")).toBeVisible();
  if (isMobile) await page.getByRole("button", { name: "Toggle suite index" }).click();
  await page.getByRole("button", { name: /^Test suite:/ }).click();
  await page
    .getByRole("navigation", { name: "Test suites", exact: true })
    .getByRole("link", { name: /orders-and-checkout/ })
    .click();
  await expect(
    page.getByRole("heading", { name: "orders-and-checkout", exact: true }),
  ).toBeVisible();
  await page.reload();
  await expect(page.locator(".capture-card")).toHaveCount(4);
  if (!isMobile) {
    await page.getByRole("button", { name: "Toggle suite index" }).click();
    await expect(page.locator(".suite-panel")).toHaveCount(0);
  }
});

test("large suites keep preview images bounded while scrolling and jumping to failures", async ({
  page,
}) => {
  await page.emulateMedia({ reducedMotion: "reduce" });
  await page.route("**/_snapshot/data/manifest.json?*", async (route) => {
    const response = await route.fetch();
    const manifest = await response.json();
    const summary = manifest.suites.find(
      (suite: { id: string }) => suite.id === "workspace-overview",
    );
    Object.assign(summary, { total: 300, passed: 299, failed: 1, errors: 0 });
    await route.fulfill({ response, json: manifest });
  });
  await page.route("**/_snapshot/data/suites/**", async (route) => {
    const response = await route.fetch();
    const suite = await response.json();
    suite.tests = Array.from({ length: 300 }, (_, index) => ({
      ...suite.tests[0],
      id: `capture-${index}.png`,
      label: `Capture ${index}`,
      scenario: `Scenario ${index}`,
      status: index === 250 ? "fail" : "pass",
      mismatch: index === 250 ? 1 : 0,
    }));
    await route.fulfill({ response, json: suite });
  });
  await page.goto("/suites/workspace-overview?view=list");
  const cards = page.locator(".capture-card");
  const images = cards.locator("img[src]");
  await expect(cards).toHaveCount(300);
  await expect.poll(() => images.count()).toBeGreaterThan(0);
  await expect.poll(() => images.count()).toBeLessThan(45);
  const first = cards.first();
  const last = cards.last();
  await last.scrollIntoViewIfNeeded();
  await expect.poll(() => last.locator("img[src]").count()).toBe(3);
  await expect.poll(() => first.locator("img[src]").count()).toBe(0);
  await expect.poll(() => images.count()).toBeLessThan(45);
  await page.evaluate(() => window.scrollTo(0, 0));
  await expect.poll(() => first.locator("img[src]").count()).toBe(3);
  await expect.poll(() => last.locator("img[src]").count()).toBe(0);
  await page.getByRole("button", { name: "Next failed comparison", exact: true }).click();
  const selected = page.locator('.capture-card[data-active-failure="true"]');
  await expect(selected).toContainText("Capture 250");
  await expect(selected).toBeFocused();
  await expect.poll(() => selected.locator("img[src]").count()).toBe(3);
  await expect.poll(() => images.count()).toBeLessThan(45);
  await page.getByRole("button", { name: "Grid view" }).click();
  await expect(page.getByRole("button", { name: "Grid view" })).toHaveAttribute(
    "aria-pressed",
    "true",
  );
  await expect(page.locator(".capture-grid")).toBeVisible();
  await expect.poll(() => images.count()).toBeGreaterThan(0);
  await expect.poll(() => images.count()).toBeLessThan(45);
  await page.getByRole("searchbox", { name: "Search captures" }).fill("Capture 299");
  await expect(cards).toHaveCount(1);
  await expect(cards).toContainText("Capture 299");
  await cards.click();
  await expect(page).toHaveURL(/\/images\/capture-299\.png\?q=Capture\+299$/);
  await expect
    .poll(() =>
      page
        .locator(".image-world img")
        .evaluateAll(
          (elements) =>
            elements.length > 0 &&
            elements.every((element) => (element as HTMLImageElement).naturalWidth > 0),
        ),
    )
    .toBe(true);
});

test("suite previews avoid full-resolution downloads until opening image review", async ({
  page,
  isMobile,
}) => {
  let originals: string[] = [];
  const requested = new Set<string>();
  page.on("request", (request) => requested.add(request.url()));
  await page.route("**/_snapshot/data/suites/**", async (route) => {
    const response = await route.fetch();
    const suite = await response.json();
    originals = [suite.tests[0].reference, suite.tests[0].test, suite.tests[0].diff]
      .filter(Boolean)
      .map((path) => new URL(`/${path}`, route.request().url()).href);
    await route.fulfill({ response, json: suite });
  });
  await page.goto("/suites/workspace-overview?view=list");
  const capture = page.locator(".capture-card").first();
  const images = capture.locator("img");
  await expect(images).toHaveCount(3);
  for (const image of await images.all()) {
    await expect(image).toHaveAttribute("width", "640");
    await expect(image).toHaveAttribute("height", "480");
  }
  await expect
    .poll(() =>
      images.evaluateAll((elements) =>
        elements.every((element) => (element as HTMLImageElement).naturalWidth > 0),
      ),
    )
    .toBe(true);
  expect(
    await images.evaluateAll((elements) =>
      elements.every(
        (element) =>
          (element as HTMLImageElement).naturalWidth <= 640 &&
          (element as HTMLImageElement).naturalHeight <= 480,
      ),
    ),
  ).toBe(true);
  expect(originals.length).toBe(3);
  expect(originals.some((url) => requested.has(url))).toBe(false);
  await page.getByRole("button", { name: "Grid view" }).click();
  await expect(images).toHaveCount(1);
  await expect(images).toHaveAttribute("width", "640");
  await expect(images).toHaveAttribute("height", "400");
  await expect(images).toHaveAttribute("src", /diff-thumbnail/);
  await expect
    .poll(() => images.evaluate((element: HTMLImageElement) => element.naturalWidth))
    .toBeGreaterThan(0);
  expect(
    await images.evaluate(
      (element: HTMLImageElement) => element.naturalWidth <= 640 && element.naturalHeight <= 400,
    ),
  ).toBe(true);
  expect(originals.some((url) => requested.has(url))).toBe(false);
  await capture.click();
  if (isMobile) await page.getByRole("combobox", { name: "Comparison mode" }).selectOption("side");
  await expect
    .poll(() =>
      page
        .locator(".image-world img")
        .evaluateAll(
          (elements) =>
            elements.length === 3 &&
            elements.every((element) => (element as HTMLImageElement).naturalWidth > 0),
        ),
    )
    .toBe(true);
  expect(originals.every((url) => requested.has(url))).toBe(true);
  for (const [index, role] of ["reference", "test", "diff"].entries()) {
    const original = new URL(originals[index]);
    await expect(page.getByRole("link", { name: `Download ${role}`, exact: true })).toHaveAttribute(
      "href",
      original.pathname + original.search,
    );
  }
});

test("short suite grid thumbnails span full width at the top of the thumbnail area", async ({
  page,
}) => {
  const shortImage = new PNG({ width: 640, height: 120 });
  for (let i = 0; i < shortImage.data.length; i += 4) {
    shortImage.data[i] = 20;
    shortImage.data[i + 1] = 140;
    shortImage.data[i + 2] = 220;
    shortImage.data[i + 3] = 255;
  }
  const shortPng = PNG.sync.write(shortImage);
  let gridPath = "";
  await page.route("**/suites/workspace-overview.json*", async (route) => {
    const response = await route.fetch();
    const suite = await response.json();
    gridPath = suite.tests[0].previews?.grid || suite.tests[0].test;
    await route.fulfill({ response, json: suite });
  });
  await page.route("**/_snapshot/images/**", async (route) => {
    if (gridPath && route.request().url().endsWith(`/${gridPath}`)) {
      await route.fulfill({ status: 200, contentType: "image/png", body: shortPng });
    } else {
      await route.continue();
    }
  });
  await page.goto("/suites/workspace-overview");
  const container = page.locator(".capture-thumbnail").first();
  const image = container.locator("img");
  await expect(image).toBeVisible();
  await expect(image).toHaveCSS("object-fit", "contain");
  await expect(image).toHaveCSS("object-position", "50% 0%");
  await expect
    .poll(() => image.evaluate((element: HTMLImageElement) => element.naturalHeight))
    .toBe(120);
  const shot = PNG.sync.read(await container.screenshot({ scale: "css", animations: "disabled" }));
  const pixelAt = (x: number, y: number) => {
    const idx = (y * shot.width + x) * 4;
    return [shot.data[idx], shot.data[idx + 1], shot.data[idx + 2]] as const;
  };
  for (const x of [10, Math.floor(shot.width / 2), shot.width - 10]) {
    const [r, g, b] = pixelAt(x, 6);
    expect(r).toBeLessThan(50);
    expect(g).toBeGreaterThan(100);
    expect(b).toBeGreaterThan(180);
  }
  const [midR, midG, midB] = pixelAt(Math.floor(shot.width / 2), Math.floor(shot.height * 0.6));
  const [botR, botG, botB] = pixelAt(Math.floor(shot.width / 2), shot.height - 10);
  expect([midR, midG, midB]).toEqual([botR, botG, botB]);
  expect(botB).not.toBe(220);

  await page.goto("/suites/product-catalog");
  const errorCard = page.locator(".capture-card").nth(3);
  await errorCard.scrollIntoViewIfNeeded();
  const errorThumb = errorCard.locator(".capture-thumbnail img");
  await expect(errorThumb).toBeVisible();
  await expect(errorThumb).toHaveCSS("object-fit", "contain");
  await expect
    .poll(() =>
      errorThumb.evaluate((element: HTMLImageElement) => [
        element.naturalWidth,
        element.naturalHeight,
      ]),
    )
    .toEqual([400, 400]);
});

test("legacy suite reports without previews remain readable", async ({ page }) => {
  let original = "";
  await page.route("**/_snapshot/data/suites/**", async (route) => {
    const response = await route.fetch();
    const suite = await response.json();
    for (const capture of suite.tests) {
      delete capture.previews;
      delete capture.imageSizes;
    }
    original = `/${suite.tests[0].test}`;
    await route.fulfill({ response, json: suite });
  });
  await page.goto("/suites/workspace-overview");
  const container = page.locator(".capture-thumbnail").first();
  const image = container.locator("img");
  await expect(image).toBeVisible();
  await expect(image).toHaveAttribute("src", original);
  await expect
    .poll(() => image.evaluate((element: HTMLImageElement) => element.naturalWidth))
    .toBeGreaterThan(0);
  const geometry = await container.evaluate((box) => {
    const img = box.querySelector("img") as HTMLImageElement;
    const boxRect = box.getBoundingClientRect();
    const imgRect = img.getBoundingClientRect();
    return {
      topDelta: Math.abs(imgRect.top - boxRect.top),
      widthDelta: Math.abs(imgRect.width - boxRect.width),
      imgHeight: imgRect.height,
      boxHeight: boxRect.height,
    };
  });
  expect(geometry.topDelta).toBeLessThan(1);
  expect(geometry.widthDelta).toBeLessThan(1);
  expect(geometry.imgHeight).toBeGreaterThan(geometry.boxHeight);
  await page.locator(".capture-card").first().click();
  await expect
    .poll(() =>
      page.locator(".image-world img").evaluateAll(
        (images) =>
          images.length > 0 &&
          images.every((element) => {
            const image = element as HTMLImageElement;
            return (
              image.naturalWidth > 0 &&
              Number(image.getAttribute("width")) === image.naturalWidth &&
              Number(image.getAttribute("height")) === image.naturalHeight
            );
          }),
      ),
    )
    .toBe(true);
});

test("suite list compares reference, test, and available diff captures side by side", async ({
  page,
}, info) => {
  await page.goto("/suites/workspace-overview?view=list");
  const capture = page.locator(".capture-list .capture-card").first();
  const lanes = capture.locator(".capture-lane");
  await expect(lanes).toHaveCount(3);
  await expect(lanes.locator("figcaption")).toHaveText(["Reference", "Test", "Diff"]);
  for (const image of await lanes.getByRole("img").all()) {
    await expect(image).toBeVisible();
    await expect(image).toHaveCSS("object-fit", "contain");
    await expect
      .poll(() => image.evaluate((element: HTMLImageElement) => element.naturalWidth))
      .toBeGreaterThan(0);
  }
  const bounds = await lanes.evaluateAll((elements) =>
    elements.map((element) => {
      const { top, left, right, width } = element.getBoundingClientRect();
      return { top, left, right, width };
    }),
  );
  for (const [index, lane] of bounds.entries()) {
    expect(lane.width).toBeGreaterThan(50);
    expect(Math.abs(lane.top - bounds[0].top)).toBeLessThan(1);
    expect(Math.abs(lane.width - bounds[0].width)).toBeLessThan(1);
    if (index > 0) expect(lane.left).toBeGreaterThanOrEqual(bounds[index - 1].right);
  }
  await screenshot(page, info.project.name, "suite-list");
  expect((await new AxeBuilder({ page }).analyze()).violations).toEqual([]);
  await capture.click();
  await expect(page).toHaveURL(/\/images\/overview__desktop.png\?view=list$/);
  await page.locator(".back-link").click();
  await expect(page.getByRole("button", { name: "List view" })).toHaveAttribute(
    "aria-pressed",
    "true",
  );
  await capture.click();
  await page.getByRole("button", { name: "Comparison", exact: true }).click();
  await expect(page).toHaveURL(/\/images\/overview__desktop.png\?view=list$/);
  await page.getByRole("button", { name: "Capture Log", exact: true }).click();
  await page.reload();
  await expect(page.getByRole("button", { name: "Capture Log", exact: true })).toHaveAttribute(
    "aria-pressed",
    "true",
  );
  await expect(page.locator(".back-link")).toHaveAttribute(
    "href",
    "/suites/workspace-overview?view=list",
  );
  await page.getByRole("button", { name: "Comparison", exact: true }).click();
  await expect(page).toHaveURL(/\/images\/overview__desktop.png\?view=list$/);
  await page.getByRole("button", { name: "Capture Log", exact: true }).click();
  await page.locator(".back-link").click();
  await expect(page.getByRole("button", { name: "List view" })).toHaveAttribute(
    "aria-pressed",
    "true",
  );
  await page.goto("/suites/customer-accounts?view=list");
  await expect(lanes).toHaveCount(2);
  await expect(lanes.locator("figcaption")).toHaveText(["Reference", "Test"]);
  await page.getByRole("button", { name: "Grid view" }).click();
  await expect(page.locator(".capture-grid .capture-card").first().getByRole("img")).toHaveCount(1);
});

test("suite list preserves missing-image lanes and wraps long capture labels", async ({ page }) => {
  const label = "LongCaptureLabel".repeat(20);
  let unavailableImage = "";
  await page.route("**/_snapshot/data/suites/**", async (route) => {
    const response = await route.fetch();
    const data = await response.json();
    unavailableImage = new URL(
      `/${data.tests[0].previews?.test || data.tests[0].test}`,
      route.request().url(),
    ).href;
    data.tests[0].reference = null;
    data.tests[0].label = label;
    await route.fulfill({ response, json: data });
  });
  await page.route("**/_snapshot/images/**", (route) =>
    route.request().url() === unavailableImage ? route.fulfill({ status: 404 }) : route.continue(),
  );
  await page.goto("/suites/workspace-overview?view=list");
  const capture = page.locator(".capture-list .capture-card").first();
  await expect(capture.locator(".capture-lane")).toHaveCount(3);
  await capture.locator(".capture-comparison").scrollIntoViewIfNeeded();
  await expect(capture.getByText("No image", { exact: true })).toBeVisible();
  await expect(capture.getByText("Image unavailable", { exact: true })).toBeVisible();
  await expect(capture.getByRole("img", { name: `${label} diff`, exact: true })).toBeVisible();
  for (const width of [320, 768, 1920]) {
    await page.setViewportSize({ width, height: 900 });
    await assertLayout(page);
    const overflows = await capture.evaluate((element) =>
      [element, ...element.querySelectorAll("h2, .capture-lane-preview")].some(
        (node) => node.scrollWidth > node.clientWidth,
      ),
    );
    expect(overflows).toBe(false);
  }
});

test("review navigation shows the current suite in a dropdown without shifting scenarios", async ({
  page,
  isMobile,
}, info) => {
  await page.goto("/suites/workspace-overview");
  if (isMobile) await page.getByRole("button", { name: "Toggle suite index" }).click();
  const suites = page.getByRole("navigation", { name: "Test suites", exact: true });
  const suiteSelector = page.getByRole("button", { name: "Test suite: workspace-overview" });
  await expect(suiteSelector).toBeVisible();
  await expect(suiteSelector).toContainText("workspace-overview");
  await expect(suiteSelector).toHaveAttribute("aria-expanded", "false");
  await expect(suites).toBeHidden();
  const scenarios = page.getByRole("navigation", { name: "Test scenarios", exact: true });
  await expect(scenarios).toBeVisible();
  await expect(scenarios.getByRole("link")).toHaveCount(4);
  const scenarioTop = await scenarios.evaluate((element) => element.getBoundingClientRect().top);
  await screenshot(page, info.project.name, "navigation-selector");
  await suiteSelector.click();
  await expect(suiteSelector).toHaveAttribute("aria-expanded", "true");
  await expect(page.getByRole("dialog", { name: "Test suites", exact: true })).toBeVisible();
  await expect(suites).toBeVisible();
  await expect(suites.getByRole("link", { name: /workspace-overview/ })).toHaveAttribute(
    "aria-current",
    "page",
  );
  await expect(page.getByRole("searchbox", { name: "Find a suite" })).toBeFocused();
  expect(await scenarios.evaluate((element) => element.getBoundingClientRect().top)).toBe(
    scenarioTop,
  );
  await screenshot(page, info.project.name, "navigation-dropdown", false);
  await expect(suites).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(suites).toBeHidden();
  await expect(suiteSelector).toBeFocused();
  await expect(scenarios).toBeVisible();
  await scenarios.getByRole("link", { name: /Scenario 2/ }).click();
  await expect(page).toHaveURL(/\/images\/recent-orders__mobile.png$/);
  await expect(
    page.getByRole("heading", { name: "Recent orders / Mobile", exact: true }),
  ).toBeVisible();
  await expect(page.getByRole("region", { name: "Zoomable comparison images" })).toBeVisible();
  if (isMobile) await page.getByRole("button", { name: "Toggle suite index" }).click();
  await expect(suites).toBeHidden();
  await expect(scenarios.getByRole("link", { name: /Scenario 2/ })).toHaveAttribute(
    "aria-current",
    "page",
  );
  await page.getByRole("searchbox", { name: "Find a scenario" }).fill("Scenario 3");
  await expect(scenarios.getByRole("link")).toHaveCount(1);
  const scenario = scenarios.getByRole("link");
  await expect(scenario).toHaveAttribute("href", /^\/suites\/workspace-overview\/images\//);
  const comparisonUrl = new URL((await scenario.getAttribute("href")) || "", page.url()).href;
  await scenario.click();
  await expect(page).toHaveURL(comparisonUrl);
  await expect(page.getByRole("region", { name: "Zoomable comparison images" })).toBeVisible();
  await page.reload();
  await expect(page).toHaveURL(comparisonUrl);
  await expect(page.getByRole("region", { name: "Zoomable comparison images" })).toBeVisible();
  if (isMobile) await page.getByRole("button", { name: "Toggle suite index" }).click();
  await expect(suiteSelector).toBeVisible();
  await expect(suites).toBeHidden();
  await expect(scenarios).toBeVisible();
});

test("review navigation dropdown searches, dismisses, and switches suites with the keyboard", async ({
  page,
  isMobile,
}) => {
  await page.goto("/suites/workspace-overview");
  if (isMobile) await page.getByRole("button", { name: "Toggle suite index" }).click();
  const selector = page.getByRole("button", { name: /^Test suite:/ });
  const suites = page.getByRole("navigation", { name: "Test suites", exact: true });
  const search = page.getByRole("searchbox", { name: "Find a suite" });
  await selector.focus();
  await selector.press("Enter");
  await expect(search).toBeFocused();
  await search.fill("no-such-suite");
  await expect(suites.getByRole("link")).toHaveCount(0);
  await expect(suites.getByRole("status")).toHaveText("No matching suites");
  await search.press("ArrowDown");
  await expect(search).toBeFocused();
  await search.fill("orders-and-checkout");
  await expect(suites.getByRole("link")).toHaveCount(1);
  await search.press("ArrowDown");
  const destination = suites.getByRole("link", { name: /orders-and-checkout/ });
  await expect(destination).toBeFocused();
  await destination.press("Enter");
  await expect(page).toHaveURL(/\/suites\/orders-and-checkout$/);
  await expect(suites).toBeHidden();
  if (isMobile) {
    await expect(page.getByRole("dialog", { name: "Suite index", exact: true })).toBeHidden();
    await page.getByRole("button", { name: "Toggle suite index" }).click();
  }
  await expect(selector).toHaveAccessibleName("Test suite: orders-and-checkout");
  await selector.click();
  await expect(search).toHaveValue("");
  await expect(suites.getByRole("link")).toHaveCount(7);
  await expect(suites.getByRole("link", { name: /orders-and-checkout/ })).toHaveAttribute(
    "aria-current",
    "page",
  );
  expect((await new AxeBuilder({ page }).analyze()).violations).toEqual([]);
  await search.fill("workspace");
  await page.getByRole("heading", { name: "Test suite", exact: true }).click();
  await expect(suites).toBeHidden();
  await selector.click();
  await expect(search).toHaveValue("");
  await expect(suites.getByRole("link")).toHaveCount(7);
  await selector.click();
  await expect(suites).toBeHidden();
});

test("review navigation dropdown contains long suite names and scrolls independently", async ({
  page,
  isMobile,
}, info) => {
  const displayName = "LongSuiteName".repeat(8);
  await page.route("**/_snapshot/data/manifest.json?*", async (route) => {
    const response = await route.fetch();
    const manifest = await response.json();
    const current = manifest.suites.find(
      (suite: { id: string }) => suite.id === "workspace-overview",
    );
    manifest.suites = Array.from({ length: 30 }, (_, index) => ({
      ...current,
      id: index === 0 ? current.id : `suite-${index}`,
      displayName: index === 0 ? displayName : `Suite ${index}`,
    }));
    await route.fulfill({ response, json: manifest });
  });
  await page.setViewportSize({ width: isMobile ? 320 : 1280, height: 720 });
  await page.goto("/suites/workspace-overview");
  await chooseTheme(page, "Dark");
  if (isMobile) await page.getByRole("button", { name: "Toggle suite index" }).click();
  const selector = page.getByRole("button", { name: `Test suite: ${displayName}` });
  await expect(selector).toContainText(displayName);
  expect(await selector.evaluate((element) => element.scrollWidth <= element.clientWidth)).toBe(
    true,
  );
  await selector.click();
  const dropdown = page.getByRole("dialog", { name: "Test suites", exact: true });
  const suites = dropdown.getByRole("navigation", { name: "Test suites", exact: true });
  await expect(suites.getByRole("link")).toHaveCount(30);
  await expect(suites.locator('[aria-current="page"] .suite-dropdown-check')).toBeVisible();
  const bounds = await dropdown.boundingBox();
  if (!bounds) throw new Error("The suite dropdown is unavailable");
  expect(bounds.x).toBeGreaterThanOrEqual(0);
  expect(bounds.y).toBeGreaterThanOrEqual(0);
  expect(bounds.x + bounds.width).toBeLessThanOrEqual(isMobile ? 320 : 1280);
  expect(bounds.y + bounds.height).toBeLessThanOrEqual(720);
  expect(await suites.evaluate((element) => element.scrollWidth <= element.clientWidth)).toBe(true);
  const sidebar = page.locator(".suite-panel-scroll:visible");
  const sidebarTop = await sidebar.evaluate((element) => element.scrollTop);
  await suites.getByRole("link").last().scrollIntoViewIfNeeded();
  expect(await suites.evaluate((element) => element.scrollTop)).toBeGreaterThan(0);
  expect(await sidebar.evaluate((element) => element.scrollTop)).toBe(sidebarTop);
  expect(await page.locator(".review-scroll").evaluate((element) => element.scrollTop)).toBe(0);
  await expect(page.getByRole("searchbox", { name: "Find a suite" })).toBeVisible();
  await screenshot(page, info.project.name, "navigation-dropdown-long-names-dark");
});

test("URL-only scenario names stay friendly while capture routes keep their IDs", async ({
  page,
  isMobile,
}, info) => {
  await page.goto("/suites/search-and-navigation");
  await expect(page.locator(".capture-card h2")).toHaveText([
    "Men / Running shoes / Desktop / Full page",
    "Women / Running shoes / Mobile / Full page",
    "Help center / Desktop / Full page",
    "Men / Running shoes / Mobile / Full page",
  ]);
  await screenshot(page, info.project.name, "url-only-scenarios");
  if (isMobile) await page.getByRole("button", { name: "Toggle suite index" }).click();
  const scenarios = page.getByRole("navigation", { name: "Test scenarios", exact: true });
  await expect(scenarios.locator(".scenario-group")).toHaveCount(1);
  await expect(scenarios.locator("summary")).toContainText("Men / Running shoes");
  await expect(scenarios.getByRole("link")).toHaveCount(4);
  const scenarioId = getAssetVersion("https://example.com/men/running-shoes");
  await expect(scenarios).not.toContainText(scenarioId);
  await expect(scenarios).not.toContainText("https://");
  await scenarios.getByRole("link", { name: "Men / Running shoes / Desktop / Full page" }).click();
  await expect(page).toHaveURL(new RegExp(`/images/${scenarioId}_desktop__FINAL\\.png$`));
  await expect(
    page.getByRole("heading", { name: "Men / Running shoes / Desktop / Full page", exact: true }),
  ).toBeVisible();
  await screenshot(page, info.project.name, "url-only-image");
  await page.reload();
  await expect(
    page.getByRole("heading", { name: "Men / Running shoes / Desktop / Full page", exact: true }),
  ).toBeVisible();
  if (isMobile) await page.getByRole("button", { name: "Toggle suite index" }).click();
  await page.getByRole("searchbox", { name: "Find a scenario" }).fill("Women / Running shoes");
  await expect(scenarios.getByRole("link")).toHaveCount(1);
  await expect(scenarios.getByRole("link")).toHaveText("Women / Running shoes");
});

test("scenario navigation is restricted to the selected suite and groups its captures", async ({
  page,
  isMobile,
}) => {
  await page.route("**/_snapshot/data/suites/**", async (route) => {
    const response = await route.fetch();
    const data = await response.json();
    data.tests.forEach((capture: { scenario: string }, index: number) => {
      capture.scenario = `${data.id} / ${index < 2 ? "First scenario" : "Second scenario"}`;
    });
    await route.fulfill({ response, json: data });
  });
  await page.goto("/suites/workspace-overview");
  if (isMobile) await page.getByRole("button", { name: "Toggle suite index" }).click();
  const scenarios = page.getByRole("navigation", { name: "Test scenarios", exact: true });
  await expect(scenarios.locator(".scenario-group")).toHaveCount(2);
  await expect(scenarios).toContainText("workspace-overview / First scenario");
  await page.getByRole("button", { name: /^Test suite:/ }).click();
  await page
    .getByRole("navigation", { name: "Test suites", exact: true })
    .getByRole("link", { name: /orders-and-checkout/ })
    .click();
  if (isMobile) await page.getByRole("button", { name: "Toggle suite index" }).click();
  await expect(scenarios).toContainText("orders-and-checkout / First scenario");
  await expect(scenarios).not.toContainText("workspace-overview");
  for (const link of await scenarios.getByRole("link").all())
    await expect(link).toHaveAttribute("href", /^\/suites\/orders-and-checkout\/images\//);
  await page.getByRole("button", { name: /^Test suite:/ }).click();
  await page
    .getByRole("navigation", { name: "Test suites", exact: true })
    .getByRole("link", { name: /empty-suite/ })
    .click();
  if (isMobile) await page.getByRole("button", { name: "Toggle suite index" }).click();
  await expect(scenarios).toContainText("No scenarios");
  await expect(scenarios.getByRole("link")).toHaveCount(0);
});

test("review panes resize and scroll independently", async ({ page, isMobile }) => {
  test.skip(isMobile, "Mobile uses the review navigation drawer.");
  await page.setViewportSize({ width: 1200, height: 420 });
  await page.goto("/suites/workspace-overview");
  await expect(page.locator(".capture-card")).toHaveCount(4);
  await page.evaluate(() => document.fonts.ready);
  const divider = page.getByRole("separator", { name: "Resize review navigation" });
  const panel = page.locator(".suite-panel");
  const start = await divider.boundingBox();
  const original = await panel.boundingBox();
  if (!start || !original) throw new Error("Review divider is unavailable");
  await page.mouse.move(start.x + start.width / 2, start.y + 100);
  await page.mouse.down();
  await page.mouse.move(start.x + 104, start.y + 100, { steps: 5 });
  await page.mouse.up();
  await expect
    .poll(async () => (await panel.boundingBox())?.width || 0)
    .toBeGreaterThan(original.width + 70);
  await divider.focus();
  await divider.press("Home");
  await expect(divider).toHaveAttribute("aria-valuenow", "220");
  await divider.press("ArrowRight");
  await expect(divider).toHaveAttribute("aria-valuenow", "236");
  const sidebarScroll = page.locator(".suite-panel-scroll");
  const reviewScroll = page.locator(".review-scroll");
  await sidebarScroll.hover({ position: { x: 60, y: 160 } });
  await page.mouse.wheel(0, 300);
  await expect
    .poll(() => sidebarScroll.evaluate((element) => element.scrollTop))
    .toBeGreaterThan(0);
  expect(await reviewScroll.evaluate((element) => element.scrollTop)).toBe(0);
  expect(await page.evaluate(() => window.scrollY)).toBe(0);
  const sidebarTop = await sidebarScroll.evaluate((element) => element.scrollTop);
  await reviewScroll.hover({ position: { x: 250, y: 200 } });
  await page.mouse.wheel(0, 400);
  await expect.poll(() => page.evaluate(() => window.scrollY)).toBeGreaterThan(0);
  expect(await sidebarScroll.evaluate((element) => element.scrollTop)).toBe(sidebarTop);
  expect(await reviewScroll.evaluate((element) => element.scrollTop)).toBe(0);
  const header = await page.locator(".app-header").boundingBox();
  const sidebar = await panel.boundingBox();
  const breadcrumb = await page.locator(".breadcrumb-bar").boundingBox();
  if (!header || !sidebar || !breadcrumb) throw new Error("Review layout is unavailable");
  expect(Math.abs(header.y)).toBeLessThanOrEqual(1);
  expect(Math.abs(sidebar.y - header.height)).toBeLessThanOrEqual(1);
  expect(breadcrumb.y).toBeLessThan(header.height);
  await assertLayout(page);
  await page.reload();
  await expect(divider).toHaveAttribute("aria-valuenow", "236");
  await divider.focus();
  await divider.press("End");
  await expect(divider).toHaveAttribute("aria-valuenow", "480");
  await page.setViewportSize({ width: 980, height: 520 });
  await expect(divider).toHaveAttribute("aria-valuenow", "441");
  await page.setViewportSize({ width: 390, height: 740 });
  await expect(divider).toHaveCount(0);
  await page.setViewportSize({ width: 1200, height: 520 });
  await expect(divider).toHaveAttribute("aria-valuenow", "480");
});

test("suite failure arrows scroll to failed captures without leaving the suite", async ({
  page,
}) => {
  await page.goto("/suites/workspace-overview?view=list");
  const previous = page.getByRole("button", { name: "Previous failed comparison", exact: true });
  const next = page.getByRole("button", { name: "Next failed comparison", exact: true });
  await expect(previous).toBeDisabled();
  for (let index = 0; index < 4; index++) {
    await next.click();
    await expect(page.getByLabel("Failed comparison position")).toHaveText(
      `${index + 1} / 4 failed`,
    );
    const selected = page.locator('.capture-card[data-active-failure="true"]');
    await expect(selected).toBeFocused();
    await expect(selected).toContainText("Failed");
    await expect(page).toHaveURL(/\/suites\/workspace-overview\?view=list$/);
  }
  await expect(next).toBeDisabled();
  await previous.click();
  await expect(page.getByLabel("Failed comparison position")).toHaveText("3 / 4 failed");
  await page.getByRole("searchbox", { name: "Search captures" }).fill("Recent orders");
  await expect(previous).toBeDisabled();
  await next.click();
  await expect(page.locator('.capture-card[data-active-failure="true"]')).toContainText(
    "Recent orders",
  );
  await expect(next).toBeDisabled();
  await page.goto("/suites/customer-accounts");
  await expect(previous).toBeDisabled();
  await expect(next).toBeDisabled();
});

test("visual mismatches remain failed while capture failures are engine errors", async ({
  page,
}, info) => {
  const dimensionMessage = "Dimensions differ: reference 100x100 vs test 80x140";
  const comparisonMessage = "Reference image not found";
  const captureMessage = "Test scenario failed to capture: click timed out";
  await page.route("**/_snapshot/data/manifest.json?*", async (route) => {
    const response = await route.fetch();
    const manifest = await response.json();
    const summary = manifest.suites.find(
      (suite: { id: string }) => suite.id === "workspace-overview",
    );
    summary.errors = 3;
    await route.fulfill({ response, json: manifest });
  });
  await page.route("**/_snapshot/data/suites/workspace-overview.json?*", async (route) => {
    const response = await route.fetch();
    const suite = await response.json();
    Object.assign(suite.tests[1], {
      mismatch: 0,
      error: dimensionMessage,
      dimensions: { same: false, width: -20, height: 40 },
    });
    Object.assign(suite.tests[2], {
      mismatch: null,
      error: null,
      comparisonError: comparisonMessage,
      reference: null,
      diff: null,
      dimensions: null,
    });
    Object.assign(suite.tests[3], {
      mismatch: null,
      error: captureMessage,
      diff: null,
      dimensions: null,
    });
    await route.fulfill({ response, json: suite });
  });

  await page.goto("/suites/workspace-overview?view=list");
  const cards = page.locator(".capture-card");
  await expect(cards).toHaveCount(4);
  await expect(cards.locator(".status-badge")).toHaveText(["Failed", "Failed", "Failed", "Error"]);
  await expect(cards.nth(1)).toContainText(dimensionMessage);
  await expect(cards.nth(2)).toContainText(comparisonMessage);
  const filters = page.getByRole("group", { name: "Filter results" });
  const errors = filters.getByRole("button", { name: /^Errors/ });
  await expect(errors).toHaveText(/Errors\s*1/);
  await errors.click();
  await expect(cards).toHaveCount(1);
  await expect(cards.first()).toContainText(captureMessage);
  await filters.getByRole("button", { name: /^Failed/ }).click();
  await expect(cards).toHaveCount(4);

  for (const [filename, label, message] of [
    ["overview__desktop.png", null, null],
    ["recent-orders__mobile.png", "Comparison failed", dimensionMessage],
    ["channel-summary__desktop.png", "Comparison failed", comparisonMessage],
    ["full-page__mobile.png", "Engine error", captureMessage],
  ] as const) {
    await page.goto(`/suites/workspace-overview/images/${filename}`);
    await expect(page.locator(".status-badge")).toHaveText(
      label === "Engine error" ? "Error" : "Failed",
    );
    await page.getByRole("button", { name: "Capture information", exact: true }).click();
    const bubble = page.getByRole("dialog", { name: "Capture information", exact: true });
    await expect(bubble).toBeVisible();
    await expect(bubble).toBeInViewport({ ratio: 1 });
    if (label && message) {
      const diagnostic = bubble.getByText(label, { exact: true });
      await diagnostic.scrollIntoViewIfNeeded();
      await expect(diagnostic).toBeInViewport({ ratio: 1 });
      await expect(bubble).toContainText(message);
      await screenshot(page, info.project.name, `capture-failure-${filename}`);
    } else {
      await expect(bubble.getByRole("alert")).toHaveCount(0);
    }
    if (label !== "Engine error") {
      await expect(bubble.getByText("Engine error", { exact: true })).toHaveCount(0);
    }
  }
});

test("tiny mismatch percentages remain visible in suite views and capture information", async ({
  page,
}, info) => {
  const cases = [
    {
      id: "overview__desktop.png",
      mismatch: 0.000017922688690066528,
      mismatchPixels: 1,
      status: "fail",
      expected: "0.00002",
    },
    {
      id: "recent-orders__mobile.png",
      mismatch: 0,
      mismatchPixels: 0,
      status: "pass",
      expected: "0.00",
    },
    {
      id: "channel-summary__desktop.png",
      mismatch: 0.005,
      mismatchPixels: 10,
      status: "fail",
      expected: "0.01",
    },
    { id: "full-page__mobile.png", mismatch: 1.2345, status: "fail", expected: "1.23" },
    {
      id: "smallest-mismatch.png",
      mismatch: Number.MIN_VALUE,
      mismatchPixels: 1,
      status: "fail",
      expected: "5e-324",
    },
    {
      id: "scientific-mismatch.png",
      mismatch: 1e-100,
      mismatchPixels: 1,
      status: "fail",
      expected: "1e-100",
    },
  ];
  await page.route("**/_snapshot/data/manifest.json?*", async (route) => {
    const response = await route.fetch();
    const manifest = await response.json();
    const summary = manifest.suites.find(
      (suite: { id: string }) => suite.id === "workspace-overview",
    );
    Object.assign(summary, { total: cases.length, passed: 1, failed: cases.length - 1, errors: 0 });
    await route.fulfill({ response, json: manifest });
  });
  await page.route("**/_snapshot/data/suites/workspace-overview.json?*", async (route) => {
    const response = await route.fetch();
    const suite = await response.json();
    for (const fixture of cases) {
      let capture = suite.tests.find((capture: { id: string }) => capture.id === fixture.id);
      if (!capture) {
        capture = { ...suite.tests[0], id: fixture.id, label: fixture.id, scenario: fixture.id };
        suite.tests.push(capture);
      }
      Object.assign(capture, {
        mismatch: fixture.mismatch,
        mismatchPixels: fixture.mismatchPixels,
        status: fixture.status,
        threshold: 0,
        error: null,
        comparisonError: null,
        dimensions: { same: true, width: 0, height: 0 },
      });
    }
    await route.fulfill({ response, json: suite });
  });

  await page.goto("/suites/workspace-overview");
  for (const view of ["Grid view", "List view"]) {
    await page.getByRole("button", { name: view, exact: true }).click();
    for (const fixture of cases) {
      const card = page.locator(`.capture-card[href*="/images/${fixture.id}"]`);
      await expect(card.locator(".capture-detail .mono").first()).toHaveText(
        `${fixture.expected}% difference`,
      );
      if (fixture.mismatchPixels !== undefined) {
        await expect(card).toContainText(
          `${fixture.mismatchPixels} mismatched ${fixture.mismatchPixels === 1 ? "pixel" : "pixels"}`,
        );
      } else {
        await expect(card).not.toContainText("mismatched pixel");
      }
      await expect(card.locator(".status-badge")).toHaveText(
        fixture.status === "pass" ? "Passed" : "Failed",
      );
    }
  }

  for (const fixture of cases) {
    await page.goto(`/suites/workspace-overview/images/${fixture.id}`);
    await page.getByRole("button", { name: "Capture information", exact: true }).click();
    const bubble = page.getByRole("dialog", { name: "Capture information", exact: true });
    await expect(bubble.getByText(`${fixture.expected}%`, { exact: true })).toBeVisible();
    if (fixture.mismatchPixels !== undefined) {
      const pixels = bubble
        .locator(".capture-metadata > div")
        .filter({ hasText: "Mismatched pixels" });
      await expect(pixels.locator("dd")).toHaveText(String(fixture.mismatchPixels));
    } else {
      await expect(bubble.getByText("Mismatched pixels", { exact: true })).toHaveCount(0);
    }
    await expect(bubble).toBeInViewport({ ratio: 1 });
    await expect(page.locator(".status-badge")).toHaveText(
      fixture.status === "pass" ? "Passed" : "Failed",
    );
    if (fixture.expected === "0.00002") {
      await screenshot(page, info.project.name, "tiny-mismatch-information");
    }
  }
});

test("capture information opens on hover or tap and remains selectable", async ({
  page,
  isMobile,
}, info) => {
  await page.goto("/suites/workspace-overview/images/overview__desktop.png");
  const trigger = page.getByRole("button", { name: "Capture information", exact: true });
  const bubble = page.getByRole("dialog", { name: "Capture information", exact: true });
  await expect(bubble).toHaveCount(0);
  if (isMobile) await trigger.tap();
  else await trigger.hover();
  await expect(bubble).toBeVisible();
  await expect(bubble).toBeInViewport({ ratio: 1 });
  await expect(bubble).toContainText("Difference");
  await expect(bubble).toContainText("Threshold");
  await expect(bubble).toContainText("overview__desktop.png");
  if (!isMobile) {
    await bubble.hover();
    await expect(bubble).toBeVisible();
    const text = bubble.getByText("overview__desktop.png", { exact: true });
    const bounds = await text.boundingBox();
    if (!bounds) throw new Error("Copyable capture label is unavailable");
    await page.mouse.move(bounds.x + 2, bounds.y + bounds.height / 2);
    await page.mouse.down();
    await page.mouse.move(
      bounds.x + Math.min(bounds.width - 2, 175),
      bounds.y + bounds.height / 2,
      { steps: 8 },
    );
    await page.mouse.up();
    await expect
      .poll(() => page.evaluate(() => window.getSelection()?.toString() || ""))
      .not.toBe("");
    const selectedText = await page.evaluate(() => window.getSelection()?.toString() || "");
    await page.context().grantPermissions(["clipboard-read", "clipboard-write"]);
    await page.keyboard.press("ControlOrMeta+C");
    await expect.poll(() => page.evaluate(() => navigator.clipboard.readText())).toBe(selectedText);
    await expect(bubble).toBeVisible();
  }
  await screenshot(page, info.project.name, "capture-information");
  expect((await new AxeBuilder({ page }).analyze()).violations).toEqual([]);
  await page.getByRole("button", { name: "Close capture information" }).click();
  await expect(bubble).toHaveCount(0);
  await trigger.focus();
  await expect(bubble).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(bubble).toHaveCount(0);
  await trigger.click();
  await page.mouse.move(1, 1);
  await expect(bubble).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(bubble).toHaveCount(0);
});

for (const [mode, unavailable] of [
  ["side", false],
  ["wipe", false],
  ["overlay", false],
  ["diff", false],
  ["reference", false],
  ["test", false],
  ["side", true],
] as const) {
  test(`comparison images reserve dimensions before delayed loads (${mode}${unavailable ? ", unavailable" : ""})`, async ({
    page,
  }) => {
    await page.emulateMedia({ reducedMotion: "reduce" });
    const imageSizes = {
      reference: { width: 320, height: 180 },
      test: { width: 200, height: 720 },
      diff: { width: 320, height: 720 },
    };
    const roles = ["reference", "test", "diff"] as const;
    const visibleRoles =
      mode === "side" ? roles : mode === "wipe" || mode === "overlay" ? roles.slice(0, 2) : [mode];
    const size = {
      width: mode === "test" ? 200 : 320,
      height: mode === "reference" ? 180 : 720,
    };
    const paths = Object.fromEntries(
      roles.map((role) => [role, `_snapshot/images/layout-${role}.png?v=${"a".repeat(20)}`]),
    );
    await page.route("**/_snapshot/data/suites/workspace-overview.json?*", async (route) => {
      const response = await route.fetch();
      const report = await response.json();
      report.tests = report.tests.map((capture: { id: string }) =>
        capture.id === "overview__desktop.png" ? { ...capture, ...paths, imageSizes } : capture,
      );
      await route.fulfill({ response, json: report });
    });
    let releaseImages = () => {};
    const imagesReady = new Promise<void>((resolve) => {
      releaseImages = resolve;
    });
    await page.route("**/_snapshot/images/layout-*.png?*", async (route) => {
      const role = roles.find((role) => route.request().url().includes(`layout-${role}.png`));
      if (!role) throw new Error("Unexpected layout fixture image");
      const image = new PNG(imageSizes[role]);
      image.data.fill(255);
      await imagesReady;
      await route.fulfill(
        unavailable && role === "test"
          ? { status: 404 }
          : { contentType: "image/png", body: PNG.sync.write(image) },
      );
    });
    try {
      await page.goto(`/suites/workspace-overview/images/overview__desktop.png?mode=${mode}`, {
        waitUntil: "domcontentloaded",
      });
      const images = page.locator(".image-world img");
      await expect(images).toHaveCount(visibleRoles.length);
      for (const [index, role] of visibleRoles.entries()) {
        await expect(images.nth(index)).toHaveAttribute("width", String(imageSizes[role].width));
        await expect(images.nth(index)).toHaveAttribute("height", String(imageSizes[role].height));
      }
      expect(
        await images.evaluateAll((elements) =>
          elements.every((element) => (element as HTMLImageElement).naturalWidth === 0),
        ),
      ).toBe(true);
      const reservedSizes = mode === "side" ? roles.map(() => size) : [size];
      await expect(page.locator(".image-world")).toHaveCount(reservedSizes.length);
      for (const [index, dimensions] of reservedSizes.entries()) {
        await expect(page.locator(".image-world").nth(index)).toHaveCSS(
          "width",
          `${dimensions.width}px`,
        );
        await expect(page.locator(".image-world").nth(index)).toHaveCSS(
          "height",
          `${dimensions.height}px`,
        );
      }
      await page.evaluate(() => document.fonts.ready);
      const canvas = page.locator(".zoom-canvas");
      const before = await canvas.boundingBox();
      const zoom = await page.getByLabel("Zoom level").textContent();
      releaseImages();
      if (unavailable) {
        await expect(page.locator(".image-world .image-unavailable")).toBeVisible();
        await expect(images).toHaveCount(visibleRoles.length - 1);
      }
      await expect
        .poll(() =>
          images.evaluateAll((elements) =>
            elements.every((element) => (element as HTMLImageElement).naturalWidth > 0),
          ),
        )
        .toBe(true);
      await expect(page.getByLabel("Zoom level")).toHaveText(zoom || "");
      const after = await canvas.boundingBox();
      if (!before || !after) throw new Error("Comparison canvas is not visible");
      for (const coordinate of ["x", "y", "width", "height"] as const) {
        expect(after[coordinate]).toBeCloseTo(before[coordinate], 0);
      }
    } finally {
      releaseImages();
    }
  });
}

test("image modes, zoom, downloads, and previous/next work", async ({ page, isMobile }, info) => {
  await page.goto("/suites/workspace-overview/images/overview__desktop.png");
  await expect(
    page.getByRole("heading", { name: "Workspace overview / Desktop", exact: true }),
  ).toBeVisible();
  await expect
    .poll(() =>
      page
        .locator(".image-world img")
        .evaluateAll((images) =>
          images.every((image) => (image as HTMLImageElement).naturalWidth > 0),
        ),
    )
    .toBe(true);
  await screenshot(page, info.project.name, "comparison-side");
  await page.getByRole("button", { name: "Actual size", exact: true }).click();
  await expect(page.getByLabel("Zoom level")).toHaveText("100%");
  await page.getByRole("button", { name: "Zoom in", exact: true }).click();
  await expect(page.getByLabel("Zoom level")).not.toHaveText("100%");
  await page.getByRole("button", { name: "Fit images", exact: true }).click();
  const mode = async (value: string, label: string) => {
    if (isMobile) await page.getByRole("combobox", { name: "Comparison mode" }).selectOption(value);
    else await page.getByRole("button", { name: label, exact: true }).click();
    await expect(page.locator(`.zoom-canvas.mode-${value}`)).toBeVisible();
  };
  await mode("wipe", "Wipe");
  if (!isMobile) {
    const bounds = await page.locator(".image-world").boundingBox();
    if (!bounds) throw new Error("Wipe image is not visible");
    await page.mouse.move(bounds.x + bounds.width * 0.25, bounds.y + bounds.height * 0.5);
    await expect(page.getByRole("slider", { name: "Wipe position" })).toHaveValue("25");
    await page.mouse.move(bounds.x + bounds.width * 0.75, bounds.y + bounds.height * 0.5);
    await expect(page.getByRole("slider", { name: "Wipe position" })).toHaveValue("75");
  }
  await page.getByRole("slider", { name: "Wipe position" }).focus();
  await page.getByRole("slider", { name: "Wipe position" }).press("Home");
  await expect(page.getByRole("slider", { name: "Wipe position" })).toHaveValue("0");
  await mode("overlay", "Overlay");
  await page.getByRole("slider", { name: "Test image opacity" }).press("End");
  await expect(page.getByRole("slider", { name: "Test image opacity" })).toHaveValue("100");
  await mode("diff", "Diff");
  await screenshot(page, info.project.name, "comparison-diff");
  await mode("reference", "Reference");
  await mode("test", "Test");
  const download = page.waitForEvent("download");
  await page.getByRole("link", { name: "Download test", exact: true }).click();
  expect((await download).suggestedFilename()).toContain("test.png");
  await page.getByRole("link", { name: "Next image", exact: true }).click();
  await expect(page).toHaveURL(/recent-orders__mobile.png/);
  await page.getByRole("link", { name: "Previous image", exact: true }).click();
  await expect(page).toHaveURL(/overview__desktop.png/);
  await page.reload();
  await expect(page.locator(".mode-test")).toBeVisible();
  await expect(page.locator(".page")).toHaveCSS("opacity", "1");
  expect((await new AxeBuilder({ page }).analyze()).violations).toEqual([]);
});

test("image viewer fit, anchored zoom, drag and keyboard controls stay scoped", async ({
  page,
}, info) => {
  await page.goto("/suites/workspace-overview/images/overview__desktop.png?mode=reference");
  const canvas = page.getByRole("region", { name: "Zoomable comparison images" });
  const content = canvas.locator(".image-pane");
  const surface = canvas.locator(".image-surface");
  await expect
    .poll(() => canvas.locator("img").evaluate((image: HTMLImageElement) => image.naturalWidth))
    .toBeGreaterThan(0);
  await page.getByRole("button", { name: "Fit to window", exact: true }).click();
  await expect(page.getByRole("button", { name: "Fit to window", exact: true })).toHaveAttribute(
    "aria-pressed",
    "true",
  );
  await expect
    .poll(() =>
      surface.evaluate((element) => {
        const bounds = element.getBoundingClientRect();
        const scroll = element.closest(".image-pane")?.getBoundingClientRect();
        return !!scroll && bounds.width <= scroll.width && bounds.bottom <= scroll.bottom + 1;
      }),
    )
    .toBe(true);
  await screenshot(page, info.project.name, "comparison-fit-window");

  await canvas.focus();
  await canvas.press("1");
  await expect(page.getByLabel("Zoom level")).toHaveText("100%");
  await canvas.press("+");
  await expect(page.getByLabel("Zoom level")).toHaveText("110%");
  await content.evaluate((element) => {
    element.scrollLeft = 200;
    element.scrollTop = 200;
  });
  const viewport = await content.boundingBox();
  if (!viewport) throw new Error("Review viewport is unavailable");
  const pointer = {
    x: viewport.x + Math.min(viewport.width - 30, 180),
    y: viewport.y + viewport.height - 100,
  };
  const before = await surface.boundingBox();
  if (!before) throw new Error("Comparison image is unavailable");
  await page.mouse.move(pointer.x, pointer.y);
  await page.keyboard.down("Control");
  try {
    await page.mouse.wheel(0, -40);
  } finally {
    await page.keyboard.up("Control");
  }
  await expect(page.getByLabel("Zoom level")).toHaveText("125%");
  const after = await surface.boundingBox();
  if (!after) throw new Error("Zoomed comparison image is unavailable");
  expect(Math.abs((pointer.x - before.x) / 1.1 - (pointer.x - after.x) / 1.25)).toBeLessThan(2);
  expect(Math.abs((pointer.y - before.y) / 1.1 - (pointer.y - after.y) / 1.25)).toBeLessThan(2);

  await content.evaluate((element) => {
    element.scrollLeft = 100;
    element.scrollTop = 0;
  });
  const start = await content.evaluate((element) => ({
    left: element.scrollLeft,
    top: element.scrollTop,
  }));
  await page.mouse.down();
  await page.mouse.move(pointer.x - 70, pointer.y - 60, { steps: 6 });
  await expect(canvas).toHaveClass(/is-panning/);
  await page.mouse.up();
  await expect(canvas).not.toHaveClass(/is-panning/);
  const dragged = await content.evaluate((element) => ({
    left: element.scrollLeft,
    top: element.scrollTop,
  }));
  expect(dragged.left - start.left).toBeCloseTo(70, 0);
  expect(dragged.top - start.top).toBeCloseTo(60, 0);
  await canvas.press("ArrowRight");
  await expect
    .poll(() => content.evaluate((element) => element.scrollLeft))
    .toBeGreaterThan(dragged.left);
  await canvas.press("0");
  await expect(page.getByRole("button", { name: "Fit to window", exact: true })).toHaveAttribute(
    "aria-pressed",
    "true",
  );
  await canvas.dblclick();
  await expect(page.getByLabel("Zoom level")).toHaveText("100%");
  await canvas.press("w");
  await expect(page.getByRole("button", { name: "Fit images", exact: true })).toHaveAttribute(
    "aria-pressed",
    "true",
  );
  expect(await page.evaluate(() => window.scrollY)).toBe(0);
});

test("image viewer wheel zoom anchors fitted images and captures empty viewport space", async ({
  page,
}) => {
  await page.goto("/suites/workspace-overview/images/overview__desktop.png?mode=reference");
  const canvas = page.getByRole("region", { name: "Zoomable comparison images" });
  const content = canvas.locator(".image-pane");
  const surface = canvas.locator(".image-surface");
  await expect
    .poll(() => canvas.locator("img").evaluate((image: HTMLImageElement) => image.naturalWidth))
    .toBeGreaterThan(0);
  await page.getByRole("button", { name: "Actual size", exact: true }).click();
  const bounds = await surface.boundingBox();
  const viewport = await content.boundingBox();
  if (!bounds || !viewport) throw new Error("Image viewer is unavailable");
  const pointer = {
    x: Math.min(bounds.x + bounds.width, viewport.x + viewport.width) - 24,
    y: Math.min(bounds.y + bounds.height, viewport.y + viewport.height) - 24,
  };
  const imagePoint = { x: pointer.x - bounds.x, y: pointer.y - bounds.y };
  const pageScale = await page.evaluate(() => ({
    width: innerWidth,
    ratio: devicePixelRatio,
    scale: visualViewport?.scale,
  }));
  let previousScale = 1;
  await page.keyboard.down("Control");
  try {
    for (const scale of [0.9, 0.8, 0.75, 2 / 3, 0.5, 1 / 3, 0.25, 0.25, 1 / 3, 0.5]) {
      await page.mouse.move(pointer.x, pointer.y);
      await page.mouse.wheel(0, scale > previousScale ? -40 : 40);
      previousScale = scale;
      await expect(page.getByLabel("Zoom level")).toHaveText(`${Math.round(scale * 1000) / 10}%`);
      await expect
        .poll(() =>
          surface.evaluate(
            (element, { point, image, scale }) => {
              const current = element.getBoundingClientRect();
              return Math.max(
                Math.abs(current.left + image.x * scale - point.x),
                Math.abs(current.top + image.y * scale - point.y),
              );
            },
            { point: pointer, image: imagePoint, scale },
          ),
        )
        .toBeLessThan(2);
    }
  } finally {
    await page.keyboard.up("Control");
  }
  const margin = { x: viewport.x + 8, y: viewport.y + viewport.height - 8 };
  const wheelIsCancelled = (modifier: "ctrlKey" | "metaKey" | null, inside = true) =>
    page.evaluate(
      ({ point, modifier, inside }) => {
        const target = inside ? document.elementFromPoint(point.x, point.y) : document.body;
        if (
          !target ||
          (inside &&
            document.querySelector(".comparison-viewer") &&
            (!target.closest(".image-pane") || target.closest(".image-surface")))
        )
          throw new Error("Expected empty image viewport space");
        const event = new WheelEvent("wheel", {
          bubbles: true,
          cancelable: true,
          clientX: point.x,
          clientY: point.y,
          deltaY: 40,
          ctrlKey: modifier === "ctrlKey",
          metaKey: modifier === "metaKey",
        });
        target.dispatchEvent(event);
        return event.defaultPrevented;
      },
      { point: margin, modifier, inside },
    );
  expect(await wheelIsCancelled("ctrlKey")).toBe(true);
  expect(await wheelIsCancelled("metaKey")).toBe(true);
  await expect(page.getByLabel("Zoom level")).toHaveText("25%");
  expect(await wheelIsCancelled("ctrlKey")).toBe(true);
  expect(await wheelIsCancelled(null)).toBe(false);
  expect(await wheelIsCancelled("ctrlKey", false)).toBe(false);
  expect(
    await page.evaluate(() => ({
      width: innerWidth,
      ratio: devicePixelRatio,
      scale: visualViewport?.scale,
    })),
  ).toEqual(pageScale);
  await page.getByRole("button", { name: "Capture Log", exact: true }).click();
  await expect(canvas).toHaveCount(0);
  expect(await wheelIsCancelled("ctrlKey")).toBe(false);
});

test("image viewer anchors wheel zoom to the pixel in each side-by-side lane", async ({ page }) => {
  await page.goto("/suites/workspace-overview/images/overview__desktop.png?mode=side");
  const images = page.locator(".comparison-lane img");
  await expect(images).toHaveCount(3);
  await expect
    .poll(() =>
      images.evaluateAll((elements) =>
        elements.every((image) => (image as HTMLImageElement).naturalWidth > 0),
      ),
    )
    .toBe(true);
  for (const image of await images.all()) {
    await page.getByRole("button", { name: "Fit to window", exact: true }).click();
    await image.locator("..").hover();
    const before = await image.boundingBox();
    if (!before) throw new Error("Comparison image is unavailable");
    const pointer = {
      x: Math.round(before.x + before.width * 0.6),
      y: Math.round(before.y + before.height * 0.2),
    };
    const horizontal = (pointer.x - before.x) / before.width;
    const vertical = (pointer.y - before.y) / before.height;
    await page.mouse.move(pointer.x, pointer.y);
    await page.keyboard.down("Control");
    try {
      for (let step = 0; step < 2; step++) {
        const previous = Number.parseFloat(await page.getByLabel("Zoom level").innerText());
        await page.mouse.wheel(0, -40);
        await expect
          .poll(async () => Number.parseFloat(await page.getByLabel("Zoom level").innerText()))
          .toBeGreaterThan(previous);
        await expect
          .poll(() =>
            image.evaluate(
              (element, { pointer, horizontal, vertical }) => {
                const current = element.getBoundingClientRect();
                return Math.max(
                  Math.abs(current.left + current.width * horizontal - pointer.x),
                  Math.abs(current.top + current.height * vertical - pointer.y),
                );
              },
              { pointer, horizontal, vertical },
            ),
          )
          .toBeLessThan(2);
      }
    } finally {
      await page.keyboard.up("Control");
    }
  }
});

async function mockComparisonImages(
  page: Page,
  imageSizes: Record<"reference" | "test" | "diff", { width: number; height: number }>,
  hasDiff: boolean,
) {
  const roles = ["reference", "test", "diff"] as const;
  const paths = Object.fromEntries(
    roles.map((role) => [role, `_snapshot/images/shared-canvas-${role}.png?v=${"a".repeat(20)}`]),
  );
  await page.route("**/_snapshot/data/suites/workspace-overview.json?*", async (route) => {
    const response = await route.fetch();
    const report = await response.json();
    Object.assign(report.tests[0], paths, { imageSizes, diff: hasDiff ? paths.diff : null });
    delete report.tests[0].alignment;
    await route.fulfill({ response, json: report });
  });
  await page.route("**/_snapshot/images/shared-canvas-*.png?*", async (route) => {
    const role = roles.find((role) => route.request().url().includes(`shared-canvas-${role}.png`));
    if (!role) throw new Error("Unexpected shared canvas fixture image");
    const image = new PNG(imageSizes[role]);
    image.data.fill(255);
    await route.fulfill({ contentType: "image/png", body: PNG.sync.write(image) });
  });
}

for (const hasDiff of [false, true]) {
  test(`image viewer keeps ${hasDiff ? "three" : "two"} lanes bounded with native scrollbars`, async ({
    page,
  }, info) => {
    await page.emulateMedia({ reducedMotion: "reduce" });
    if (!hasDiff) {
      await page.route("**/_snapshot/data/suites/**", async (route) => {
        const response = await route.fetch();
        const data = await response.json();
        if (data.id === "workspace-overview") data.tests[0].diff = null;
        await route.fulfill({ response, json: data });
      });
    }
    await page.goto("/suites/workspace-overview/images/overview__desktop.png?mode=side");
    const lanes = page.locator(".comparison-lane");
    await expect(lanes).toHaveCount(hasDiff ? 3 : 2);
    const heading = await page.locator(".image-page .page-heading").boundingBox();
    const controls = await page.locator(".comparison-modes").boundingBox();
    const before = await lanes.first().boundingBox();
    if (!heading || !controls || !before) throw new Error("Image viewer is unavailable");
    await page.getByRole("button", { name: "Actual size", exact: true }).click();
    for (let step = 0; step < 5; step++) {
      await page.getByRole("button", { name: "Zoom in", exact: true }).click();
    }
    await expect(page.getByLabel("Zoom level")).toHaveText("200%");
    for (const lane of await lanes.all()) {
      const bounds = await lane.boundingBox();
      if (!bounds) throw new Error("Comparison lane is unavailable");
      expect(Math.abs(bounds.width - before.width)).toBeLessThan(1);
      expect(bounds.x).toBeGreaterThanOrEqual(before.x - 1);
      expect(bounds.x + bounds.width).toBeLessThanOrEqual(controls.x + controls.width + 1);
      const overflow = await lane.evaluate((element) => ({
        horizontal: element.scrollWidth > element.clientWidth,
        vertical: element.scrollHeight > element.clientHeight,
      }));
      expect(overflow).toEqual({ horizontal: true, vertical: true });
      await lane.hover({ position: { x: bounds.width / 2, y: Math.min(80, bounds.height / 2) } });
      await page.mouse.wheel(160, 200);
      await expect.poll(() => lane.evaluate((element) => element.scrollTop)).toBeGreaterThan(0);
      await expect.poll(() => lane.evaluate((element) => element.scrollLeft)).toBeGreaterThan(0);
      await lane.evaluate((element) => {
        element.scrollLeft = element.scrollWidth;
        element.scrollTop = element.scrollHeight;
      });
      expect(
        await lane.evaluate((element) => {
          const image = element.querySelector("img")?.getBoundingClientRect();
          const pane = element.getBoundingClientRect();
          return !!image && image.right <= pane.right + 1 && image.bottom <= pane.bottom + 1;
        }),
      ).toBe(true);
      await lane.evaluate((element) => element.scrollTo(0, 0));
    }
    expect(await page.locator(".image-page .page-heading").boundingBox()).toEqual(heading);
    expect(await page.locator(".comparison-modes").boundingBox()).toEqual(controls);
    expect(
      await page.locator(".review-scroll").evaluate((element) => ({
        left: element.scrollLeft,
        top: element.scrollTop,
        overflow: element.scrollWidth > element.clientWidth,
      })),
    ).toEqual({ left: 0, top: 0, overflow: false });
    await screenshot(
      page,
      info.project.name,
      `comparison-fixed-${hasDiff ? "three" : "two"}-lanes`,
    );
  });

  test(`image viewer synchronizes ${hasDiff ? "three" : "two"} unequal lanes on the maximum canvas`, async ({
    page,
  }) => {
    await page.emulateMedia({ reducedMotion: "reduce" });
    const imageSizes = {
      reference: { width: 2000, height: 1000 },
      test: { width: 3000, height: 500 },
      diff: { width: 2500, height: 800 },
    };
    await mockComparisonImages(page, imageSizes, hasDiff);
    await page.goto("/suites/workspace-overview/images/overview__desktop.png?mode=side");
    const lanes = page.locator(".comparison-lane");
    await expect(lanes).toHaveCount(hasDiff ? 3 : 2);
    await expect
      .poll(() =>
        lanes
          .locator("img")
          .evaluateAll((images) =>
            images.every((image) => (image as HTMLImageElement).naturalWidth > 0),
          ),
      )
      .toBe(true);
    await page.getByRole("button", { name: "Actual size", exact: true }).click();
    const expectCanvas = async () => {
      expect(
        await lanes.evaluateAll((elements) =>
          elements.map((element) => ({
            width: element.scrollWidth,
            height: element.scrollHeight,
          })),
        ),
      ).toEqual(Array.from({ length: hasDiff ? 3 : 2 }, () => ({ width: 3000, height: 1000 })));
    };
    await expectCanvas();
    for (const [index, dimensions] of Object.values(imageSizes)
      .slice(0, hasDiff ? 3 : 2)
      .entries()) {
      const image = lanes.nth(index).locator("img");
      await expect(image).toHaveCSS("width", `${dimensions.width}px`);
      await expect(image).toHaveCSS("height", `${dimensions.height}px`);
    }
    const limits = await lanes.evaluateAll((elements) =>
      elements.map((element) => {
        element.scrollTo(element.scrollWidth, element.scrollHeight);
        const limit = { left: element.scrollLeft, top: element.scrollTop };
        element.scrollTo(0, 0);
        return limit;
      }),
    );
    const source = lanes.nth(1);
    const sourceLimit = limits[1];
    if (!sourceLimit || !limits[0]) throw new Error("Comparison lanes are unavailable");
    for (const limit of limits) {
      expect(Math.abs(limit.left - sourceLimit.left)).toBeLessThanOrEqual(1);
      expect(limit.top).toBe(sourceLimit.top);
    }
    const expectPosition = async (position: { left: number; top: number }) => {
      await expect
        .poll(() =>
          lanes.evaluateAll(async (elements) => {
            await new Promise<void>((resolve) =>
              requestAnimationFrame(() => requestAnimationFrame(() => resolve())),
            );
            return elements.map((element) => ({
              left: element.scrollLeft,
              top: element.scrollTop,
            }));
          }),
        )
        .toEqual(
          limits.map((limit) => ({
            left: Math.min(position.left, limit.left),
            top: Math.min(position.top, limit.top),
          })),
        );
    };
    for (const [index, lane] of (await lanes.all()).entries()) {
      await lane.evaluate((element) => element.scrollTo(element.scrollWidth, element.scrollHeight));
      await expectPosition(limits[index] ?? sourceLimit);
      const pointer = await lane.evaluate((element) => {
        const bounds = element.getBoundingClientRect();
        const image = element.querySelector("img")?.getBoundingClientRect();
        if (!image) throw new Error("Comparison image is unavailable");
        return {
          x: Math.floor(bounds.left + element.clientLeft + element.clientWidth - 20),
          y: Math.floor(bounds.top + element.clientTop + element.clientHeight - 20),
          right: image.right,
          bottom: image.bottom,
        };
      });
      expect(pointer.x > pointer.right || pointer.y > pointer.bottom).toBe(true);
      await page.mouse.move(pointer.x, pointer.y);
      await expect(page.locator(".comparison-crosshair")).toHaveCount(hasDiff ? 3 : 2);
      const guide = await lane.locator(".crosshair-horizontal").boundingBox();
      if (!guide) throw new Error("Blank canvas must retain its crosshair");
      expect(Math.abs(guide.y + guide.height / 2 - pointer.y)).toBeLessThan(1);
      await expectCanvas();
      await lane.evaluate((element) => element.scrollTo(0, 0));
      await expectPosition({ left: 0, top: 0 });
    }
    await source.evaluate((element) => element.scrollTo(160, 80));
    await expectPosition({ left: 160, top: 80 });
    await source.focus();
    await source.press("ArrowLeft");
    await source.press("ArrowUp");
    await expectPosition({ left: 120, top: 40 });
    const bounds = await source.boundingBox();
    if (!bounds) throw new Error("Image viewport is unavailable");
    const pointer = { x: Math.round(bounds.x + 30), y: Math.round(bounds.y + 60) };
    await page.mouse.move(pointer.x, pointer.y);
    await page.mouse.down();
    await page.mouse.move(pointer.x - 20, pointer.y - 30, { steps: 3 });
    await page.mouse.up();
    await expectPosition({ left: 140, top: 70 });

    const image = source.locator("img");
    const before = await image.boundingBox();
    if (!before) throw new Error("Comparison image is unavailable");
    const pixel = {
      horizontal: (pointer.x - before.x) / before.width,
      vertical: (pointer.y - before.y) / before.height,
    };
    await page.mouse.move(pointer.x, pointer.y);
    await page.keyboard.down("Control");
    try {
      await page.mouse.wheel(0, -100);
      await expect(page.getByLabel("Zoom level")).toHaveText("110%");
      await expect
        .poll(() =>
          image.evaluate(
            (element, position) => {
              const bounds = element.getBoundingClientRect();
              return Math.max(
                Math.abs(bounds.left + bounds.width * position.horizontal - position.x),
                Math.abs(bounds.top + bounds.height * position.vertical - position.y),
              );
            },
            { ...pointer, ...pixel },
          ),
        )
        .toBeLessThan(2);
    } finally {
      await page.keyboard.up("Control");
    }
    await page.getByRole("button", { name: "Fit to window", exact: true }).click();
    await expectPosition({ left: 0, top: 0 });
  });

  test(`image viewer drags ${hasDiff ? "three" : "two"} fitted lanes together within their edges`, async ({
    page,
  }) => {
    await page.emulateMedia({ reducedMotion: "reduce" });
    await mockComparisonImages(
      page,
      {
        reference: { width: 60, height: 1000 },
        test: { width: 90, height: 500 },
        diff: { width: 75, height: 800 },
      },
      hasDiff,
    );
    await page.goto("/suites/workspace-overview/images/overview__desktop.png?mode=side");
    const lanes = page.locator(".comparison-lane");
    await expect(lanes.locator("img")).toHaveCount(hasDiff ? 3 : 2);
    await expect
      .poll(() =>
        lanes
          .locator("img")
          .evaluateAll((images) =>
            images.every((image) => (image as HTMLImageElement).naturalWidth > 0),
          ),
      )
      .toBe(true);
    const placement = () =>
      lanes.evaluateAll((elements) =>
        elements.map((element) => {
          const bounds = element.getBoundingClientRect();
          const surface = element.querySelector(".image-surface")?.getBoundingClientRect();
          if (!surface) throw new Error("Comparison canvas is unavailable");
          return {
            left: surface.left - bounds.left - element.clientLeft,
            right: bounds.left + element.clientLeft + element.clientWidth - surface.right,
            overflow: element.scrollWidth - element.clientWidth,
          };
        }),
      );
    for (const fit of ["Fit to window", "Fit images", "Actual size", "Zoom in"]) {
      await page.getByRole("button", { name: fit, exact: true }).click();
      if (fit === "Actual size" || fit === "Zoom in") {
        await expect(page.getByLabel("Zoom level")).toHaveText(
          fit === "Actual size" ? "100%" : "110%",
        );
      }
      const zoom = await page.getByLabel("Zoom level").innerText();
      const initial = await placement();
      for (const pane of initial) {
        expect(pane.left + pane.right).toBeGreaterThan(0);
        if (fit === "Fit to window" || fit === "Fit images")
          expect(Math.abs(pane.left - pane.right)).toBeLessThan(1);
        expect(pane.overflow).toBe(0);
      }
      for (const [index, lane] of (await lanes.all()).entries()) {
        for (const direction of [-1, 1]) {
          const pointer = await lane.evaluate((element) => {
            const pane = element.getBoundingClientRect();
            const surface = element.querySelector(".image-surface")?.getBoundingClientRect();
            if (!surface) throw new Error("Comparison canvas is unavailable");
            return {
              x: Math.round(surface.left + surface.width / 2),
              y: Math.round(pane.top + element.clientTop + 20),
              left: pane.left + element.clientLeft - surface.left - 2,
              right: pane.left + element.clientLeft + element.clientWidth - surface.right + 2,
            };
          });
          await page.mouse.move(pointer.x, pointer.y);
          await page.mouse.down();
          try {
            await page.mouse.move(
              pointer.x + (direction < 0 ? pointer.left : pointer.right),
              pointer.y,
              {
                steps: 4,
              },
            );
          } finally {
            await page.mouse.up();
          }
          await expect
            .poll(async () =>
              Math.max(
                ...(await placement()).map((pane) =>
                  Math.abs(direction < 0 ? pane.left : pane.right),
                ),
              ),
            )
            .toBeLessThan(1);
          expect((await placement()).every((pane) => pane.overflow === 0)).toBe(true);
          await expect(page.getByLabel("Zoom level")).toHaveText(zoom);
          await expect(lanes).toHaveCount(hasDiff ? 3 : 2);
          await lane.focus();
          if (fit !== "Fit to window") {
            await lane.press("ArrowDown");
            await expect
              .poll(() => lanes.nth(index).evaluate((element) => element.scrollTop))
              .toBeGreaterThan(0);
          }
        }
      }
    }
    const viewport = page.viewportSize();
    if (!viewport) throw new Error("Comparison viewport is unavailable");
    for (const width of [viewport.width + 80, viewport.width]) {
      await page.setViewportSize({ ...viewport, width });
      await expect
        .poll(async () => Math.max(...(await placement()).map((pane) => Math.abs(pane.right))))
        .toBeLessThan(1);
      expect((await placement()).every((pane) => pane.overflow === 0)).toBe(true);
    }
    const source = lanes.first();
    const image = source.locator("img");
    const before = await image.boundingBox();
    const pane = await source.boundingBox();
    if (!before || !pane) throw new Error("Comparison image is unavailable");
    const pointer = {
      x: Math.round(before.x + before.width / 2),
      y: Math.round(pane.y + 30),
    };
    const pixel = {
      x: (pointer.x - before.x) / before.width,
      y: (pointer.y - before.y) / before.height,
    };
    await page.mouse.move(pointer.x, pointer.y);
    await page.keyboard.down("Control");
    try {
      await page.mouse.wheel(0, 100);
      await expect(page.getByLabel("Zoom level")).toHaveText("100%");
      await expect
        .poll(() =>
          image.evaluate(
            (element, { pointer, pixel }) => {
              const bounds = element.getBoundingClientRect();
              return Math.max(
                Math.abs(bounds.left + pixel.x * bounds.width - pointer.x),
                Math.abs(bounds.top + pixel.y * bounds.height - pointer.y),
              );
            },
            { pointer, pixel },
          ),
        )
        .toBeLessThan(2);
    } finally {
      await page.keyboard.up("Control");
    }
    await page.mouse.down();
    try {
      await page.mouse.move(pointer.x - pane.width, pointer.y, { steps: 4 });
    } finally {
      await page.mouse.up();
    }
    await expect
      .poll(async () => Math.max(...(await placement()).map((pane) => Math.abs(pane.left))))
      .toBeLessThan(1);
    expect((await placement()).every((pane) => pane.overflow === 0)).toBe(true);
    await expect(page.getByLabel("Zoom level")).toHaveText("100%");
    await page.getByRole("button", { name: "Fit to window", exact: true }).click();
    for (const pane of await placement()) {
      expect(Math.abs(pane.left - pane.right)).toBeLessThan(1);
    }
  });
}

for (const hasDiff of [false, true]) {
  test(`image viewer drags ${hasDiff ? "three" : "two"} short lanes vertically within their edges`, async ({
    page,
  }) => {
    await page.emulateMedia({ reducedMotion: "reduce" });
    await mockComparisonImages(
      page,
      {
        reference: { width: 1000, height: 60 },
        test: { width: 500, height: 90 },
        diff: { width: 800, height: 75 },
      },
      hasDiff,
    );
    await page.goto("/suites/workspace-overview/images/overview__desktop.png?mode=side");
    const lanes = page.locator(".comparison-lane");
    await expect(lanes.locator("img")).toHaveCount(hasDiff ? 3 : 2);
    const placement = () =>
      lanes.evaluateAll((elements) =>
        elements.map((element) => {
          const pane = element.getBoundingClientRect();
          const surface = element.querySelector(".image-surface")?.getBoundingClientRect();
          if (!surface) throw new Error("Comparison canvas is unavailable");
          return {
            top: surface.top - pane.top - element.clientTop,
            bottom: pane.top + element.clientTop + element.clientHeight - surface.bottom,
            overflow: element.scrollHeight - element.clientHeight,
          };
        }),
      );
    for (const fit of ["Fit to window", "Fit images", "Actual size", "Zoom in"]) {
      await page.getByRole("button", { name: fit, exact: true }).click();
      if (fit === "Actual size" || fit === "Zoom in") {
        await expect(page.getByLabel("Zoom level")).toHaveText(
          fit === "Actual size" ? "100%" : "110%",
        );
      }
      const zoom = await page.getByLabel("Zoom level").innerText();
      for (const lane of await lanes.all()) {
        for (const direction of [1, -1, 1]) {
          const pointer = await lane.evaluate((element) => {
            const pane = element.getBoundingClientRect();
            const surface = element.querySelector(".image-surface")?.getBoundingClientRect();
            if (!surface) throw new Error("Comparison canvas is unavailable");
            return {
              x: Math.round(Math.max(pane.left + element.clientLeft, surface.left) + 12),
              y: Math.round(surface.top + surface.height / 2),
              up: pane.top + element.clientTop - surface.top - 2,
              down: pane.top + element.clientTop + element.clientHeight - surface.bottom + 2,
            };
          });
          await page.mouse.move(pointer.x, pointer.y);
          await page.mouse.down();
          try {
            await page.mouse.move(
              pointer.x,
              pointer.y + (direction < 0 ? pointer.up : pointer.down),
              {
                steps: 4,
              },
            );
          } finally {
            await page.mouse.up();
          }
          await expect
            .poll(async () =>
              Math.max(
                ...(await placement()).map((pane) =>
                  Math.abs(direction < 0 ? pane.top : pane.bottom),
                ),
              ),
            )
            .toBeLessThan(1);
          expect((await placement()).every((pane) => pane.overflow === 0)).toBe(true);
          await expect(page.getByLabel("Zoom level")).toHaveText(zoom);
          await expect(lanes).toHaveCount(hasDiff ? 3 : 2);
        }
        if (fit === "Actual size" || fit === "Zoom in") {
          await lane.focus();
          await lane.press("ArrowRight");
          await expect
            .poll(() => lane.evaluate((element) => element.scrollLeft))
            .toBeGreaterThan(0);
        }
      }
    }
    const viewport = page.viewportSize();
    if (!viewport) throw new Error("Comparison viewport is unavailable");
    for (const height of [viewport.height + 100, viewport.height]) {
      await page.setViewportSize({ ...viewport, height });
      await expect
        .poll(async () => Math.max(...(await placement()).map((pane) => Math.abs(pane.bottom))))
        .toBeLessThan(1);
      expect((await placement()).every((pane) => pane.overflow === 0)).toBe(true);
    }
    const source = lanes.first();
    const surface = source.locator(".image-surface");
    const before = await surface.boundingBox();
    const pane = await source.boundingBox();
    if (!before || !pane) throw new Error("Comparison canvas is unavailable");
    const pointer = { x: Math.round(pane.x + 30), y: Math.round(before.y + before.height / 2) };
    const pixel = {
      x: (pointer.x - before.x) / before.width,
      y: (pointer.y - before.y) / before.height,
    };
    await page.mouse.move(pointer.x, pointer.y);
    await page.keyboard.down("Control");
    try {
      await page.mouse.wheel(0, 100);
      await expect(page.getByLabel("Zoom level")).toHaveText("100%");
      await expect
        .poll(() =>
          surface.evaluate(
            (element, { pointer, pixel }) => {
              const bounds = element.getBoundingClientRect();
              return Math.max(
                Math.abs(bounds.left + pixel.x * bounds.width - pointer.x),
                Math.abs(bounds.top + pixel.y * bounds.height - pointer.y),
              );
            },
            { pointer, pixel },
          ),
        )
        .toBeLessThan(2);
    } finally {
      await page.keyboard.up("Control");
    }
    await page.mouse.down();
    try {
      await page.mouse.move(pointer.x, pointer.y - pane.height, { steps: 4 });
    } finally {
      await page.mouse.up();
    }
    await expect
      .poll(async () => Math.max(...(await placement()).map((pane) => Math.abs(pane.top))))
      .toBeLessThan(1);
    for (const fit of ["Fit to window", "Fit images"]) {
      await page.getByRole("button", { name: fit, exact: true }).click();
      await expect
        .poll(async () => Math.max(...(await placement()).map((pane) => Math.abs(pane.top))))
        .toBeLessThan(1);
      expect((await placement()).every((pane) => pane.overflow === 0)).toBe(true);
    }
  });
}

test("image viewer drags underfilled canvases diagonally without overflow", async ({
  page,
  isMobile,
}) => {
  await page.emulateMedia({ reducedMotion: "reduce" });
  await mockComparisonImages(
    page,
    {
      reference: { width: 60, height: 40 },
      test: { width: 90, height: 50 },
      diff: { width: 75, height: 80 },
    },
    true,
  );
  await page.goto("/suites/workspace-overview/images/overview__desktop.png?mode=side");
  await page.getByRole("button", { name: "Actual size", exact: true }).click();
  const lanes = page.locator(".comparison-lane");
  await expect(lanes).toHaveCount(3);
  for (const direction of [1, -1, 1]) {
    const drag = await lanes.first().evaluate((element) => {
      const pane = element.getBoundingClientRect();
      const image = element.querySelector(".image-surface")?.getBoundingClientRect();
      if (!image) throw new Error("Comparison canvas is unavailable");
      return {
        x: image.left + image.width / 2,
        y: image.top + image.height / 2,
        left: pane.left + element.clientLeft - image.left - 2,
        right: pane.left + element.clientLeft + element.clientWidth - image.right + 2,
        top: pane.top + element.clientTop - image.top - 2,
        bottom: pane.top + element.clientTop + element.clientHeight - image.bottom + 2,
      };
    });
    await page.mouse.move(drag.x, drag.y);
    await page.mouse.down();
    try {
      await page.mouse.move(
        drag.x + (direction < 0 ? drag.left : drag.right),
        drag.y + (direction < 0 ? drag.top : drag.bottom),
        { steps: 4 },
      );
    } finally {
      await page.mouse.up();
    }
    await expect
      .poll(() =>
        lanes.evaluateAll(
          (elements, direction) =>
            Math.max(
              ...elements.map((element) => {
                const pane = element.getBoundingClientRect();
                const image = element.querySelector(".image-surface")?.getBoundingClientRect();
                if (!image) throw new Error("Comparison canvas is unavailable");
                return Math.max(
                  Math.abs(
                    direction < 0
                      ? image.left - pane.left - element.clientLeft
                      : pane.left + element.clientLeft + element.clientWidth - image.right,
                  ),
                  Math.abs(
                    direction < 0
                      ? image.top - pane.top - element.clientTop
                      : pane.top + element.clientTop + element.clientHeight - image.bottom,
                  ),
                  element.scrollWidth - element.clientWidth,
                  element.scrollHeight - element.clientHeight,
                );
              }),
            ),
          direction,
        ),
      )
      .toBeLessThan(1);
    await expect(page.getByLabel("Zoom level")).toHaveText("100%");
  }
  await page.screenshot({
    path: `.fixtures\\screenshots\\${isMobile ? "mobile" : "desktop"}\\comparison-diagonal-pan.png`,
  });
});

test("image viewer keeps per-mode zoom and cancels interrupted drags", async ({
  page,
  isMobile,
}) => {
  await page.goto("/suites/workspace-overview/images/overview__desktop.png?mode=reference");
  const canvas = page.getByRole("region", { name: "Zoomable comparison images" });
  const mode = async (value: string, label: string) => {
    if (isMobile) await page.getByRole("combobox", { name: "Comparison mode" }).selectOption(value);
    else await page.getByRole("button", { name: label, exact: true }).click();
  };
  await page.getByRole("button", { name: "Actual size", exact: true }).click();
  await page.getByRole("button", { name: "Zoom in", exact: true }).click();
  await mode("test", "Test");
  await page.getByRole("button", { name: "Fit to window", exact: true }).click();
  await mode("reference", "Reference");
  await expect(page.getByLabel("Zoom level")).toHaveText("110%");
  await page.locator(".image-pane").evaluate((element) => {
    element.scrollLeft = 0;
    element.scrollTop = 0;
  });
  await canvas.hover({ position: { x: 100, y: 100 } });
  await page.mouse.down();
  await page.mouse.move(90, 90);
  await page.evaluate(() => window.dispatchEvent(new Event("blur")));
  await page.mouse.up();
  await expect(canvas).not.toHaveClass(/is-panning/);
  const scroll = await page.locator(".image-pane").evaluate((element) => element.scrollTop);
  await canvas.dispatchEvent("pointerdown", {
    pointerId: 8,
    pointerType: "touch",
    button: 0,
    buttons: 1,
    clientX: 100,
    clientY: 100,
  });
  await canvas.dispatchEvent("pointermove", {
    pointerId: 8,
    pointerType: "touch",
    buttons: 1,
    clientX: 50,
    clientY: 50,
  });
  await canvas.dispatchEvent("pointercancel", { pointerId: 8, pointerType: "touch" });
  expect(await page.locator(".image-pane").evaluate((element) => element.scrollTop)).toBe(scroll);
  await mode("overlay", "Overlay");
  const slider = page.getByRole("slider", { name: "Test image opacity" });
  await slider.focus();
  await slider.press("ArrowRight");
  await expect(slider).toHaveValue("51");
  await page.getByRole("link", { name: "Next image", exact: true }).click();
  await expect(page.getByLabel("Zoom level")).not.toHaveText("110%");
});

test("scrolling over images synchronizes lanes without changing zoom", async ({ page }) => {
  await page.goto("/suites/workspace-overview/images/overview__desktop.png?mode=side");
  await expect(page.locator(".comparison-lane img")).toHaveCount(3);
  await expect
    .poll(() =>
      page
        .locator(".comparison-lane img")
        .evaluateAll((images) =>
          images.every((image) => (image as HTMLImageElement).naturalWidth > 0),
        ),
    )
    .toBe(true);
  await page.getByRole("button", { name: "Actual size", exact: true }).click();
  await expect(page.getByLabel("Zoom level")).toHaveText("100%");
  await page.locator(".zoom-canvas").hover({ position: { x: 80, y: 80 } });
  await expect(page.locator(".comparison-crosshair")).toHaveCount(3);
  for (const content of await page.locator(".comparison-lane").all()) {
    await content.hover({ position: { x: 40, y: 40 } });
    await page.mouse.wheel(160, 300);
    await expect.poll(() => content.evaluate((element) => element.scrollTop)).toBeGreaterThan(0);
    await expect.poll(() => content.evaluate((element) => element.scrollLeft)).toBeGreaterThan(0);
    await expect(page.locator(".comparison-crosshair")).toHaveCount(3);
    const position = await content.evaluate((element) => ({
      left: element.scrollLeft,
      top: element.scrollTop,
    }));
    await expect
      .poll(() =>
        page
          .locator(".comparison-lane")
          .evaluateAll(
            (lanes, position) =>
              lanes.every(
                (lane) =>
                  lane.scrollLeft ===
                    Math.min(position.left, lane.scrollWidth - lane.clientWidth) &&
                  lane.scrollTop === Math.min(position.top, lane.scrollHeight - lane.clientHeight),
              ),
            position,
          ),
      )
      .toBe(true);
    await page.mouse.wheel(-80, -120);
    await expect
      .poll(() => content.evaluate((element) => element.scrollTop))
      .toBeLessThan(position.top);
    const returned = await content.evaluate((element) => ({
      left: element.scrollLeft,
      top: element.scrollTop,
    }));
    for (const lane of await page.locator(".comparison-lane").all()) {
      await expect
        .poll(() =>
          lane.evaluate((element) => ({ left: element.scrollLeft, top: element.scrollTop })),
        )
        .toEqual(returned);
    }
  }
  await expect(page.getByLabel("Zoom level")).toHaveText("100%");
  expect(await page.locator(".zoom-canvas").evaluate((element) => element.scrollTop)).toBe(0);
  expect(await page.locator(".review-scroll").evaluate((element) => element.scrollTop)).toBe(0);
  expect(await page.evaluate(() => window.scrollY)).toBe(0);
});

test("side by side includes a third lane only when a diff is available", async ({
  page,
  isMobile,
}, info) => {
  await page.goto("/suites/workspace-overview/images/overview__desktop.png?mode=side");
  await expect(page.locator(".comparison-lane")).toHaveCount(3);
  await expect(page.locator(".suite-panel")).toHaveCount(isMobile ? 0 : 1);
  await expect(page.locator(".review-details")).toHaveCount(0);
  await expect(page.locator(".canvas-labels")).toHaveCount(0);
  await expect(
    page.getByRole("img", { name: "Diff: Workspace overview / Desktop", exact: true }),
  ).toBeVisible();
  const lanes = await page.locator(".comparison-lane").evaluateAll((elements) =>
    elements.map((element) => {
      const bounds = element.getBoundingClientRect();
      return { x: bounds.x, y: bounds.y, width: bounds.width };
    }),
  );
  expect(lanes[0]?.y).toBe(lanes[2]?.y);
  expect(lanes[0]?.width).toBeCloseTo(lanes[2]?.width || 0, 0);
  expect(lanes[0]?.x).toBeLessThan(lanes[1]?.x || 0);
  expect(lanes[1]?.x).toBeLessThan(lanes[2]?.x || 0);
  await expect
    .poll(() =>
      page.evaluate(() => {
        const canvas = document.querySelector(".zoom-canvas")?.getBoundingClientRect();
        const lane = document.querySelector(".comparison-lane")?.getBoundingClientRect();
        return canvas && lane ? Math.abs(lane.y - canvas.y) : Number.POSITIVE_INFINITY;
      }),
    )
    .toBeLessThan(1);
  await screenshot(page, info.project.name, "comparison-three-lanes");
  await page.goto("/suites/customer-accounts/images/overview__desktop.png?mode=side");
  await expect(page.locator(".comparison-lane")).toHaveCount(2);
  await expect(page.locator(".canvas-labels")).toHaveCount(0);
});

for (const shorter of ["reference", "test"] as const) {
  test(`side by side crosshairs follow aligned diff bands (${shorter} shorter)`, async ({
    page,
  }, info) => {
    await page.emulateMedia({ reducedMotion: "reduce" });
    const roles = ["reference", "test", "diff"] as const;
    const gaps = [
      { start: 100, height: 24 },
      { start: 200, height: 24 },
    ];
    const toSourceRow = (alignedRow: number): number | null => {
      let shift = 0;
      for (const gap of gaps) {
        const alignedStart = gap.start + shift;
        if (alignedRow < alignedStart) return alignedRow - shift;
        if (alignedRow < alignedStart + gap.height) return null;
        shift += gap.height;
      }
      return alignedRow - shift;
    };
    const imageSizes = {
      reference: { width: 320, height: shorter === "reference" ? 400 : 448 },
      test: { width: 320, height: shorter === "test" ? 400 : 448 },
      diff: { width: 320, height: 448 },
    };
    const paths = Object.fromEntries(
      roles.map((role) => [role, `_snapshot/images/diff-band-${role}.png?v=${"a".repeat(20)}`]),
    );
    await page.route("**/_snapshot/data/suites/workspace-overview.json?*", async (route) => {
      const response = await route.fetch();
      const report = await response.json();
      Object.assign(report.tests[0], paths, {
        imageSizes,
        alignment: {
          reference: shorter === "reference" ? gaps[0] : null,
          test: shorter === "test" ? gaps[0] : null,
          ...(shorter === "reference" ? { referenceGaps: gaps } : { testGaps: gaps }),
        },
      });
      await route.fulfill({ response, json: report });
    });
    await page.route("**/_snapshot/images/diff-band-*.png?*", async (route) => {
      const role = roles.find((role) => route.request().url().includes(`diff-band-${role}.png`));
      if (!role) throw new Error("Unexpected aligned diff fixture image");
      const image = new PNG(imageSizes[role]);
      image.data.fill(255);
      await route.fulfill({ contentType: "image/png", body: PNG.sync.write(image) });
    });
    await page.goto("/suites/workspace-overview/images/overview__desktop.png?mode=side");
    const lanes = page.locator(".comparison-lane");
    const images = lanes.locator("img");
    await expect(images).toHaveCount(3);
    await expect
      .poll(() =>
        images.evaluateAll((elements) =>
          elements.every((image) => (image as HTMLImageElement).naturalWidth > 0),
        ),
      )
      .toBe(true);
    for (const zoomIn of [false, true]) {
      if (zoomIn) await page.getByRole("button", { name: "Zoom in", exact: true }).click();
      for (const [sourceIndex, source] of roles.entries()) {
        for (const row of [50, 112, 160, 236, 300]) {
          const shorterRow = toSourceRow(row);
          const inBand = shorterRow === null;
          const sourceRow = source === shorter ? shorterRow : row;
          if (sourceRow === null) continue;
          await lanes.nth(sourceIndex).hover();
          const bounds = await images.nth(sourceIndex).boundingBox();
          if (!bounds) throw new Error("Aligned comparison image is not visible");
          await page.mouse.move(
            bounds.x + bounds.width / 2,
            bounds.y + (sourceRow / imageSizes[source].height) * bounds.height,
          );
          await expect(page.locator(".comparison-crosshair")).toHaveCount(inBand ? 2 : 3);
          for (const [targetIndex, target] of roles.entries()) {
            const guide = lanes.nth(targetIndex).locator(".crosshair-horizontal");
            const targetRow = target === shorter ? shorterRow : row;
            if (targetRow === null) {
              await expect(guide).toHaveCount(0);
              continue;
            }
            await expect
              .poll(() =>
                lanes.nth(targetIndex).evaluate((lane, row) => {
                  const image = lane.querySelector("img");
                  const guide = lane.querySelector(".crosshair-horizontal");
                  if (!image || !guide) return Infinity;
                  const bounds = image.getBoundingClientRect();
                  const crosshair = guide.getBoundingClientRect();
                  return Math.abs(
                    crosshair.y +
                      crosshair.height / 2 -
                      bounds.y -
                      (row / image.naturalHeight) * bounds.height,
                  );
                }, targetRow),
              )
              .toBeLessThan(1);
          }
        }
      }
    }
    await screenshot(page, info.project.name, `comparison-aligned-crosshairs-${shorter}`);
  });
}

for (const [differentSizes, hasDiff] of [
  [false, true],
  [true, true],
  [false, false],
  [true, false],
]) {
  test(`side by side crosshairs track image pixels (${differentSizes ? "different" : "equal"} sizes, ${hasDiff ? "with" : "without"} diff)`, async ({
    page,
  }, info) => {
    const laneCount = hasDiff ? 3 : 2;
    if (differentSizes || !hasDiff) {
      await page.route("**/_snapshot/data/suites/**", async (route) => {
        const response = await route.fetch();
        const data = await response.json();
        if (data.id === "workspace-overview") {
          if (differentSizes) data.tests[0].reference = data.tests[1].reference;
          if (!hasDiff) data.tests[0].diff = null;
        }
        await route.fulfill({ response, json: data });
      });
    }
    await page.goto("/suites/workspace-overview/images/overview__desktop.png?mode=side");
    const images = page.locator(".comparison-lane img");
    const crosshairs = page.locator(".comparison-crosshair");
    await expect(images).toHaveCount(laneCount);
    await expect
      .poll(() =>
        images.evaluateAll((elements) =>
          elements.every((element) => (element as HTMLImageElement).naturalWidth > 0),
        ),
      )
      .toBe(true);
    const commonSize = await images.evaluateAll((elements) => ({
      width: Math.min(...elements.map((element) => (element as HTMLImageElement).naturalWidth)),
      height: Math.min(...elements.map((element) => (element as HTMLImageElement).naturalHeight)),
    }));
    const hoverPixel = async (index: number, horizontal: number, vertical: number) => {
      const image = images.nth(index);
      const pane = page.locator(".comparison-lane").nth(index);
      await image.evaluate(
        (element, position) => {
          const scroll = element.closest<HTMLElement>(".image-pane");
          if (!scroll) throw new Error("Image viewport is unavailable");
          const image = element.getBoundingClientRect();
          const bounds = scroll.getBoundingClientRect();
          scroll.scrollLeft +=
            image.left +
            image.width * position.horizontal -
            bounds.left -
            scroll.clientLeft -
            scroll.clientWidth / 2;
          scroll.scrollTop +=
            image.top +
            image.height * position.vertical -
            bounds.top -
            scroll.clientTop -
            scroll.clientHeight / 2;
        },
        { horizontal, vertical },
      );
      const position = await image.evaluate(
        (element, point) => {
          const image = element.getBoundingClientRect();
          const pane = element.closest<HTMLElement>(".image-pane");
          if (!pane) throw new Error("Image viewport is unavailable");
          const bounds = pane.getBoundingClientRect();
          return {
            x: image.left + image.width * point.horizontal - bounds.left - pane.clientLeft,
            y: image.top + image.height * point.vertical - bounds.top - pane.clientTop,
          };
        },
        { horizontal, vertical },
      );
      await pane.hover({ position });
    };
    await expect(crosshairs).toHaveCount(0);
    for (const zoomIn of [false, true]) {
      if (zoomIn) await page.getByRole("button", { name: "Zoom in", exact: true }).click();
      for (const [index, image] of (await images.all()).entries()) {
        const { horizontal, vertical } = await image.evaluate(
          (element, position) => ({
            horizontal: position.x / (element as HTMLImageElement).naturalWidth,
            vertical: position.y / (element as HTMLImageElement).naturalHeight,
          }),
          { x: commonSize.width * (0.3 + index * 0.1), y: commonSize.height * (0.2 + index * 0.1) },
        );
        await hoverPixel(index, horizontal, vertical);
        await expect(crosshairs).toHaveCount(laneCount);
        await expect
          .poll(() =>
            page.locator(".comparison-lane").evaluateAll(
              (lanes, position) => {
                const source = lanes[position.index]?.querySelector("img");
                if (!source) return Number.POSITIVE_INFINITY;
                return Math.max(
                  ...lanes.map((lane) => {
                    const target = lane.querySelector("img");
                    const horizontalGuide = lane
                      .querySelector(".crosshair-horizontal")
                      ?.getBoundingClientRect();
                    const verticalGuide = lane
                      .querySelector(".crosshair-vertical")
                      ?.getBoundingClientRect();
                    if (!target || !horizontalGuide || !verticalGuide)
                      return Number.POSITIVE_INFINITY;
                    const bounds = target.getBoundingClientRect();
                    return Math.max(
                      Math.abs(
                        verticalGuide.x +
                          verticalGuide.width / 2 -
                          bounds.x -
                          (source.naturalWidth * position.horizontal * bounds.width) /
                            target.naturalWidth,
                      ),
                      Math.abs(
                        horizontalGuide.y +
                          horizontalGuide.height / 2 -
                          bounds.y -
                          (source.naturalHeight * position.vertical * bounds.height) /
                            target.naturalHeight,
                      ),
                    );
                  }),
                );
              },
              { index, horizontal, vertical },
            ),
          )
          .toBeLessThan(1);
        for (const guide of await crosshairs.all()) {
          await expect(guide).toHaveAttribute("aria-hidden", "true");
          await expect(guide).toHaveCSS("pointer-events", "none");
          expect((await guide.locator(".crosshair-horizontal").boundingBox())?.height).toBeCloseTo(
            1,
          );
          expect((await guide.locator(".crosshair-vertical").boundingBox())?.width).toBeCloseTo(1);
        }
      }
    }
    await screenshot(
      page,
      info.project.name,
      `comparison-crosshairs${differentSizes ? "-different-sizes" : ""}${hasDiff ? "" : "-without-diff"}`,
    );
    if (differentSizes) {
      await hoverPixel(1, 0.9, 0.2);
      await expect(crosshairs).toHaveCount(laneCount);
      await expect(
        page.locator(".comparison-lane").first().locator(".comparison-crosshair"),
      ).toHaveCount(1);
    }
    await page.getByRole("button", { name: "Fit images", exact: true }).click();
    const firstLane = await page.locator(".comparison-lane").first().boundingBox();
    const secondLane = await page.locator(".comparison-lane").nth(1).boundingBox();
    if (!firstLane || !secondLane) throw new Error("Comparison lanes are not visible");
    await images.first().locator("..").hover();
    await expect(crosshairs).toHaveCount(laneCount);
    await page.mouse.move((firstLane.x + firstLane.width + secondLane.x) / 2, firstLane.y + 10);
    await expect(crosshairs).toHaveCount(0);
    await page.getByRole("button", { name: "Fit images", exact: true }).hover();
    await expect(crosshairs).toHaveCount(0);
    await page.goto("/suites/customer-accounts/images/overview__desktop.png?mode=side");
    await expect(images).toHaveCount(2);
    await images.first().locator("..").hover();
    await expect(crosshairs).toHaveCount(2);
  });
}

for (const hasDiff of [false, true]) {
  test(`side by side crosshairs cover empty space in ${hasDiff ? "three" : "two"} full lanes`, async ({
    page,
  }, info) => {
    await page.emulateMedia({ reducedMotion: "reduce" });
    await mockComparisonImages(
      page,
      {
        reference: { width: 60, height: 100 },
        test: { width: 90, height: 50 },
        diff: { width: 75, height: 80 },
      },
      hasDiff,
    );
    await page.goto("/suites/workspace-overview/images/overview__desktop.png?mode=side");
    const lanes = page.locator(".comparison-lane");
    await expect(lanes).toHaveCount(hasDiff ? 3 : 2);
    await expect
      .poll(() =>
        lanes
          .locator("img")
          .evaluateAll((images) =>
            images.every((image) => (image as HTMLImageElement).naturalWidth > 0),
          ),
      )
      .toBe(true);
    await page.getByRole("button", { name: "Fit to window", exact: true }).click();
    for (const lane of await lanes.all()) {
      const pointer = await lane.evaluate((element) => {
        const bounds = element.getBoundingClientRect();
        return {
          x: Math.round(bounds.left + element.clientLeft + element.clientWidth / 2),
          y: Math.floor(bounds.top + element.clientTop + element.clientHeight - 12),
          imageBottom: element.querySelector("img")?.getBoundingClientRect().bottom ?? Infinity,
          width: element.clientWidth,
          height: element.clientHeight,
        };
      });
      expect(pointer.y).toBeGreaterThan(pointer.imageBottom);
      await page.mouse.move(pointer.x, pointer.y);
      await expect(page.locator(".comparison-crosshair")).toHaveCount(hasDiff ? 3 : 2);
      const horizontal = await lane.locator(".crosshair-horizontal").boundingBox();
      const vertical = await lane.locator(".crosshair-vertical").boundingBox();
      if (!horizontal || !vertical) throw new Error("Lane crosshairs are unavailable");
      expect(Math.abs(horizontal.y + horizontal.height / 2 - pointer.y)).toBeLessThan(1);
      expect(Math.abs(vertical.x + vertical.width / 2 - pointer.x)).toBeLessThan(1);
      expect(horizontal.width).toBe(pointer.width);
      expect(vertical.height).toBe(pointer.height);
      expect(horizontal.height).toBe(1);
      expect(vertical.width).toBe(1);
    }
    await screenshot(
      page,
      info.project.name,
      `comparison-lane-crosshairs-${hasDiff ? "three" : "two"}`,
    );
    const padding = await lanes.first().evaluate((element) => {
      const bounds = element.getBoundingClientRect();
      const image = element.querySelector("img")?.getBoundingClientRect();
      if (!image) throw new Error("Reference image is unavailable");
      return {
        x: Math.ceil(bounds.left + element.clientLeft + 3),
        y: Math.round(image.top + image.height / 2),
        imageLeft: image.left,
      };
    });
    expect(padding.x).toBeLessThan(padding.imageLeft);
    await page.mouse.move(padding.x, padding.y);
    await expect(lanes.first().locator(".comparison-crosshair")).toHaveCount(1);
    const zoom = await page.getByLabel("Zoom level").textContent();
    await page.keyboard.down("Control");
    try {
      await page.mouse.wheel(0, -100);
      await expect(page.getByLabel("Zoom level")).not.toHaveText(zoom ?? "");
      await expect(lanes.first().locator(".comparison-crosshair")).toHaveCount(1);
    } finally {
      await page.keyboard.up("Control");
    }
    await lanes.first().focus();
    await lanes.first().press("1");
    for (let step = 0; step < 11; step++) await lanes.first().press("+");
    await expect(page.getByLabel("Zoom level")).toHaveText("800%");
    await lanes.first().press("ArrowDown");
    await expect
      .poll(() => lanes.first().evaluate((element) => element.scrollTop))
      .toBeGreaterThan(0);
    await expect(page.locator(".comparison-crosshair")).toHaveCount(hasDiff ? 3 : 2);
    await lanes.first().press("ArrowRight");
    await expect
      .poll(() => lanes.first().evaluate((element) => element.scrollLeft))
      .toBeGreaterThan(0);
    await expect(lanes.first().locator(".comparison-crosshair")).toHaveCount(1);
    const guide = await lanes.first().locator(".crosshair-horizontal").boundingBox();
    if (!guide) throw new Error("Crosshair disappeared after scrolling");
    expect(Math.abs(guide.y + guide.height / 2 - padding.y)).toBeLessThan(1);
    await lanes.first().press("0");
    await expect(lanes.first().locator(".comparison-crosshair")).toHaveCount(1);
    await page.getByRole("button", { name: "Fit images", exact: true }).hover();
    await expect(page.locator(".comparison-crosshair")).toHaveCount(0);
  });
}

test("side by side crosshairs clear for view changes, touch, and canceled pointers", async ({
  page,
  isMobile,
}) => {
  await page.goto("/suites/workspace-overview/images/overview__desktop.png?mode=side");
  const image = page.locator(".comparison-image").first();
  const crosshairs = page.locator(".comparison-crosshair");
  for (const event of ["pointerdown", "pointercancel", "pointermove"] as const) {
    await image.hover();
    await expect(crosshairs).toHaveCount(3);
    await image.dispatchEvent(event, { pointerType: "touch" });
    await expect(crosshairs).toHaveCount(0);
  }
  await image.hover();
  await expect(crosshairs).toHaveCount(3);
  await page.evaluate(() => window.dispatchEvent(new Event("blur")));
  await expect(crosshairs).toHaveCount(0);
  for (const [mode, label] of [
    ["wipe", "Wipe"],
    ["side", "Side by side"],
  ]) {
    if (isMobile)
      await page.getByRole("combobox", { name: "Comparison mode" }).selectOption(mode || "side");
    else await page.getByRole("button", { name: label, exact: true }).click();
    await expect(crosshairs).toHaveCount(0);
  }
  await image.hover();
  await expect(crosshairs).toHaveCount(3);
  await page.getByRole("link", { name: "Next image", exact: true }).click();
  await expect(page).toHaveURL(/recent-orders__mobile.png/);
  await expect(crosshairs).toHaveCount(0);
});

for (const missing of [true, false]) {
  test(`side by side crosshairs exclude ${missing ? "missing" : "unavailable"} images`, async ({
    page,
  }) => {
    await page.route("**/_snapshot/data/suites/**", async (route) => {
      const response = await route.fetch();
      const data = await response.json();
      if (data.id === "workspace-overview") {
        if (missing) data.tests[0].reference = null;
        else await page.route(`**/${data.tests[0].reference}`, (image) => image.abort());
      }
      await route.fulfill({ response, json: data });
    });
    await page.goto("/suites/workspace-overview/images/overview__desktop.png?mode=side");
    await expect(
      page.getByText(missing ? "No image" : "Image unavailable", { exact: true }),
    ).toBeVisible();
    const lanes = page.locator(".comparison-image");
    await lanes.nth(1).hover();
    await expect(page.locator(".comparison-crosshair")).toHaveCount(2);
    await expect(lanes.first().locator(".comparison-crosshair")).toHaveCount(0);
    await lanes.first().hover();
    await expect(page.locator(".comparison-crosshair")).toHaveCount(0);
  });
}

test("comparison images are centered without labels and oversized images stay reachable", async ({
  page,
  isMobile,
}, info) => {
  await page.route("**/_snapshot/data/suites/**", async (route) => {
    const response = await route.fetch();
    const data = await response.json();
    if (data.id === "workspace-overview") data.tests[0].reference = data.tests[1].reference;
    await route.fulfill({ response, json: data });
  });
  await page.goto("/suites/workspace-overview/images/overview__desktop.png?mode=side");
  await expect(page.locator(".comparison-lane img")).toHaveCount(3);
  await expect
    .poll(() =>
      page
        .locator(".comparison-lane img")
        .evaluateAll((images) =>
          images.every((image) => (image as HTMLImageElement).naturalWidth > 0),
        ),
    )
    .toBe(true);
  for (const [mode, label] of [
    ["side", "Side by side"],
    ["reference", "Reference"],
    ["test", "Test"],
    ["diff", "Diff"],
    ["wipe", "Wipe"],
    ["overlay", "Overlay"],
  ]) {
    if (isMobile)
      await page.getByRole("combobox", { name: "Comparison mode" }).selectOption(mode || "side");
    else await page.getByRole("button", { name: label, exact: true }).click();
    await expect(page.locator(`.zoom-canvas.mode-${mode}`)).toBeVisible();
    const zoomOut = page.getByRole("button", { name: "Zoom out", exact: true });
    if (await zoomOut.isEnabled()) await zoomOut.click();
    else {
      await expect(zoomOut).toBeDisabled();
      expect(
        Number.parseFloat((await page.getByLabel("Zoom level").textContent()) || "NaN"),
      ).toBeLessThanOrEqual(25);
    }
    await expect(page.locator(".canvas-labels")).toHaveCount(0);
    await expect
      .poll(() =>
        page.locator(".zoom-canvas").evaluate((canvas) => {
          const bounds = canvas.getBoundingClientRect();
          const parent = canvas.closest(".comparison-viewer")?.getBoundingClientRect();
          return parent
            ? Math.abs(bounds.left + bounds.width / 2 - parent.left - parent.width / 2)
            : Number.POSITIVE_INFINITY;
        }),
      )
      .toBeLessThan(1);
    if (mode === "side") {
      const layout = await page.locator(".comparison-viewer").evaluate((viewer) => {
        const lanes = Array.from(viewer.querySelectorAll(".comparison-lane"), (lane) =>
          lane.getBoundingClientRect(),
        );
        return {
          available: viewer.clientWidth,
          span: (lanes.at(-1)?.right ?? 0) - (lanes[0]?.left ?? 0),
          widthDifference:
            Math.max(...lanes.map((lane) => lane.width)) -
            Math.min(...lanes.map((lane) => lane.width)),
        };
      });
      expect(Math.abs(layout.span - layout.available)).toBeLessThan(1);
      expect(layout.widthDifference).toBeLessThan(1);
      for (const lane of await page.locator(".comparison-lane").all()) {
        const geometry = await lane.evaluate((element) => {
          const bounds = element.getBoundingClientRect();
          const surface = element.querySelector(".image-surface")?.getBoundingClientRect();
          const image = element.querySelector("img")?.getBoundingClientRect();
          if (!surface || !image) throw new Error("Comparison canvas is unavailable");
          return {
            center: Math.abs(
              surface.left +
                surface.width / 2 -
                bounds.left -
                element.clientLeft -
                element.clientWidth / 2,
            ),
            imageLeft: image.left - surface.left,
            imageTop: image.top - surface.top,
          };
        });
        expect(geometry.center).toBeLessThan(1);
        expect(geometry.imageLeft).toBe(0);
        expect(geometry.imageTop).toBe(0);
      }
      await screenshot(page, info.project.name, "comparison-centered");
    }
  }
  if (isMobile) await page.getByRole("combobox", { name: "Comparison mode" }).selectOption("side");
  else await page.getByRole("button", { name: "Side by side", exact: true }).click();
  await page.getByRole("button", { name: "Actual size", exact: true }).click();
  const edges = await page
    .locator(".comparison-lane")
    .first()
    .evaluate((pane) => ({
      image: pane.querySelector(".image-surface")?.getBoundingClientRect().left,
      viewer: pane.getBoundingClientRect().left + pane.clientLeft,
    }));
  expect(edges.image).toBe(edges.viewer);
});

test("empty results, missing images, errors, and unknown routes are explicit", async ({
  page,
  isMobile,
}, info) => {
  await page.goto("/");
  await page.getByRole("searchbox", { name: "Search suites" }).fill("no-such-suite");
  await expect(page.getByRole("heading", { name: "No matching suites" })).toBeVisible();
  await page.goto("/suites/empty-suite");
  await expect(page.getByRole("heading", { name: "No matching captures" })).toBeVisible();
  await page.goto("/suites/product-catalog/images/full-page__mobile.png");
  await page.getByRole("button", { name: "Capture information", exact: true }).click();
  await expect(page.getByRole("alert")).toContainText("Navigation timed out");
  await page.keyboard.press("Escape");
  await expect(page.getByText("No image", { exact: true })).toBeVisible();
  if (!isMobile)
    await expect(page.getByRole("button", { name: "Wipe", exact: true })).toBeDisabled();
  await screenshot(page, info.project.name, "comparison-error");
  await page.goto("/suites/missing");
  await expect(page.getByRole("heading", { name: "Suite not found" })).toBeVisible();
  await page.goto("/suites/workspace-overview/images/missing");
  await expect(page.getByRole("heading", { name: "Image not found" })).toBeVisible();
});

test("invalid report data fails visibly and can be retried", async ({ page, isMobile }) => {
  await page.route("**/_snapshot/**", async (route) => {
    if (new URL(route.request().url()).pathname.endsWith(".json"))
      await route.fulfill({ contentType: "application/json", body: '{"version":99}' });
    else await route.continue();
  });
  await page.goto("/");
  await expect(page.getByRole("heading", { name: "Snapshot unavailable" })).toBeVisible();
  await page.unroute("**/_snapshot/**");
  await page.getByRole("button", { name: "Retry", exact: true }).click();
  await expect(page.getByRole("heading", { name: "Snapshot Overview", exact: true })).toBeVisible();

  let suiteRequests = 0;
  let failSuite = true;
  await page.route("**/_snapshot/data/suites/workspace-overview.json*", async (route) => {
    suiteRequests += 1;
    if (failSuite) {
      await route.fulfill({ status: 500, body: "unavailable" });
    } else {
      await route.continue();
    }
  });
  await page.getByRole("link", { name: "workspace-overview", exact: true }).click();
  await expect(page.getByRole("heading", { name: "Suite unavailable" })).toBeVisible();
  expect(suiteRequests).toBe(1);
  failSuite = false;
  await page.locator("#main").getByRole("button", { name: "Retry", exact: true }).click();
  await expect(
    page.getByRole("heading", { name: "workspace-overview", exact: true }),
  ).toBeVisible();
  if (!isMobile) {
    await expect(page.locator("#suite-panel").getByRole("alert")).toHaveCount(0);
  }
  expect(suiteRequests).toBe(2);
});

test("untrusted Markdown is not executable markup or a remote image request", async ({ page }) => {
  await page.route("**/_snapshot/**", async (route) => {
    if (!new URL(route.request().url()).pathname.endsWith(".json")) return route.continue();
    const response = await route.fetch();
    const data = await response.json();
    if (data.version === 2)
      data.subheading =
        '<img src="https://untrusted.invalid/leak" onerror="window.compromised=true"> [Unsafe](javascript:alert(1)) **Safe text**';
    await route.fulfill({ response, json: data });
  });
  await page.goto("/");
  await expect(page.locator(".report-subheading")).toContainText("Safe text");
  await expect(page.locator(".report-subheading img")).toHaveCount(0);
  await expect(page.locator('a[href^="javascript:"]')).toHaveCount(0);
  expect(await page.evaluate(() => "compromised" in window)).toBe(false);
});

test("empty snapshots and very long report text fit narrow and wide screens", async ({ page }) => {
  let reportText = {
    heading: "LongUnbrokenReportHeading".repeat(8),
    subheading: `**Release** / ${"VeryLongBuildIdentifier".repeat(8)}`,
  };
  await page.route("**/_snapshot/**", async (route) => {
    if (!new URL(route.request().url()).pathname.endsWith(".json")) return route.continue();
    const response = await route.fetch();
    const data = await response.json();
    if (data.version === 2) {
      data.heading = reportText.heading;
      data.subheading = reportText.subheading;
      data.suites = [];
    }
    await route.fulfill({ response, json: data });
  });
  await page.goto("/");
  await expect(page.getByRole("heading", { name: "No comparisons yet" })).toBeVisible();
  for (const width of [320, 768, 1920]) {
    await page.setViewportSize({ width, height: 900 });
    await assertLayout(page);
    const header = page.locator(".app-header");
    const headerBottom = await header.evaluate((element) => element.getBoundingClientRect().bottom);
    await expect(page.locator(".breadcrumb-bar")).toHaveCount(0);
    expect(
      await page
        .locator(".overview-page")
        .evaluate((element) => element.getBoundingClientRect().top),
    ).toBeGreaterThanOrEqual(headerBottom);
    expect(
      await header
        .locator(".report-identity")
        .evaluate((element) => element.scrollWidth <= element.clientWidth),
    ).toBe(true);
  }
  for (reportText of [
    { heading: "", subheading: "" },
    { heading: "Release", subheading: "" },
    { heading: "", subheading: "**Staging**" },
  ]) {
    await page.reload();
    await expect(
      page.getByRole("heading", { name: "Snapshot Overview", exact: true }),
    ).toBeVisible();
    await expect(page.locator(".app-header .report-title")).toHaveCount(reportText.heading ? 1 : 0);
    await expect(page.locator(".app-header .report-subheading")).toHaveCount(
      reportText.subheading ? 1 : 0,
    );
    await assertLayout(page);
  }
});

test("every snapshot payload has a content version on a stable path", async ({ page }) => {
  const requests = new Set<string>();
  page.on("request", (request) => {
    if (new URL(request.url()).pathname.startsWith("/_snapshot/")) requests.add(request.url());
  });
  await page.goto("/suites/workspace-overview/images/overview__desktop.png?mode=side");
  await expect(page.locator(".comparison-lane img")).toHaveCount(3);
  await expect
    .poll(() =>
      page
        .locator(".comparison-lane img")
        .evaluateAll((images) =>
          images.every((image) => (image as HTMLImageElement).naturalWidth > 0),
        ),
    )
    .toBe(true);
  expect([...requests].some((url) => new URL(url).pathname.endsWith("/manifest.json"))).toBe(true);
  expect(
    [...requests].filter((url) => new URL(url).pathname.endsWith(".png")).length,
  ).toBeGreaterThanOrEqual(3);
  for (const resource of requests) {
    const url = new URL(resource);
    expect(url.searchParams.get("v")).toMatch(/^[A-Za-z0-9_-]{20}$/);
    expect(url.pathname).not.toMatch(/[a-f0-9]{64}\.(json|png)$/);
    const response = await page.request.get(resource);
    expect(response.ok()).toBe(true);
    expect(url.searchParams.get("v")).toBe(getAssetVersion(await response.body()));
  }
});

test("report data requests carry same-origin access cookies", async ({ page, baseURL }) => {
  // Cloudflare Access redirects cookie-less requests to a cross-origin login (a CORS error).
  await page
    .context()
    .addCookies([{ name: "CF_Authorization", value: "synthetic-session", url: baseURL }]);
  const requests: Request[] = [];
  // Development StrictMode aborts a duplicate first fetch before it reaches the network.
  page.on("requestfinished", (request) => {
    if (new URL(request.url()).pathname.startsWith("/_snapshot/data/")) requests.push(request);
  });
  await page.goto("/suites/workspace-overview");
  await expect(
    page.getByRole("heading", { name: "workspace-overview", exact: true }),
  ).toBeVisible();
  expect(requests.map((request) => new URL(request.url()).pathname)).toEqual(
    expect.arrayContaining([
      "/_snapshot/data/manifest.json",
      "/_snapshot/data/suites/workspace-overview.json",
    ]),
  );
  for (const request of requests)
    expect((await request.allHeaders()).cookie).toContain("CF_Authorization=synthetic-session");
});

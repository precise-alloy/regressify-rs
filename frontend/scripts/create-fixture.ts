import { existsSync } from "node:fs";
import { mkdir, readFile, rm, writeFile } from "node:fs/promises";
import { dirname, join, resolve } from "node:path";
import { chromium } from "@playwright/test";
import pixelmatch from "pixelmatch";
import { PNG } from "pngjs";
import {
  type Capture,
  encodeArtifactPath,
  type Manifest,
  manifestSchema,
  type SuiteReport,
  suiteReportSchema,
} from "../src/data/report";
import { getAssetVersion } from "./version-assets";

export const fixtureRoot = resolve(import.meta.dir, "../.fixtures");

export function browserExecutable(): string | undefined {
  return (
    process.env.PLAYWRIGHT_CHROMIUM_EXECUTABLE_PATH ||
    [
      "C:/Program Files/Google/Chrome/Application/chrome.exe",
      "C:/Program Files (x86)/Microsoft/Edge/Application/msedge.exe",
      "/usr/bin/chromium",
      "/usr/bin/google-chrome",
    ].find(existsSync)
  );
}

async function artifact(path: string, bytes: Uint8Array) {
  await mkdir(dirname(join(fixtureRoot, path)), { recursive: true });
  await writeFile(join(fixtureRoot, path), bytes);
  return `${encodeArtifactPath(path)}?v=${getAssetVersion(bytes)}`;
}

export async function createFixture() {
  await rm(join(fixtureRoot, "_snapshot"), { recursive: true, force: true });
  const html = await readFile(resolve(import.meta.dir, "../tests/fixtures/dashboard.html"), "utf8");
  const errorIconBytes = await readFile(
    resolve(import.meta.dir, "../../src/test/data/error-icon.png"),
  );
  const browser = await chromium.launch({ executablePath: browserExecutable(), headless: true });
  let errorThumbnailUrl = "";
  const captures: {
    reference: string;
    test: string;
    diff: string;
    mismatch: number;
    bytes: Buffer[];
    imageSizes: NonNullable<Capture["imageSizes"]>;
    previews: {
      reference: string;
      test: string;
      diff: string;
      gridReference: string;
      gridTest: string;
      gridDiff: string;
      gridError: string;
    };
  }[] = [];
  try {
    for (const width of [1080, 600]) {
      const page = await browser.newPage({
        viewport: { width, height: 760 },
        deviceScaleFactor: 1,
      });
      await page.setContent(html);
      const reference = await page.screenshot({ fullPage: true });
      await page.evaluate(() => document.body.classList.add("changed"));
      const changed = await page.screenshot({ fullPage: true });
      const before = PNG.sync.read(reference);
      const after = PNG.sync.read(changed);
      const difference = new PNG({ width: before.width, height: before.height });
      const pixels = pixelmatch(
        before.data,
        after.data,
        difference.data,
        before.width,
        before.height,
        { threshold: 0.1 },
      );
      const diff = PNG.sync.write(difference);
      const { previews, errorGrid } = await page.evaluate(
        async ({ sources, iconSource }) => {
          const results = [];
          for (const [sourceIndex, source] of sources.entries()) {
            const image = new Image();
            image.src = `data:image/png;base64,${source}`;
            await image.decode();
            const render = (grid: boolean, cropDiff: boolean) => {
              const width = image.width;
              const height = grid
                ? Math.min(image.height, Math.max(1, Math.floor((image.width * 5) / 8)))
                : image.height;
              let cropY = 0;
              if (grid && cropDiff && image.height > height) {
                const probe = document.createElement("canvas");
                probe.width = image.width;
                probe.height = image.height;
                const probeCtx = probe.getContext("2d");
                if (!probeCtx) throw new Error("Preview canvas is unavailable");
                probeCtx.drawImage(image, 0, 0);
                const { data } = probeCtx.getImageData(0, 0, image.width, image.height);
                for (let offset = 0; offset < data.length; offset += 4) {
                  if (
                    (data[offset + 3] ?? 0) > 0 &&
                    data[offset] === 255 &&
                    data[offset + 1] === 0 &&
                    (data[offset + 2] === 255 || data[offset + 2] === 0)
                  ) {
                    const row = Math.floor(offset / 4 / image.width);
                    cropY = Math.min(
                      image.height - height,
                      Math.max(0, row - Math.floor(height / 8)),
                    );
                    break;
                  }
                }
              }
              const scale = Math.min(1, 640 / width, (grid ? 400 : 480) / height);
              const canvas = document.createElement("canvas");
              canvas.width = Math.max(1, Math.round(width * scale));
              canvas.height = Math.max(1, Math.round(height * scale));
              const context = canvas.getContext("2d");
              if (!context) throw new Error("Preview canvas is unavailable");
              context.drawImage(image, 0, cropY, width, height, 0, 0, canvas.width, canvas.height);
              return canvas.toDataURL("image/png").split(",")[1] || "";
            };
            results.push({ preview: render(false, false), grid: render(true, sourceIndex === 2) });
          }
          const icon = new Image();
          icon.src = `data:image/png;base64,${iconSource}`;
          await icon.decode();
          const iconScale = Math.min(1, 640 / icon.width, 400 / icon.height);
          const iconCanvas = document.createElement("canvas");
          iconCanvas.width = Math.max(1, Math.round(icon.width * iconScale));
          iconCanvas.height = Math.max(1, Math.round(icon.height * iconScale));
          const iconCtx = iconCanvas.getContext("2d");
          if (!iconCtx) throw new Error("Preview canvas is unavailable");
          iconCtx.drawImage(
            icon,
            0,
            0,
            icon.width,
            icon.height,
            0,
            0,
            iconCanvas.width,
            iconCanvas.height,
          );
          return {
            previews: results,
            errorGrid: iconCanvas.toDataURL("image/png").split(",")[1] || "",
          };
        },
        {
          sources: [reference, changed, diff].map((bytes) => bytes.toString("base64")),
          iconSource: errorIconBytes.toString("base64"),
        },
      );
      if (!errorThumbnailUrl) {
        errorThumbnailUrl = await artifact(
          "_snapshot/images/preview/error-thumbnail.png",
          Buffer.from(errorGrid, "base64"),
        );
      }
      const previewUrls = [];
      for (const [index, role] of ["reference", "test", "diff"].entries()) {
        const preview = previews[index];
        if (!preview) throw new Error("Fixture preview is missing");
        previewUrls.push({
          preview: await artifact(
            `_snapshot/images/preview/${role}-preview-${width}.png`,
            Buffer.from(preview.preview, "base64"),
          ),
          grid: await artifact(
            `_snapshot/images/preview/${role}-thumbnail-${width}.png`,
            Buffer.from(preview.grid, "base64"),
          ),
        });
      }
      const [referencePreview, testPreview, diffPreview] = previewUrls;
      if (!referencePreview || !testPreview || !diffPreview)
        throw new Error("Fixture previews are missing");
      captures.push({
        reference: await artifact(`_snapshot/images/preview/reference-${width}.png`, reference),
        test: await artifact(`_snapshot/images/preview/test-${width}.png`, changed),
        diff: await artifact(`_snapshot/images/preview/diff-${width}.png`, diff),
        mismatch: (pixels / (before.width * before.height)) * 100,
        bytes: [reference, changed, diff],
        imageSizes: {
          reference: { width: before.width, height: before.height },
          test: { width: after.width, height: after.height },
          diff: { width: difference.width, height: difference.height },
        },
        previews: {
          reference: referencePreview.preview,
          test: testPreview.preview,
          diff: diffPreview.preview,
          gridReference: referencePreview.grid,
          gridTest: testPreview.grid,
          gridDiff: diffPreview.grid,
          gridError: errorThumbnailUrl,
        },
      });
      await page.close();
    }
  } finally {
    await browser.close();
  }
  const manifest: Manifest = {
    version: 2,
    title: "Northstar | Visual review",
    heading: "A clear view of every change.",
    subheading: "**Northstar** / Production baseline compared with staging",
    generatedAt: "2026-09-23T09:42:00Z",
    regressifyVersion: "0.0.0 (fixture)",
    suites: [],
  };
  const names = [
    "workspace-overview",
    "orders-and-checkout",
    "customer-accounts",
    "product-catalog",
    "search-and-navigation",
    "mobile-experience",
    "empty-suite",
  ];
  for (const [suiteIndex, id] of names.entries()) {
    const tests: Capture[] = [];
    const rawTests: object[] = [];
    const suiteDir = join(fixtureRoot, "workspace/.regressify", id);
    await mkdir(join(suiteDir, "html_report"), { recursive: true });
    await mkdir(join(suiteDir, "bitmaps_test/20260923-094200"), { recursive: true });
    await mkdir(join(suiteDir, "bitmaps_reference"), { recursive: true });
    for (let captureIndex = 0; captureIndex < (id === "empty-suite" ? 0 : 4); captureIndex++) {
      const images = captures[captureIndex % 2];
      if (!images) throw new Error("Fixture image missing.");
      const failed = suiteIndex % 3 === 0 || (suiteIndex === 1 && captureIndex === 0);
      const error =
        suiteIndex === 3 && captureIndex === 3
          ? "Navigation timed out after 30 seconds. The page did not reach a stable state."
          : null;
      const urlScenario =
        id === "search-and-navigation"
          ? [
              { path: "/men/running-shoes", name: "Men / Running shoes" },
              { path: "/women/running-shoes", name: "Women / Running shoes" },
              { path: "/help-center", name: "Help center" },
              { path: "/men/running-shoes", name: "Men / Running shoes" },
            ][captureIndex]
          : undefined;
      const referenceUrl = `https://example.com${urlScenario?.path || "/workspace"}`;
      const scenarioId = urlScenario
        ? getAssetVersion(Buffer.from(referenceUrl))
        : `scenario-${captureIndex}`;
      const viewport = captureIndex % 2 ? "mobile" : "desktop";
      const fileName = urlScenario
        ? `${scenarioId}_${viewport}__FINAL.png`
        : `${["overview", "recent-orders", "channel-summary", "full-page"][captureIndex]}__${viewport}.png`;
      const capture: Capture = {
        id: fileName,
        label: urlScenario
          ? `${urlScenario.name} / ${captureIndex % 2 ? "Mobile" : "Desktop"} / Full page`
          : `${["Workspace overview", "Recent orders", "Sales by channel", "Full page"][captureIndex]} / ${captureIndex % 2 ? "Mobile" : "Desktop"}`,
        scenario: urlScenario?.name || `Scenario ${captureIndex + 1}`,
        selector: "document",
        status: failed ? "fail" : "pass",
        threshold: 0.05,
        mismatch: error ? null : failed ? images.mismatch : 0,
        error,
        reference: error ? null : images.reference,
        test: failed ? images.test : images.reference,
        diff: failed && !error ? images.diff : null,
        imageSizes: {
          reference: error ? null : images.imageSizes.reference,
          test: failed ? images.imageSizes.test : images.imageSizes.reference,
          diff: failed && !error ? images.imageSizes.diff : null,
        },
        previews: {
          reference: error ? null : images.previews.reference,
          test: failed ? images.previews.test : images.previews.reference,
          diff: failed && !error ? images.previews.diff : null,
          grid: error
            ? images.previews.gridError
            : failed
              ? images.previews.gridDiff
              : images.previews.gridReference,
        },
        referenceUrl,
        testUrl: referenceUrl.replace("example.com", "staging.example.com"),
        dimensions: { same: true, width: 0, height: 0 },
      };
      tests.push(capture);
      await writeFile(
        join(suiteDir, "bitmaps_reference", fileName),
        images.bytes[0] || new Uint8Array(),
      );
      await writeFile(
        join(suiteDir, "bitmaps_test/20260923-094200", fileName),
        images.bytes[failed ? 1 : 0] || new Uint8Array(),
      );
      const diffName = fileName.replace(".png", "_diff.png");
      if (capture.diff)
        await writeFile(
          join(suiteDir, "bitmaps_test/20260923-094200", diffName),
          images.bytes[2] || new Uint8Array(),
        );
      rawTests.push({
        status: capture.status,
        pair: {
          label: urlScenario ? fileName.replace(/\.png$/, "").replaceAll("_", " ") : capture.label,
          scenarioId,
          scenarioLabel: urlScenario ? undefined : capture.scenario,
          selector: capture.selector,
          fileName,
          reference: error
            ? "../bitmaps_reference/missing.png"
            : `../bitmaps_reference/${fileName}`,
          test: `../bitmaps_test/20260923-094200/${fileName}`,
          ...(capture.diff ? { diffImage: `../bitmaps_test/20260923-094200/${diffName}` } : {}),
          misMatchThreshold: capture.threshold,
          referenceUrl: capture.referenceUrl,
          url: capture.testUrl,
          ...(error
            ? { engineErrorMsg: error }
            : {
                diff: {
                  rawMisMatchPercentage: capture.mismatch,
                  isSameDimensions: true,
                  dimensionDifference: { width: 0, height: 0 },
                },
              }),
        },
      });
    }
    const suite: SuiteReport = suiteReportSchema.parse({ id, runId: "20260923-094200", tests });
    const passed = tests.filter((test) => test.status === "pass").length;
    manifest.suites.push({
      id,
      total: tests.length,
      passed,
      failed: tests.length - passed,
      errors: tests.filter((test) => test.error !== null).length,
      report: await artifact(
        `_snapshot/data/suites/${id}.json`,
        Buffer.from(JSON.stringify(suite)),
      ),
    });
    const raw = JSON.stringify({ id, tests: rawTests });
    await writeFile(join(suiteDir, "html_report/config.js"), `report(${raw});`);
    await writeFile(join(suiteDir, "bitmaps_test/20260923-094200/report.json"), raw);
  }
  manifestSchema.parse(manifest);
  await artifact("_snapshot/data/manifest.json", Buffer.from(JSON.stringify(manifest)));
  await writeFile(
    join(fixtureRoot, "workspace/regressify.yaml"),
    'snapshotTitle: "Northstar | Visual review"\nsnapshotHeading: "A clear view of every change."\nsnapshotSubheading: "**Northstar** / Production baseline compared with staging"\n',
  );
  console.log(
    `Created ${manifest.suites.length} synthetic suites and query-versioned screenshot fixtures.`,
  );
}

if (import.meta.main) await createFixture();

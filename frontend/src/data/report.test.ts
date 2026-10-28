import { describe, expect, test } from "bun:test";
import { getAssetVersion } from "../../scripts/version-assets";
import {
  fetchLogChunk,
  formatLogRecord,
  logChunkSchema,
  logRecordSchema,
  matchesLog,
} from "./logs";
import {
  alignedRowToImageRow,
  artifactPath,
  captureSchema,
  encodeArtifactPath,
  fetchReport,
  filterCaptures,
  formatMismatch,
  imageAlignmentGap,
  imageAlignmentGaps,
  imagePath,
  imageRowToAlignedRow,
  manifestSchema,
  passRate,
  safePageUrl,
  suiteLabel,
  suiteReportSchema,
} from "./report";

const image = `_snapshot/images/shop/test/home.png?v=${"a".repeat(20)}`;
export const capture = captureSchema.parse({
  id: "home__FINAL.png",
  label: "Home, desktop",
  scenario: "Home",
  selector: "document",
  status: "fail",
  threshold: 0.1,
  mismatch: 1.25,
  error: null,
  reference: image,
  test: image,
  diff: image,
  referenceUrl: "https://example.com/",
  testUrl: null,
  dimensions: { same: false, width: -20, height: 30 },
});

describe("snapshot contract", () => {
  test.each([
    [0, "0.00"],
    [-0, "0.00"],
    [0.005, "0.01"],
    [0.01, "0.01"],
    [1.2345, "1.23"],
    [12.3456, "12.35"],
    [100, "100.00"],
    [0.0049999, "0.005"],
    [0.0005, "0.001"],
    [0.0004999, "0.0005"],
    [0.000017922688690066528, "0.00002"],
    [1e-8, "0.00000001"],
    [1e-12, "0.000000000001"],
    [1e-100, "1e-100"],
    [1e-101, "1e-101"],
    [Number.MIN_VALUE, "5e-324"],
  ] as const)("mismatch formatting: %s becomes %s", (mismatch, expected) => {
    expect(formatMismatch(mismatch)).toBe(expected);
  });

  test("positive mismatch formatting never displays zero or changes audit values", () => {
    for (let exponent = -323; exponent <= 2; exponent++) {
      for (const coefficient of [1, 4.9999, 5, 9.9999]) {
        const mismatch = coefficient * 10 ** exponent;
        if (mismatch <= 0 || mismatch > 100) continue;
        const parsed = captureSchema.parse({ ...capture, mismatch });
        const formatted = formatMismatch(mismatch);
        expect(Number(formatted)).toBeGreaterThan(0);
        expect(parsed.mismatch).toBe(mismatch);
        expect(JSON.parse(JSON.stringify(parsed)).mismatch).toBe(mismatch);
        if (Number(mismatch.toFixed(2)) > 0) expect(formatted).toBe(mismatch.toFixed(2));
      }
    }
  });

  test("mismatch pixel counts preserve exact zero and legacy absence", () => {
    expect(capture.mismatchPixels).toBeUndefined();
    for (const mismatchPixels of [0, 1, 64_000_000]) {
      const parsed = captureSchema.parse({ ...capture, mismatchPixels });
      expect(parsed.mismatchPixels).toBe(mismatchPixels);
      expect(JSON.parse(JSON.stringify(parsed)).mismatchPixels).toBe(mismatchPixels);
    }
    for (const mismatchPixels of [-1, 1.5, 64_000_001, Infinity, NaN, "1", null]) {
      expect(captureSchema.safeParse({ ...capture, mismatchPixels }).success).toBe(false);
    }
  });

  test("capture diagnostic schema is typed and excludes provider detail", () => {
    const record = {
      schemaVersion: 1,
      operationId: "11111111-1111-4111-8111-111111111111",
      operationKind: "capture",
      captureRunId: null,
      execution: null,
      sequence: 0,
      timestampUtcMs: 1790812800000,
      elapsedMs: 10,
      severity: "debug",
      stage: "navigation",
      eventName: "decision",
      message: "<img src=x onerror=alert(1)>",
      fields: { statusCode: { type: "number", value: 200 } },
      spanId: "22222222-2222-4222-8222-222222222222",
      parentSpan: null,
      durationMs: 10,
      outcome: "success",
      errorCause: null,
    };
    const parsed = logRecordSchema.parse(record);
    for (const value of [
      "drag",
      "dropFiles",
      "wheel",
      "dialog",
      "key",
      "tap",
      "swipe",
      "longPress",
      "blur",
      "pinch",
      "selectText",
      "paste",
      "closeTab",
      "hidden",
      "detached",
      "disabled",
      "content",
    ]) {
      expect(
        logRecordSchema.safeParse({
          ...record,
          fields: { actionType: { type: "kind", value } },
        }).success,
      ).toBe(true);
    }
    const simplification = logRecordSchema.parse({
      ...record,
      stage: "screenshotSettling",
      fields: {
        condition: { type: "kind", value: "images" },
        simplifiedImages: { type: "number", value: 4 },
        smallImages: { type: "number", value: 3 },
        excludedImages: { type: "number", value: 2 },
        filteredImages: { type: "number", value: 1 },
        transformedImages: { type: "number", value: 0 },
        overlappedImages: { type: "number", value: 5 },
        invalidSelectors: { type: "number", value: 1 },
      },
    });
    expect(formatLogRecord(simplification)).toContain("simplifiedImages=4");
    expect(formatLogRecord(simplification)).toContain("invalidSelectors=1");
    expect(formatLogRecord(parsed)).toContain("statusCode=200");
    expect(matchesLog(parsed, "error", "all", "")).toBe(false);
    expect(matchesLog(parsed, "debug", "navigation", "STATUSCODE")).toBe(true);
    const readable = logRecordSchema.parse({
      ...record,
      execution: {
        executionId: "33333333-3333-4333-8333-333333333333",
        suiteId: "search-functions",
        scenarioId: "keyword-search-button",
        viewport: { name: "desktop", width: 1920, height: 1080 },
        captureRole: "test",
        attempt: 1,
      },
      fields: {
        target: { type: "identity", value: "#hero-search" },
        destination: { type: "identity", value: "https://example.test/search-results/?q=heart" },
        artifact: { type: "identity", value: "common/_search-ready.js" },
      },
    });
    for (const query of [
      "search-functions",
      "keyword-search-button",
      "desktop",
      "#hero-search",
      "common/_search-ready.js",
      "q=heart",
    ]) {
      expect(matchesLog(readable, "debug", "navigation", query)).toBe(true);
    }
    for (const value of [`sha256:${"a".repeat(64)}`, "[data-secret='literal selector']"]) {
      expect(
        logRecordSchema.safeParse({ ...record, fields: { target: { type: "identity", value } } })
          .success,
      ).toBe(true);
    }
    for (const value of [
      "",
      "new\nline",
      "\u202ehidden",
      "x".repeat(2049),
      "\u{1f600}".repeat(513),
    ]) {
      expect(
        logRecordSchema.safeParse({ ...record, fields: { target: { type: "identity", value } } })
          .success,
      ).toBe(false);
    }
    for (const timestampUtcMs of [-8640000000000001, 8640000000000001, Number.MAX_SAFE_INTEGER]) {
      expect(logRecordSchema.safeParse({ ...record, timestampUtcMs }).success).toBe(false);
    }
    for (const timestampUtcMs of [-8640000000000000, 0, 8640000000000000]) {
      const valid = logRecordSchema.parse({ ...record, timestampUtcMs });
      expect(() => new Date(valid.timestampUtcMs).toISOString()).not.toThrow();
    }
    for (const field of ["password", "value", "inputValue"]) {
      expect(
        logRecordSchema.safeParse({
          ...record,
          fields: { [field]: { type: "identity", value: "simulated input" } },
        }).success,
      ).toBe(false);
    }
    expect(logRecordSchema.safeParse({ ...record, operationKind: "deployment" }).success).toBe(
      false,
    );
    for (const operationKind of ["command", "application"]) {
      expect(logRecordSchema.safeParse({ ...record, operationKind }).success).toBe(false);
    }
    expect(
      logRecordSchema.safeParse({
        ...record,
        errorCause: { category: "provider", causes: [], localDetail: "private" },
      }).success,
    ).toBe(false);
    expect(
      logChunkSchema.safeParse({
        version: 1,
        operationId: record.operationId,
        records: [],
      }).success,
    ).toBe(false);
  });
  test("manifest_round_trips_optional_execution_log_fields", () => {
    expect(capture.logs).toBeUndefined();
    const states = ["complete", "partial", "missing", "legacyUnavailable", "loadingError"] as const;
    const parsed = states.map((state) => {
      const log = {
        state,
        operationId: null,
        executionId: null,
        chunks: [],
        records: 0,
        missingRecords: null,
        build: null,
      };
      const value = captureSchema.parse({ ...capture, logs: { reference: null, test: log } });
      expect(value.logs?.test).toEqual(log);
      expect(captureSchema.parse(JSON.parse(JSON.stringify(value)))).toEqual(value);
      return value.logs?.test?.state;
    });
    expect(new Set(parsed).size).toBe(5);
    const operationId = "11111111-1111-4111-8111-111111111111";
    const validChunk = `_snapshot/data/logs/${operationId}/chunk-000000.json?v=${"a".repeat(20)}`;
    for (const invalidLog of [
      {
        state: "complete",
        operationId: null,
        executionId: null,
        chunks: ["https://remote.example.test/private.json"],
        records: 0,
        missingRecords: 0,
        build: null,
      },
      {
        state: "missing",
        operationId,
        executionId: null,
        chunks: [validChunk],
        records: 1,
        missingRecords: null,
        build: null,
      },
      {
        state: "complete",
        operationId,
        executionId: null,
        chunks: [validChunk, validChunk],
        records: 2,
        missingRecords: 0,
        build: null,
      },
      {
        state: "complete",
        operationId,
        executionId: null,
        chunks: [
          `_snapshot/data/logs/22222222-2222-4222-8222-222222222222/chunk-000000.json?v=${"a".repeat(20)}`,
        ],
        records: 1,
        missingRecords: 0,
        build: null,
      },
      {
        state: "complete",
        operationId,
        executionId: null,
        chunks: [validChunk],
        records: 0,
        missingRecords: 0,
        build: null,
      },
    ]) {
      expect(
        captureSchema.safeParse({ ...capture, logs: { reference: null, test: invalidLog } })
          .success,
      ).toBe(false);
    }
  });
  test("fetchLogChunk validates sequence boundaries and respects abort signals", async () => {
    const operationId = "11111111-1111-4111-8111-111111111111";
    const makeRecord = (sequence: number) => ({
      schemaVersion: 1 as const,
      operationId,
      operationKind: "capture" as const,
      captureRunId: null,
      execution: null,
      sequence,
      timestampUtcMs: 0,
      elapsedMs: 1,
      severity: "info" as const,
      stage: "navigation" as const,
      eventName: "decision" as const,
      message: "ok",
      fields: {},
      spanId: "22222222-2222-4222-8222-222222222222",
      parentSpan: null,
      durationMs: 1,
      outcome: "success" as const,
      errorCause: null,
    });
    const payload = JSON.stringify({
      version: 1,
      operationId,
      records: [makeRecord(1)],
    });
    const chunkPath = `_snapshot/data/logs/${operationId}/chunk-000000.json?v=${getAssetVersion(payload)}`;
    const log = {
      state: "complete" as const,
      operationId,
      executionId: null,
      chunks: [chunkPath],
      records: 2,
      missingRecords: 0,
      build: null,
    };
    const aborted = new AbortController();
    aborted.abort();
    await expect(fetchLogChunk(log, 0, aborted.signal)).rejects.toThrow();
    await expect(fetchLogChunk(log, -1)).rejects.toThrow("missing");
    const originalFetch = globalThis.fetch;
    const originalWindow = globalThis.window;
    Object.assign(globalThis, {
      window: { location: { origin: "http://127.0.0.1:4173" } },
      fetch: async () => new Response(payload, { status: 200 }),
    });
    try {
      await expect(fetchLogChunk(log, 0)).rejects.toThrow("sequence");
    } finally {
      globalThis.fetch = originalFetch;
      Object.assign(globalThis, { window: originalWindow });
    }
  });
  test("diff alignment is optional and accepts only one bounded source gap", () => {
    expect(capture.alignment).toBeUndefined();
    const gap = { start: 24, height: 5 };
    const gap2 = { start: 80, height: 10 };
    for (const alignment of [
      { reference: null, test: null },
      { reference: gap, test: null },
      { reference: null, test: gap },
      { reference: gap, test: null, referenceGaps: [gap, gap2] },
      { reference: gap, test: gap2, referenceGaps: [gap], testGaps: [gap2] },
    ]) {
      expect(captureSchema.parse({ ...capture, alignment }).alignment).toEqual(alignment);
    }
    for (const alignment of [
      null,
      {},
      { reference: gap, test: gap },
      { reference: gap, test: null, referenceGaps: [gap, gap] },
      { reference: gap, test: gap, referenceGaps: [gap], testGaps: [gap] },
      ...[
        { start: -1, height: 5 },
        { start: 0.5, height: 5 },
        { start: 24, height: 0 },
        { start: 24, height: -5 },
        { start: 24, height: Infinity },
        { start: 0x100000000, height: 5 },
      ].map((reference) => ({ reference, test: null })),
    ]) {
      expect(captureSchema.safeParse({ ...capture, alignment }).success).toBe(false);
    }
  });
  test("aligned rows map both ways and missing band pixels have no source coordinate", () => {
    const gap = { start: 24, height: 5 };
    const multiGaps = [gap, { start: 80, height: 10 }];
    expect(imageAlignmentGap(gap, 128, 133)).toEqual(gap);
    expect(imageAlignmentGaps(multiGaps, 128, 143)).toEqual(multiGaps);
    expect(imageAlignmentGaps(multiGaps, 128, 133)).toBeNull();
    for (const [height, diffHeight] of [
      [128, 128],
      [128, undefined],
      [20, 25],
    ]) {
      expect(imageAlignmentGap(gap, height || 0, diffHeight)).toBeNull();
    }
    expect(imageAlignmentGap(undefined, 128, 133)).toBeNull();
    for (const row of [0, 23.5, 24, 24.5, 127.5]) {
      const aligned = imageRowToAlignedRow(row, gap);
      expect(aligned).toBe(row < 24 ? row : row + 5);
      expect(alignedRowToImageRow(aligned, gap)).toBe(row);
      expect(imageRowToAlignedRow(row, null)).toBe(row);
      expect(alignedRowToImageRow(row, null)).toBe(row);
    }
    for (const row of [0, 23.5, 24, 79.5, 80, 120]) {
      const aligned = imageRowToAlignedRow(row, multiGaps);
      expect(aligned).toBe(row < 24 ? row : row < 80 ? row + 5 : row + 15);
      expect(alignedRowToImageRow(aligned, multiGaps)).toBe(row);
    }
    for (const row of [24, 24.5, 28.999]) expect(alignedRowToImageRow(row, gap)).toBeNull();
    for (const row of [24, 28.5, 85, 94.5]) expect(alignedRowToImageRow(row, multiGaps)).toBeNull();
    expect(alignedRowToImageRow(29, gap)).toBe(24);
    expect(alignedRowToImageRow(95, multiGaps)).toBe(80);
  });
  test("source image sizes are optional and distinct from signed comparison deltas", () => {
    expect(capture.imageSizes).toBeUndefined();
    const imageSizes = {
      reference: { width: 1, height: 0xffffffff },
      test: { width: 320, height: 6000 },
      diff: null,
    };
    const parsed = captureSchema.parse({ ...capture, imageSizes });
    expect(parsed.imageSizes).toEqual(imageSizes);
    expect(parsed.dimensions).toEqual(capture.dimensions);
    expect(captureSchema.parse(JSON.parse(JSON.stringify(parsed)))).toEqual(parsed);
    for (const role of ["reference", "test", "diff"] as const) {
      for (const axis of ["width", "height"] as const) {
        for (const value of [0, -1, 1.5, Number.NaN, Infinity, 0x100000000, "320", null]) {
          expect(
            captureSchema.safeParse({
              ...capture,
              imageSizes: { ...imageSizes, [role]: { width: 320, height: 480, [axis]: value } },
            }).success,
          ).toBe(false);
        }
      }
      expect(
        captureSchema.safeParse({ ...capture, imageSizes: { ...imageSizes, [role]: null } })
          .success,
      ).toBe(true);
    }
    for (const imageSizes of [null, {}, { test: { width: 320 } }]) {
      expect(captureSchema.safeParse({ ...capture, imageSizes }).success).toBe(false);
    }
  });
  test("previews are optional and every preview URL uses the validated image contract", () => {
    expect(capture.previews).toBeUndefined();
    const previews = { reference: image, test: image, diff: null, grid: image };
    expect(captureSchema.parse({ ...capture, previews }).previews).toEqual(previews);
    for (const role of ["reference", "test", "diff", "grid"]) {
      for (const path of [
        "_snapshot/images/thumb.png",
        "https://example.com/thumb.png",
        `../thumb.png?v=${"a".repeat(20)}`,
      ]) {
        expect(
          captureSchema.safeParse({ ...capture, previews: { ...previews, [role]: path } }).success,
        ).toBe(false);
      }
    }
  });
  test("suite display names are optional presentation data, not route identity", () => {
    for (const displayName of [undefined, "", "  ", "Customer checkout", "<img src=x>"]) {
      const suite = suiteReportSchema.parse({ id: "shop", displayName, runId: "run", tests: [] });
      expect(suiteLabel(suite)).toBe(displayName?.trim() ? displayName : "shop");
      expect(imagePath(suite.id, "home.png")).toBe("/suites/shop/images/home.png");
    }
    expect(
      suiteReportSchema.safeParse({ id: "shop", displayName: 10, runId: "run", tests: [] }).success,
    ).toBe(false);
  });
  test("accepts signed dimension differences and hostile labels as plain data", () => {
    expect(
      captureSchema.parse({ ...capture, label: '<img src=x onerror="alert(1)">' }).label,
    ).toContain("<img");
    expect(capture.dimensions?.width).toBe(-20);
  });
  test("rejects unversioned, remote and traversal images", () => {
    for (const path of [
      "../../state.json",
      "https://example.com/image.png",
      "/images/x.png",
      "_snapshot/images/x.png",
      `${image}?x=1`,
      `_snapshot/images/../private.png?v=${"a".repeat(20)}`,
      `_snapshot/images/%2E%2E/private.png?v=${"a".repeat(20)}`,
      `_snapshot/images/shop/%2Fprivate.png?v=${"a".repeat(20)}`,
      `_snapshot/images/shop/CON.png?v=${"a".repeat(20)}`,
      `_snapshot/images/shop/home.png?v=${"a".repeat(19)}`,
    ]) {
      expect(captureSchema.safeParse({ ...capture, test: path }).success).toBe(false);
    }
  });
  test("decodes canonical paths once and requires a version on data and every image role", () => {
    const path = "_snapshot/images/caf\u00e9 & shop/reference/home #1 %2F.png";
    const url = `${encodeArtifactPath(path)}?v=${"A_-0".repeat(5)}`;
    expect(artifactPath(url, "images")).toBe(path);
    expect(artifactPath(url, "data")).toBeUndefined();
    for (const role of ["reference", "test", "diff"]) {
      expect(
        captureSchema.safeParse({ ...capture, [role]: "_snapshot/images/shop/test/home.png" })
          .success,
      ).toBe(false);
    }
    expect(artifactPath(`_snapshot/data/manifest.json?v=${"a".repeat(20)}`, "data")).toBe(
      "_snapshot/data/manifest.json",
    );
    expect(artifactPath("_snapshot/data/manifest.json", "data")).toBeUndefined();
  });
  test("rejects unknown statuses, invalid percentages, duplicate image IDs", () => {
    expect(captureSchema.safeParse({ ...capture, status: "unknown" }).success).toBe(false);
    expect(
      captureSchema.safeParse({ ...capture, status: "pass", mismatch: null, error: "Timeout" })
        .success,
    ).toBe(false);
    for (const invalidId of ["CON", "NUL.png", "bad:name.png", "bad*name.png", "trailing."]) {
      expect(captureSchema.safeParse({ ...capture, id: invalidId }).success).toBe(false);
    }
    expect(captureSchema.safeParse({ ...capture, threshold: 101 }).success).toBe(false);
    expect(captureSchema.safeParse({ ...capture, mismatch: Number.NaN }).success).toBe(false);
    expect(
      suiteReportSchema.safeParse({ id: "shop", runId: "run", tests: [capture, capture] }).success,
    ).toBe(false);
  });
  test("validates empty manifests and rejects inconsistent totals and versions", () => {
    const manifest = {
      version: 2,
      title: "",
      heading: "",
      subheading: "",
      generatedAt: "2026-09-23T00:00:00Z",
      suites: [],
    };
    expect(manifestSchema.parse(manifest).suites).toEqual([]);
    expect(manifestSchema.safeParse({ ...manifest, version: 1 }).success).toBe(false);
    const suite = {
      id: "shop",
      total: 2,
      passed: 1,
      failed: 0,
      errors: 0,
      report: `_snapshot/data/suites/shop.json?v=${"a".repeat(20)}`,
    };
    expect(manifestSchema.safeParse({ ...manifest, suites: [suite] }).success).toBe(false);
  });
  test("report data requests send same-origin cookies for access-protected hosts", async () => {
    const manifest = {
      version: 2,
      title: "",
      heading: "",
      subheading: "",
      generatedAt: "2026-09-23T00:00:00Z",
      suites: [],
    };
    const url = `/_snapshot/data/manifest.json?v=${"a".repeat(20)}`;
    const requests: { url: string; init?: RequestInit }[] = [];
    const originalFetch = globalThis.fetch;
    Object.assign(globalThis, {
      fetch: async (input: string, init?: RequestInit) => {
        requests.push({ url: input, init });
        return Response.json(manifest);
      },
    });
    try {
      expect((await fetchReport(url, manifestSchema)).suites).toEqual([]);
      expect((await fetchReport(url, manifestSchema, undefined, true)).suites).toEqual([]);
    } finally {
      globalThis.fetch = originalFetch;
    }
    // Cloudflare Access redirects cookie-less requests to a cross-origin login (a CORS error).
    expect(requests.map((request) => request.url)).toEqual([url, url]);
    expect(requests.map((request) => request.init?.credentials)).toEqual([
      "same-origin",
      "same-origin",
    ]);
    expect(requests.map((request) => request.init?.cache)).toEqual(["default", "reload"]);
  });
  test("encodes route segments without interpreting names as routes", () => {
    expect(imagePath("caf\u00e9 & shop", "home #1.png")).toBe(
      "/suites/caf%C3%A9%20%26%20shop/images/home%20%231.png",
    );
  });
  test("allows only uncredentialed http(s) page links", () => {
    for (const url of [
      "javascript:alert(1)",
      "data:text/html,x",
      "file:///etc/passwd",
      "//example.com",
      "https://user:secret@example.com",
      "bad",
      null,
    ]) {
      expect(safePageUrl(url)).toBeUndefined();
    }
    expect(safePageUrl("https://example.com/a?q=b")).toBe("https://example.com/a?q=b");
  });
  test("comparison diagnostics stay failed without becoming engine errors", () => {
    expect(capture.comparisonError).toBeUndefined();
    const message = "Dimensions differ: reference 100x100 vs test 80x140";
    const compared = captureSchema.parse({ ...capture, comparisonError: message });
    expect(compared.error).toBeNull();
    expect(compared.comparisonError).toBe(message);
    expect(compared.mismatch).toBe(1.25);
    expect(filterCaptures([compared], "", "error")).toEqual([]);
    expect(filterCaptures([compared], "", "fail")).toEqual([compared]);
    expect(captureSchema.safeParse({ ...capture, comparisonError: 42 }).success).toBe(false);
  });
  test("normalizes legacy comparison errors without hiding capture failures", () => {
    const dimensions = "Dimensions differ: reference 100x100 vs test 80x140";
    for (const [mismatch, error] of [
      [1.25, dimensions],
      [0, dimensions],
      [null, "Reference image not found"],
      [null, "Comparison error: unable to compare images"],
    ] as const) {
      const parsed = captureSchema.parse({ ...capture, mismatch, error });
      expect(parsed.status).toBe("fail");
      expect(parsed.error).toBeNull();
      expect(parsed.comparisonError).toBe(error);
      expect(filterCaptures([parsed], "", "error")).toEqual([]);
      expect(filterCaptures([parsed], "", "fail")).toEqual([parsed]);
    }
    for (const error of [
      "Test scenario failed to capture: click timed out",
      "Reference scenario failed to capture: navigation failed",
      "Both reference and test scenarios failed to capture (reference: Timeout; test: Timeout)",
      "Navigation timed out after 30 seconds",
      "Test scenario failed to capture: Reference image not found",
      "Test scenario failed to capture: Comparison error: action failed",
    ]) {
      const parsed = captureSchema.parse({
        ...capture,
        mismatch: null,
        diff: null,
        dimensions: null,
        error,
      });
      expect(parsed.error).toBe(error);
      expect(parsed.comparisonError).toBeUndefined();
      expect(filterCaptures([parsed], "", "error")).toEqual([parsed]);
      expect(filterCaptures([parsed], "", "fail")).toEqual([parsed]);
    }
  });
  test("combines search and status including engine errors and zero totals", () => {
    const tests = [
      capture,
      captureSchema.parse({ ...capture, id: "error", mismatch: null, error: "Timeout" }),
      { ...capture, id: "pass", status: "pass" as const, label: "Account" },
    ];
    expect(filterCaptures(tests, "home", "fail")).toHaveLength(2);
    expect(filterCaptures(tests, " HOME ", "error")).toHaveLength(1);
    expect(filterCaptures(tests, "account", "pass")).toHaveLength(1);
    expect(filterCaptures(tests, "missing", "all")).toHaveLength(0);
    expect(passRate(0, 0)).toBe("0");
    expect(passRate(2, 3)).toBe("66.7");
  });
});

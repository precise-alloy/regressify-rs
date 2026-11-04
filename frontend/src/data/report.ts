import { z } from "zod";

const textDecoder = new TextDecoder();
const count = z.number().int().nonnegative().max(Number.MAX_SAFE_INTEGER);
const percentage = z.number().finite().min(0).max(100);
const segment = z
  .string()
  .min(1)
  .refine(
    (value) =>
      value !== "." &&
      value !== ".." &&
      !/[\\/:<>"|?*\p{Cc}]/u.test(value) &&
      !/[. ]$/.test(value) &&
      !/^(con|prn|aux|nul|com[1-9]|lpt[1-9])(?:\.|$)/i.test(value),
  );

export function encodeArtifactPath(path: string): string {
  return path
    .split("/")
    .map((part) =>
      encodeURIComponent(part).replace(
        /[!'()*]/g,
        (character) => `%${character.charCodeAt(0).toString(16).toUpperCase()}`,
      ),
    )
    .join("/");
}

export function artifactPath(value: string, kind: "data" | "images"): string | undefined {
  const [path, version, extra] = value.split("?v=");
  if (
    !path ||
    !version ||
    extra !== undefined ||
    !/^[A-Za-z0-9_-]{20}$/.test(version) ||
    !path.startsWith(`_snapshot/${kind}/`) ||
    !path.toLowerCase().endsWith(kind === "data" ? ".json" : ".png")
  )
    return undefined;
  try {
    const parts = path.split("/").map(decodeURIComponent);
    if (parts.some((part) => !segment.safeParse(part).success)) return undefined;
    const decoded = parts.join("/");
    return encodeArtifactPath(decoded) === path ? decoded : undefined;
  } catch {
    return undefined;
  }
}

const artifact = (kind: "data" | "images") =>
  z.string().refine((value) => artifactPath(value, kind) !== undefined);

export const suiteSummarySchema = z
  .object({
    id: segment,
    displayName: z.string().optional(),
    total: count,
    passed: count,
    failed: count,
    errors: count,
    report: artifact("data"),
  })
  .refine((suite) => suite.passed + suite.failed === suite.total && suite.errors <= suite.failed);

export const manifestSchema = z
  .object({
    version: z.literal(2),
    title: z.string(),
    heading: z.string(),
    subheading: z.string(),
    generatedAt: z.iso.datetime(),
    regressifyVersion: z.string().optional(),
    suites: z.array(suiteSummarySchema),
  })
  .refine(
    (manifest) => new Set(manifest.suites.map((suite) => suite.id)).size === manifest.suites.length,
  );

export const archiveStateSchema = z.enum([
  "complete",
  "partial",
  "missing",
  "legacyUnavailable",
  "loadingError",
]);

export const executionLogSchema = z
  .object({
    state: archiveStateSchema,
    operationId: z.uuid().nullable(),
    executionId: z.uuid().nullable(),
    chunks: z.array(artifact("data")).max(16384),
    records: count,
    missingRecords: count.nullable(),
    build: z
      .object({ version: z.string(), sourceCommit: z.string(), dirty: z.boolean().nullable() })
      .nullable(),
  })
  .refine(
    (log) =>
      (["complete", "partial"].includes(log.state) || log.chunks.length === 0) &&
      (log.chunks.length === 0 ||
        (log.operationId !== null &&
          log.records > 0 &&
          new Set(log.chunks).size === log.chunks.length &&
          log.chunks.every((chunk) =>
            artifactPath(chunk, "data")?.startsWith(`_snapshot/data/logs/${log.operationId}/`),
          ))),
  );

export type ExecutionLog = z.infer<typeof executionLogSchema>;
export type ArchiveState = z.infer<typeof archiveStateSchema>;

const imageSizeSchema = z.object({
  width: z.number().int().positive().max(0xffffffff),
  height: z.number().int().positive().max(0xffffffff),
});

const verticalGapSchema = z.object({
  start: count.max(0xffffffff),
  height: z.number().int().positive().max(0xffffffff),
});
type VerticalGap = z.infer<typeof verticalGapSchema>;

function validGapIntervals(gaps: readonly VerticalGap[]): Array<[number, number]> | null {
  const intervals: Array<[number, number]> = [];
  let shift = 0;
  let previousStart = -1;
  for (const gap of gaps) {
    if (gap.start <= previousStart) return null;
    previousStart = gap.start;
    const alignedStart = gap.start + shift;
    const alignedEnd = alignedStart + gap.height;
    intervals.push([alignedStart, alignedEnd]);
    shift += gap.height;
  }
  return intervals;
}

const diffAlignmentSchema = z
  .object({
    reference: verticalGapSchema.nullable(),
    test: verticalGapSchema.nullable(),
    referenceGaps: z.array(verticalGapSchema).nonempty().optional(),
    testGaps: z.array(verticalGapSchema).nonempty().optional(),
  })
  .refine((alignment) => {
    if (!alignment.referenceGaps && !alignment.testGaps && alignment.reference && alignment.test) {
      return false;
    }
    if (
      alignment.referenceGaps &&
      (!alignment.reference ||
        alignment.reference.start !== alignment.referenceGaps[0]?.start ||
        alignment.reference.height !== alignment.referenceGaps[0]?.height)
    ) {
      return false;
    }
    if (
      alignment.testGaps &&
      (!alignment.test ||
        alignment.test.start !== alignment.testGaps[0]?.start ||
        alignment.test.height !== alignment.testGaps[0]?.height)
    ) {
      return false;
    }
    const refGaps = alignment.referenceGaps ?? (alignment.reference ? [alignment.reference] : []);
    const testGaps = alignment.testGaps ?? (alignment.test ? [alignment.test] : []);
    const refIntervals = validGapIntervals(refGaps);
    const testIntervals = validGapIntervals(testGaps);
    if (!refIntervals || !testIntervals) return false;
    return !refIntervals.some(([rStart, rEnd]) =>
      testIntervals.some(([tStart, tEnd]) => rStart < tEnd && tStart < rEnd),
    );
  });

export const captureSchema = z
  .object({
    id: segment,
    label: z.string(),
    scenario: z.string(),
    selector: z.string(),
    status: z.enum(["pass", "fail"]),
    threshold: percentage,
    mismatch: percentage.nullable(),
    mismatchPixels: count.max(64_000_000).optional(),
    error: z.string().nullable(),
    comparisonError: z.string().nullable().optional(),
    reference: artifact("images").nullable(),
    test: artifact("images").nullable(),
    diff: artifact("images").nullable(),
    alignment: diffAlignmentSchema.optional(),
    imageSizes: z
      .object({
        reference: imageSizeSchema.nullable(),
        test: imageSizeSchema.nullable(),
        diff: imageSizeSchema.nullable(),
      })
      .optional(),
    previews: z
      .object({
        reference: artifact("images").nullable(),
        test: artifact("images").nullable(),
        diff: artifact("images").nullable(),
        grid: artifact("images").nullable(),
      })
      .optional(),
    referenceUrl: z.string().nullable(),
    testUrl: z.string().nullable(),
    dimensions: z
      .object({ same: z.boolean(), width: z.number().int(), height: z.number().int() })
      .nullable(),
    logs: z
      .object({ reference: executionLogSchema.nullable(), test: executionLogSchema.nullable() })
      .optional(),
  })
  .transform((capture) => {
    if (
      capture.error !== null &&
      (capture.mismatch !== null ||
        capture.error === "Reference image not found" ||
        capture.error.startsWith("Comparison error: "))
    ) {
      return {
        ...capture,
        comparisonError: capture.comparisonError ?? capture.error,
        error: null,
      };
    }
    return capture;
  })
  .refine((capture) => capture.error === null || capture.status === "fail");

export const suiteReportSchema = z
  .object({
    id: segment,
    displayName: z.string().optional(),
    runId: z.string(),
    tests: z.array(captureSchema),
  })
  .refine((suite) => new Set(suite.tests.map((test) => test.id)).size === suite.tests.length);

export type Manifest = z.infer<typeof manifestSchema>;
export type SuiteSummary = z.infer<typeof suiteSummarySchema>;
export type SuiteReport = z.infer<typeof suiteReportSchema>;
export type Capture = z.infer<typeof captureSchema>;
export type StatusFilter = "all" | "fail" | "pass" | "error";

export function imageAlignmentGaps(
  gaps: readonly VerticalGap[] | VerticalGap | null | undefined,
  imageHeight: number,
  diffHeight: number | undefined,
): VerticalGap[] | null {
  const list: readonly VerticalGap[] = Array.isArray(gaps) ? gaps : gaps ? [gaps] : [];
  if (list.length === 0 || diffHeight === undefined) return null;
  let totalHeight = 0;
  let previousStart = -1;
  for (const gap of list) {
    if (gap.height <= 0 || gap.start > imageHeight || gap.start <= previousStart) return null;
    previousStart = gap.start;
    totalHeight += gap.height;
  }
  return imageHeight + totalHeight === diffHeight ? [...list] : null;
}

export function imageAlignmentGap(
  gap: VerticalGap | null | undefined,
  imageHeight: number,
  diffHeight: number | undefined,
): VerticalGap | null {
  return gap && gap.start <= imageHeight && imageHeight + gap.height === diffHeight ? gap : null;
}

export function imageRowToAlignedRow(
  row: number,
  gap: VerticalGap | readonly VerticalGap[] | null,
): number {
  if (!gap) return row;
  if (!Array.isArray(gap)) {
    const single = gap as VerticalGap;
    return row >= single.start ? row + single.height : row;
  }
  let shift = 0;
  for (const item of gap) {
    if (row >= item.start) shift += item.height;
  }
  return row + shift;
}

export function alignedRowToImageRow(
  row: number,
  gap: VerticalGap | readonly VerticalGap[] | null,
): number | null {
  if (!gap) return row;
  if (!Array.isArray(gap)) {
    const single = gap as VerticalGap;
    if (row < single.start) return row;
    return row < single.start + single.height ? null : row - single.height;
  }
  let shift = 0;
  for (const item of gap) {
    const alignedStart = item.start + shift;
    if (row < alignedStart) return row - shift;
    if (row < alignedStart + item.height) return null;
    shift += item.height;
  }
  return row - shift;
}

export function suiteLabel(suite: { id: string; displayName?: string }): string {
  return suite.displayName?.trim() ? suite.displayName : suite.id;
}

export function safePageUrl(value: string | null): string | undefined {
  if (!value) return undefined;
  try {
    const url = new URL(value);
    return (url.protocol === "http:" || url.protocol === "https:") && !url.username && !url.password
      ? url.href
      : undefined;
  } catch {
    return undefined;
  }
}

export function suitePath(id: string): string {
  return `/suites/${encodeURIComponent(id)}`;
}

export function imagePath(suite: string, capture: string): string {
  return `${suitePath(suite)}/images/${encodeURIComponent(capture)}`;
}

export function passRate(passed: number, total: number): string {
  return total === 0
    ? "0"
    : ((passed / total) * 100).toLocaleString("en", { maximumFractionDigits: 1 });
}

export function formatMismatch(mismatch: number): string {
  for (let decimals = 2; decimals <= 12; decimals++) {
    const formatted = mismatch.toFixed(decimals);
    if (mismatch === 0 || Number(formatted) !== 0) return formatted;
  }
  return mismatch.toString();
}

export function filterCaptures(tests: Capture[], query: string, status: StatusFilter): Capture[] {
  const needle = query.trim().toLocaleLowerCase();
  return tests.filter(
    (test) =>
      (status === "all" || (status === "error" ? test.error !== null : test.status === status)) &&
      (!needle ||
        `${test.label} ${test.scenario} ${test.selector} ${test.id}`
          .toLocaleLowerCase()
          .includes(needle)),
  );
}

export async function fetchReport<T>(
  url: string,
  schema: z.ZodType<T>,
  signal?: AbortSignal,
  reload = false,
  maxBytes?: number,
): Promise<T> {
  const response = await fetch(url, {
    signal: signal
      ? AbortSignal.any([signal, AbortSignal.timeout(15_000)])
      : AbortSignal.timeout(15_000),
    // Access gateways such as Cloudflare Access authorize data with the page's session cookie.
    credentials: "same-origin",
    cache: reload ? "reload" : "default",
  });
  if (!response.ok) throw new Error(`Report request failed (${response.status}).`);
  let value: unknown;
  if (maxBytes !== undefined) {
    const reader = response.body?.getReader();
    if (!reader) throw new Error("Report response has no content.");
    const chunks: Uint8Array[] = [];
    let length = 0;
    try {
      while (true) {
        const item = await reader.read();
        if (item.done) break;
        length += item.value.byteLength;
        if (length > maxBytes) throw new Error("Log chunk exceeds its size limit.");
        chunks.push(item.value);
      }
    } finally {
      await reader.cancel().catch(() => {});
      reader.releaseLock();
    }
    const bytes = new Uint8Array(length);
    let offset = 0;
    for (const chunk of chunks) {
      bytes.set(chunk, offset);
      offset += chunk.length;
    }
    const expected = new URL(url, window.location.origin).searchParams.get("v");
    const digest = new Uint8Array(await crypto.subtle.digest("SHA-384", bytes));
    const actual = btoa(String.fromCharCode(...digest))
      .replaceAll("+", "-")
      .replaceAll("/", "_")
      .replace(/=+$/, "")
      .slice(0, 20);
    if (expected !== actual) throw new Error("Log chunk version does not match its content.");
    value = JSON.parse(textDecoder.decode(bytes));
  } else {
    value = await response.json();
  }
  const parsed = schema.safeParse(value);
  if (!parsed.success) throw new Error("This report is invalid or uses an unsupported format.");
  return parsed.data;
}

import { z } from "zod";
import { type ExecutionLog, fetchReport } from "./report";

export const logLevels = ["trace", "debug", "info", "warn", "error"] as const;
export const logStages = [
  "queueSetup",
  "navigation",
  "stateHooks",
  "actionsFrames",
  "screenshotSettling",
  "captureArtifacts",
  "finish",
  "preflight",
  "snapshotStaging",
  "packagingOptimization",
  "apiUploads",
  "retryPolling",
  "completionCleanup",
] as const;
const textEncoder = new TextEncoder();
const integer = z.number().int().nonnegative().max(Number.MAX_SAFE_INTEGER);
const identity = z
  .string()
  .min(1)
  .refine(
    (value) =>
      value === value.trim() &&
      textEncoder.encode(value).byteLength <= 2048 &&
      !/[\p{Cc}\u202a-\u202e\u2066-\u2069]/u.test(value),
    "Invalid diagnostic source metadata",
  );
const fieldValue = z.discriminatedUnion("type", [
  z.object({ type: z.literal("number"), value: integer }),
  z.object({
    type: z.literal("coordinate"),
    value: z.number().int().min(-2147483648).max(2147483647),
  }),
  z.object({ type: z.literal("boolean"), value: z.boolean() }),
  z.object({ type: z.literal("identity"), value: identity }),
  z.object({
    type: z.literal("kind"),
    value: z.enum([
      "click",
      "hover",
      "check",
      "uncheck",
      "focus",
      "input",
      "press",
      "select",
      "scroll",
      "navigate",
      "save",
      "hide",
      "remove",
      "wait",
      "persist",
      "restore",
      "switchTab",
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
      "hook",
      "before",
      "ready",
      "exists",
      "visible",
      "stable",
      "enabled",
      "hidden",
      "detached",
      "disabled",
      "content",
      "unobscured",
      "fonts",
      "images",
      "lazyLoading",
      "animation",
      "caret",
      "cached",
      "download",
      "browser",
      "page",
      "handler",
      "azure",
      "netlify",
      "cloudflare",
      "manifest",
      "package",
      "upload",
      "poll",
      "publication",
      "preflight",
      "transient",
      "rateLimited",
      "timeout",
      "pending",
      "complete",
      "failed",
      "cancelled",
      "init",
      "install",
      "mcp",
      "version",
      "test",
      "reference",
      "compare",
      "approve",
      "snapshot",
      "optimize",
      "clean",
      "empty",
      "belowMinimum",
      "unchanged",
      "cli",
      "desktop",
      "worker",
      "shell",
      "window",
      "unknown",
    ]),
  }),
]);
const fields = [
  "command",
  "actionIndex",
  "actionType",
  "target",
  "destination",
  "artifact",
  "capture",
  "timeoutMs",
  "elementTimeoutMs",
  "actionabilityTimeoutMs",
  "pollIntervalMs",
  "scenarioTimeoutMs",
  "retryLimit",
  "retryAttempt",
  "backoffMs",
  "queueMs",
  "elapsedMs",
  "completedAnimations",
  "frozenAnimations",
  "width",
  "height",
  "x",
  "y",
  "bytes",
  "bytesBefore",
  "bytesAfter",
  "budgetBytes",
  "fileCount",
  "skippedFiles",
  "batchCount",
  "selectedSuites",
  "statusCode",
  "osErrorCode",
  "requestId",
  "provider",
  "endpoint",
  "condition",
  "reason",
  "state",
  "credentialPresent",
  "compression",
  "deadlineMs",
  "redirectCount",
  "frameDepth",
  "tabCount",
  "infrastructure",
  "cleanupFailed",
  "simplifiedImages",
  "smallImages",
  "excludedImages",
  "filteredImages",
  "transformedImages",
  "overlappedImages",
  "invalidSelectors",
] as const;
const category = z.enum([
  "timeout",
  "notFound",
  "permission",
  "navigation",
  "actionability",
  "cancelled",
  "invalidData",
  "storage",
  "provider",
  "unknown",
]);
export const logRecordSchema = z.object({
  schemaVersion: z.literal(1),
  operationId: z.uuid(),
  operationKind: z.literal("capture"),
  captureRunId: z.string().nullable(),
  execution: z
    .object({
      executionId: z.uuid(),
      suiteId: identity,
      scenarioId: identity,
      viewport: z.object({ name: identity, width: integer.positive(), height: integer.positive() }),
      captureRole: z.enum(["reference", "test"]),
      attempt: integer.positive(),
    })
    .nullable(),
  sequence: integer,
  timestampUtcMs: z.number().int().min(-8640000000000000).max(8640000000000000),
  elapsedMs: integer,
  severity: z.enum(logLevels),
  stage: z.enum(logStages),
  eventName: z.enum([
    "queued",
    "started",
    "stageStarted",
    "stageFinished",
    "decision",
    "waiting",
    "retry",
    "heartbeat",
    "captured",
    "compared",
    "published",
    "warning",
    "cancelled",
    "finished",
  ]),
  message: z.string().max(4096),
  fields: z.partialRecord(z.enum(fields), fieldValue),
  spanId: z.uuid(),
  parentSpan: z.uuid().nullable(),
  durationMs: integer.nullable(),
  outcome: z
    .enum(["success", "failure", "cancelled", "cancelledRemoteUnknown", "mismatch", "unknown"])
    .nullable(),
  errorCause: z
    .object({ category, causes: z.array(category).max(8) })
    .strict()
    .nullable(),
});
export const logChunkSchema = z.object({
  version: z.literal(1),
  operationId: z.uuid(),
  records: z.array(logRecordSchema).min(1).max(250),
});
export type LogRecord = z.infer<typeof logRecordSchema>;
const chunkCache = new Map<string, Promise<z.infer<typeof logChunkSchema>>>();

export function formatLogRecord(record: LogRecord): string {
  const source = record.execution
    ? ` suite=${record.execution.suiteId} scenario=${record.execution.scenarioId} viewport=${record.execution.viewport.name}`
    : "";
  const fields = Object.entries(record.fields)
    .map(([key, field]) => `${key}=${field.value}`)
    .join(" ");
  const outcome = record.outcome ? ` outcome=${record.outcome}` : "";
  const duration = record.durationMs === null ? "" : ` duration=${record.durationMs}ms`;
  const causes = record.errorCause ? ` causes=${record.errorCause.causes.join(", ")}` : "";
  return `[${record.elapsedMs}ms #${record.sequence}] ${record.severity} ${record.stage}: ${record.message}${source}${fields ? ` ${fields}` : ""}${outcome}${duration}${causes}`;
}

export async function fetchLogChunk(
  log: ExecutionLog,
  index: number,
  signal?: AbortSignal,
  revision = 0,
) {
  signal?.throwIfAborted();
  const path = index >= 0 ? log.chunks[index] : undefined;
  if (!path || !log.operationId) throw new Error("Log chunk is missing.");
  const cacheKey = `${path}#${revision}`;
  let request = chunkCache.get(cacheKey);
  if (!request) {
    while (chunkCache.size >= 4) {
      const oldest = chunkCache.keys().next().value;
      if (!oldest) break;
      chunkCache.delete(oldest);
    }
    request = fetchReport(
      `/${path}`,
      logChunkSchema,
      AbortSignal.timeout(15000),
      revision > 0,
      256 * 1024,
    );
    chunkCache.set(cacheKey, request);
    request.catch(() => {
      if (chunkCache.get(cacheKey) === request) chunkCache.delete(cacheKey);
    });
  }
  const chunk = await (signal
    ? new Promise<z.infer<typeof logChunkSchema>>((resolve, reject) => {
        const abort = () => reject(signal.reason);
        signal.addEventListener("abort", abort, { once: true });
        request.then(
          (value) => {
            signal.removeEventListener("abort", abort);
            resolve(value);
          },
          (error: unknown) => {
            signal.removeEventListener("abort", abort);
            reject(error);
          },
        );
      })
    : request);
  signal?.throwIfAborted();
  const firstSequence = chunk.records[0]?.sequence ?? -1;
  const lastSequence = chunk.records.at(-1)?.sequence ?? -1;
  if (
    chunk.operationId !== log.operationId ||
    (index === 0 ? firstSequence !== 0 : firstSequence < index) ||
    lastSequence >= log.records ||
    (log.state === "complete" &&
      index === log.chunks.length - 1 &&
      lastSequence !== log.records - 1) ||
    chunk.records.some(
      (record, position) =>
        record.operationId !== log.operationId ||
        (position > 0 && record.sequence !== (chunk.records[position - 1]?.sequence ?? -1) + 1) ||
        (record.execution && log.executionId && record.execution.executionId !== log.executionId),
    )
  )
    throw new Error("Log identity or sequence does not match this capture.");
  return chunk.records;
}

export function matchesLog(
  record: LogRecord,
  level: string,
  stage: string,
  search: string,
): boolean {
  const minimum = logLevels.indexOf(level as (typeof logLevels)[number]);
  if (
    logLevels.indexOf(record.severity) < minimum ||
    (stage && stage !== "all" && record.stage !== stage)
  )
    return false;
  const needle = search.toLowerCase();
  return !needle || formatLogRecord(record).toLowerCase().includes(needle);
}

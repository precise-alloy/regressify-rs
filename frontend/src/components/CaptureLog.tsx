import { ArrowLeft, ArrowRight, Copy, Download, RefreshCw } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import {
  fetchLogChunk,
  formatLogRecord,
  type LogRecord,
  logLevels,
  logStages,
  matchesLog,
} from "../data/logs";
import type { ArchiveState, Capture } from "../data/report";
import { IconButton, segmented, segmentedButton } from "./ui";

const textEncoder = new TextEncoder();
const availability: Record<ArchiveState, string> = {
  complete: "Complete capture log",
  partial: "Partial or interrupted log; records may be missing",
  missing: "Expected capture log is missing",
  legacyUnavailable: "Legacy capture: log not available",
  loadingError: "Capture log could not be loaded",
};

const logLabel = "flex min-w-0 flex-col gap-1 text-[12px]";

export function CaptureLog({
  capture,
  params,
  set,
}: {
  capture: Capture;
  params: URLSearchParams;
  set: (key: string, value: string) => void;
}) {
  const role = params.get("logRole") === "reference" ? "reference" : "test";
  const log = capture.logs?.[role];
  const state = log?.state ?? "legacyUnavailable";
  const requested = Number(params.get("logChunk") || 0);
  const index =
    Number.isSafeInteger(requested) && requested >= 0 && requested < (log?.chunks.length ?? 0)
      ? requested
      : 0;
  const level = params.get("logLevel") || "all";
  const stage = params.get("logStage") || "all";
  const search = params.get("logSearch") || "";
  const [records, setRecords] = useState<LogRecord[]>([]);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState("");
  const [revision, setRevision] = useState(0);
  const [exporting, setExporting] = useState(false);
  const [notice, setNotice] = useState("");
  const exportController = useRef<AbortController | null>(null);
  useEffect(() => () => exportController.current?.abort(), []);
  useEffect(() => {
    const controller = new AbortController();
    setRecords([]);
    setError("");
    setNotice("");
    if (!log || !["complete", "partial"].includes(log.state) || !log.chunks.length) {
      setLoading(false);
      return;
    }
    setLoading(true);
    fetchLogChunk(log, index, controller.signal, revision)
      .then((records) => {
        if (!controller.signal.aborted) setRecords(records);
      })
      .catch((error: unknown) => {
        if (!controller.signal.aborted)
          setError(error instanceof Error ? error.message : "Capture log could not be loaded");
      })
      .finally(() => {
        if (!controller.signal.aborted) setLoading(false);
      });
    return () => controller.abort();
  }, [log, index, revision]);
  const filtered = records.filter((record) => matchesLog(record, level, stage, search));
  const execution = records.find((record) => record.execution)?.execution;
  const shownState = error ? "loadingError" : state;
  async function download() {
    if (!log || state !== "complete" || error || exporting) return;
    exportController.current?.abort();
    const controller = new AbortController();
    exportController.current = controller;
    setExporting(true);
    setNotice("");
    try {
      const parts: string[] = [];
      let count = 0;
      let bytes = 0;
      for (let chunk = 0; chunk < log.chunks.length; chunk += 1) {
        const entries = await fetchLogChunk(log, chunk, controller.signal, revision);
        for (const record of entries) {
          if (record.sequence !== count) throw new Error("Log is incomplete; export stopped.");
          count += 1;
          if (matchesLog(record, level, stage, search)) {
            const text = `${formatLogRecord(record)}\n`;
            bytes += textEncoder.encode(text).byteLength;
            if (bytes > 128 * 1024 * 1024)
              throw new Error(
                "Filtered log exceeds the 128 MiB browser export limit. Narrow the filters.",
              );
            parts.push(text);
          }
        }
      }
      if (count !== log.records)
        throw new Error("Log record count does not match; export stopped.");
      const url = URL.createObjectURL(new Blob(parts, { type: "text/plain;charset=utf-8" }));
      const link = document.createElement("a");
      link.href = url;
      link.download = `${capture.id}-${role}-log.txt`;
      link.click();
      URL.revokeObjectURL(url);
    } catch (error) {
      if (!controller.signal.aborted)
        setNotice(error instanceof Error ? error.message : "Log export failed");
    } finally {
      if (!controller.signal.aborted) setExporting(false);
    }
  }
  return (
    <section className="w-full min-w-0" aria-label="Capture Log">
      <div className="flex flex-wrap gap-2.5 border-b border-b-border py-3 [align-items:end] print:hidden">
        <fieldset className={`${segmented} max-[601px]:basis-full`} aria-label="Log execution">
          {(["reference", "test"] as const).map((value) => (
            <button
              type="button"
              key={value}
              aria-pressed={role === value}
              className={segmentedButton}
              onClick={() => set("logRole", value)}
            >
              {value === "reference" ? "Reference" : "Test"}
            </button>
          ))}
        </fieldset>
        <label className={logLabel}>
          Severity
          <select
            aria-label="Log severity"
            className="max-w-full min-h-8 rounded-[4px] border border-border bg-surface px-2 py-1 text-foreground max-[601px]:w-[150px]"
            value={level}
            onChange={(event) => set("logLevel", event.target.value)}
          >
            <option value="all">All levels</option>
            {logLevels.map((value) => (
              <option key={value} value={value}>
                {value}
              </option>
            ))}
          </select>
        </label>
        <label className={logLabel}>
          Stage
          <select
            aria-label="Log stage"
            className="max-w-full min-h-8 rounded-[4px] border border-border bg-surface px-2 py-1 text-foreground max-[601px]:w-[150px]"
            value={stage}
            onChange={(event) => set("logStage", event.target.value)}
          >
            <option value="all">All stages</option>
            {logStages.map((value) => (
              <option key={value} value={value}>
                {value}
              </option>
            ))}
          </select>
        </label>
        <label className={`${logLabel} flex-[1_1_160px]`}>
          Search
          <input
            className="max-w-full min-h-8 rounded-[4px] border border-border bg-surface px-2 py-1 text-foreground"
            type="search"
            aria-label="Search capture log"
            value={search}
            onChange={(event) => set("logSearch", event.target.value)}
          />
        </label>
        <IconButton
          label="Copy displayed log page"
          disabled={shownState !== "complete" || loading}
          onClick={() => {
            navigator.clipboard
              .writeText(filtered.map(formatLogRecord).join("\n"))
              .catch(() => setNotice("Clipboard is unavailable"));
          }}
        >
          <Copy size={16} />
        </IconButton>
        <IconButton
          label={
            shownState === "complete"
              ? "Download filtered capture log"
              : `Export unavailable: ${availability[shownState]}`
          }
          disabled={shownState !== "complete" || loading || exporting}
          onClick={download}
        >
          <Download size={16} />
        </IconButton>
      </div>
      <div
        className="flex flex-wrap gap-2.5 py-2.5 text-[12px] not-data-[log-state=complete]:text-error"
        data-log-state={shownState}
        role="status"
      >
        <span>{availability[shownState]}</span>
        {execution && (
          <span className="mono">
            {execution.viewport.width} x {execution.viewport.height} / Attempt {execution.attempt}
          </span>
        )}
        {log?.missingRecords ? <span>{log.missingRecords} missing records</span> : null}
      </div>
      {notice && <p role="alert">{notice}</p>}
      {error && (
        <p role="alert">
          {error}{" "}
          <IconButton label="Retry capture log" onClick={() => setRevision(revision + 1)}>
            <RefreshCw size={16} />
          </IconButton>
        </p>
      )}
      {loading ? (
        <p role="status">Loading capture log...</p>
      ) : (
        // `log-records` is a test hook.
        <ol className="log-records m-0 p-0 [list-style:none]" aria-label="Execution timeline">
          {filtered.map((record) => {
            const timestamp = new Date(record.timestampUtcMs).toISOString();
            return (
              <li
                key={`${record.operationId}-${record.sequence}`}
                data-severity={record.severity}
                className="group/record grid grid-cols-[100px_minmax(0,1fr)] gap-2.5 border-b border-b-border py-2 max-[601px]:grid-cols-[minmax(0,1fr)] max-[601px]:gap-0.5"
              >
                <time dateTime={timestamp} className="font-mono text-[11px] text-muted">
                  {timestamp.slice(11, 23)}
                </time>
                <pre className="m-0 font-mono text-[12px] leading-[1.6] whitespace-pre-wrap [overflow-wrap:anywhere] group-data-[severity=error]/record:text-fail group-data-[severity=warn]/record:text-error">
                  {formatLogRecord(record)}
                </pre>
              </li>
            );
          })}
        </ol>
      )}
      <div className="flex items-center gap-3 py-3 [justify-content:end]">
        <IconButton
          label="Previous log chunk"
          disabled={index === 0 || loading}
          onClick={() => set("logChunk", String(index - 1))}
        >
          <ArrowLeft size={16} />
        </IconButton>
        <span className="mono">
          {log?.chunks.length ? `${index + 1} / ${log.chunks.length}` : "0 / 0"}
        </span>
        <IconButton
          label="Next log chunk"
          disabled={index + 1 >= (log?.chunks.length ?? 0) || loading}
          onClick={() => set("logChunk", String(index + 1))}
        >
          <ArrowRight size={16} />
        </IconButton>
      </div>
    </section>
  );
}

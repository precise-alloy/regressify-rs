import { ArrowDown, ArrowUp, ArrowUpRight, Grid2X2, List } from "lucide-react";
import { useDeferredValue, useRef, useState } from "react";
import { Link, useLocation, useParams } from "react-router";
import {
  Filters,
  IconButton,
  ReportImage,
  StatePanel,
  StatusBadge,
  segmented,
  useFilters,
} from "../components/ui";
import { filterCaptures, formatMismatch, imagePath, passRate, suiteLabel } from "../data/report";
import { useSuite } from "../data/store";

export function SuitePage() {
  const { suiteId } = useParams();
  const { data, summary, error, retry } = useSuite(suiteId);
  const { query, status, params, set } = useFilters();
  const deferredQuery = useDeferredValue(query);
  const location = useLocation();
  const cards = useRef(new Map<string, HTMLAnchorElement>());
  const [selectedFailure, setSelectedFailure] = useState<{ scope: string; id: string } | null>(
    null,
  );
  if (!summary)
    return (
      <StatePanel
        title="Suite not found"
        detail="This suite is not included in this snapshot."
        back
      />
    );
  if (error) return <StatePanel title="Suite unavailable" detail={error} retry={retry} />;
  if (!data) return <StatePanel title="Loading suite" loading />;
  const captures = filterCaptures(data.tests, deferredQuery, status);
  const failures = captures.filter((capture) => capture.status === "fail");
  const scope = JSON.stringify([suiteId, deferredQuery, status]);
  const failureIndex =
    selectedFailure?.scope === scope
      ? failures.findIndex((capture) => capture.id === selectedFailure.id)
      : -1;
  const jumpToFailure = (index: number) => {
    const capture = failures[index];
    const target = capture && cards.current.get(capture.id);
    if (!capture || !target) return;
    setSelectedFailure({ scope, id: capture.id });
    target.scrollIntoView({
      behavior: matchMedia("(prefers-reduced-motion: reduce)").matches ? "instant" : "smooth",
      block: "start",
      inline: "nearest",
    });
    target.focus({ preventScroll: true });
  };
  const list = params.get("view") === "list";
  // `capture-grid`, `capture-list`, `capture-card`, `capture-thumbnail`,
  // `capture-detail`, `capture-comparison`, `capture-lane`, and
  // `capture-lane-preview` stay as hooks for tests (and, for the last one, the
  // shared image-unavailable rule in styles.css).
  const summaryCount = "mr-1 font-mono text-[14px] font-medium";
  const detail = "mono text-[12px]";
  return (
    <div className="page suite-page">
      <header className="page-heading">
        <div>
          <div className="mb-2.5 font-mono text-[10px] text-muted uppercase">Test suite</div>
          <h1>{suiteLabel(summary)}</h1>
          <p className="mono mt-2.5 text-[11px] text-muted">Run {data.runId || "Unknown"}</p>
        </div>
        <StatusBadge status={summary.failed ? "fail" : "pass"} />
      </header>
      <div className="mb-4 flex flex-wrap gap-x-6 gap-y-3 border-b border-b-border pb-4 text-[14px] max-[601px]:gap-x-[18px]">
        <span>
          <strong className={summaryCount}>{summary.total}</strong> captures
        </span>
        <span className="text-pass">
          <strong className={summaryCount}>{summary.passed}</strong> passed
        </span>
        <span className={summary.failed ? "text-fail" : "muted"}>
          <strong className={summaryCount}>{summary.failed}</strong> failed
        </span>
        <span className="muted">{passRate(summary.passed, summary.total)}% pass rate</span>
      </div>
      <div className="flex items-start gap-3 max-[1151px]:flex-wrap max-[601px]:gap-0">
        <Filters
          suite
          counts={{
            all: summary.total,
            pass: summary.passed,
            fail: summary.failed,
            error: data.tests.filter((capture) => capture.error !== null).length,
          }}
        />
        <div className={`${segmented} ml-auto max-[601px]:mb-3 print:hidden`}>
          <IconButton
            compact
            label="Grid view"
            aria-pressed={!list}
            onClick={() => set("view", "")}
          >
            <Grid2X2 size={17} />
          </IconButton>
          <IconButton
            compact
            label="List view"
            aria-pressed={list}
            onClick={() => set("view", "list")}
          >
            <List size={17} />
          </IconButton>
        </div>
      </div>
      <div className="sticky top-0 z-3 mb-3 flex items-center justify-between gap-3 bg-bg py-[5px]">
        <span className="muted mono text-[10px]" aria-live="polite">
          {captures.length} {captures.length === 1 ? "capture" : "captures"}
        </span>
        <div className="flex items-center gap-1">
          <output
            className="mono muted mr-2 text-[12px]"
            aria-label="Failed comparison position"
            aria-live="polite"
          >
            {failureIndex < 0 ? failures.length : `${failureIndex + 1} / ${failures.length}`} failed
          </output>
          <IconButton
            label="Previous failed comparison"
            disabled={failureIndex <= 0}
            onClick={() => jumpToFailure(failureIndex - 1)}
          >
            <ArrowUp size={17} />
          </IconButton>
          <IconButton
            label="Next failed comparison"
            disabled={failureIndex + 1 >= failures.length}
            onClick={() => jumpToFailure(failureIndex + 1)}
          >
            <ArrowDown size={17} />
          </IconButton>
        </div>
      </div>
      {captures.length ? (
        <div
          className={
            list
              ? "capture-list flex flex-col gap-3"
              : "capture-grid grid grid-cols-[repeat(auto-fill,minmax(min(100%,290px),1fr))] gap-5"
          }
        >
          {captures.map((capture) => (
            <Link
              className={`capture-card group/card min-w-0 scroll-mt-[58px] overflow-hidden rounded-[6px] border border-border bg-surface [transition:border-color_150ms] hover:border-accent data-[active-failure=true]:outline-2 data-[active-failure=true]:outline-offset-2 data-[active-failure=true]:outline-fail print:break-inside-avoid ${list ? "flex flex-col [overflow-wrap:anywhere]" : ""}`}
              key={capture.id}
              ref={(element) => {
                if (element) cards.current.set(capture.id, element);
                else cards.current.delete(capture.id);
              }}
              data-active-failure={
                failureIndex >= 0 && failures[failureIndex]?.id === capture.id ? "true" : undefined
              }
              to={`${imagePath(summary.id, capture.id)}${location.search}`}
            >
              {!list && (
                <div className="capture-thumbnail relative aspect-[16/10] overflow-hidden border-b border-b-border bg-canvas">
                  <ReportImage
                    path={capture.previews?.grid || capture.test || capture.reference}
                    alt={`${capture.label} preview`}
                    width={640}
                    height={400}
                    className={
                      capture.previews?.grid
                        ? "block h-full w-full object-contain object-top"
                        : "block h-auto w-full"
                    }
                  />
                  <span className="absolute top-3 right-3 grid size-7 place-items-center rounded-[4px] border border-border bg-surface opacity-0 group-hover/card:opacity-100 group-focus-visible/card:opacity-100">
                    <ArrowUpRight size={18} />
                  </span>
                </div>
              )}
              <div className={`px-4 pt-3.5 pb-4 ${list ? "max-[601px]:p-3" : ""}`}>
                <div
                  className={`mb-2.5 flex items-center justify-between gap-2.5 ${list ? "max-[601px]:flex-wrap max-[601px]:gap-1.5" : ""}`}
                >
                  <span className="truncate text-[13px] text-muted">
                    {capture.scenario || capture.selector || "Capture"}
                  </span>
                  <StatusBadge status={capture.status} error={capture.error !== null} />
                </div>
                <h2 className="text-[16px] leading-[1.5] font-[550]">{capture.label}</h2>
                <div className="capture-detail mt-3 flex flex-wrap gap-x-3 gap-y-[5px]">
                  <span className={detail}>
                    {capture.mismatch === null
                      ? "Not compared"
                      : `${formatMismatch(capture.mismatch)}% difference`}
                  </span>
                  {capture.mismatchPixels !== undefined && (
                    <span className={detail}>
                      {capture.mismatchPixels.toLocaleString("en")} mismatched{" "}
                      {capture.mismatchPixels === 1 ? "pixel" : "pixels"}
                    </span>
                  )}
                  <span className={`${detail} muted`}>{capture.threshold}% threshold</span>
                </div>
                {(capture.error || capture.comparisonError) && (
                  <p className="mt-2 line-clamp-2 text-[13px] text-error [line-clamp:2]">
                    {capture.error || capture.comparisonError}
                  </p>
                )}
              </div>
              {list && (
                <div className="capture-comparison grid auto-cols-[minmax(0,1fr)] grid-flow-col gap-4 px-4 pb-4 max-[601px]:gap-2 max-[601px]:px-3 max-[601px]:pb-3">
                  {[
                    {
                      label: "Reference",
                      path: capture.reference,
                      preview: capture.previews?.reference,
                    },
                    { label: "Test", path: capture.test, preview: capture.previews?.test },
                    ...(capture.diff
                      ? [{ label: "Diff", path: capture.diff, preview: capture.previews?.diff }]
                      : []),
                  ].map(({ label, path, preview }) => (
                    <figure className="capture-lane m-0 min-w-0" key={label}>
                      <figcaption className="mb-2 flex items-center justify-center border-b border-b-border bg-surface px-0 pt-0 pb-2 text-center text-[11px] font-medium text-muted">
                        {label}
                      </figcaption>
                      <div className="capture-lane-preview h-[240px] overflow-hidden bg-canvas max-[601px]:h-[160px]">
                        <ReportImage
                          path={path ? preview || path : null}
                          alt={`${capture.label} ${label.toLowerCase()}`}
                          width={640}
                          height={480}
                          className="size-full object-contain object-top"
                          lane
                        />
                      </div>
                    </figure>
                  ))}
                </div>
              )}
            </Link>
          ))}
        </div>
      ) : (
        <StatePanel
          title="No matching captures"
          detail="No captures match the current search and status."
        />
      )}
    </div>
  );
}

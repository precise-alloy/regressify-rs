import { ArrowDown, ArrowUp, ArrowUpRight, Grid2X2, List } from "lucide-react";
import { useDeferredValue, useRef, useState } from "react";
import { Link, useLocation, useParams } from "react-router";
import {
  Filters,
  IconButton,
  ReportImage,
  StatePanel,
  StatusBadge,
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
  return (
    <div className="page suite-page">
      <header className="page-heading">
        <div>
          <div className="eyebrow">Test suite</div>
          <h1>{suiteLabel(summary)}</h1>
          <p className="run-id mono">Run {data.runId || "Unknown"}</p>
        </div>
        <StatusBadge status={summary.failed ? "fail" : "pass"} />
      </header>
      <div className="suite-summary">
        <span>
          <strong>{summary.total}</strong> captures
        </span>
        <span className="text-pass">
          <strong>{summary.passed}</strong> passed
        </span>
        <span className={summary.failed ? "text-fail" : "muted"}>
          <strong>{summary.failed}</strong> failed
        </span>
        <span className="muted">{passRate(summary.passed, summary.total)}% pass rate</span>
      </div>
      <div className="suite-toolbar">
        <Filters
          suite
          counts={{
            all: summary.total,
            pass: summary.passed,
            fail: summary.failed,
            error: data.tests.filter((capture) => capture.error !== null).length,
          }}
        />
        <div className="segmented layout-control">
          <IconButton label="Grid view" aria-pressed={!list} onClick={() => set("view", "")}>
            <Grid2X2 size={17} />
          </IconButton>
          <IconButton label="List view" aria-pressed={list} onClick={() => set("view", "list")}>
            <List size={17} />
          </IconButton>
        </div>
      </div>
      <div className="capture-navigation">
        <span className="capture-count muted mono" aria-live="polite">
          {captures.length} {captures.length === 1 ? "capture" : "captures"}
        </span>
        <div className="failure-navigation">
          <output className="mono muted" aria-label="Failed comparison position" aria-live="polite">
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
        <div className={list ? "capture-list" : "capture-grid"}>
          {captures.map((capture) => (
            <Link
              className="capture-card"
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
                <div className="capture-thumbnail">
                  <ReportImage
                    path={capture.previews?.grid || capture.test || capture.reference}
                    alt={`${capture.label} preview`}
                    width={640}
                    height={400}
                    className={capture.previews?.grid ? "grid-preview" : undefined}
                  />
                  <span className="open-capture">
                    <ArrowUpRight size={18} />
                  </span>
                </div>
              )}
              <div className="capture-info">
                <div className="capture-card-top">
                  <span className="capture-scenario">
                    {capture.scenario || capture.selector || "Capture"}
                  </span>
                  <StatusBadge status={capture.status} error={capture.error !== null} />
                </div>
                <h2>{capture.label}</h2>
                <div className="capture-detail">
                  <span className="mono">
                    {capture.mismatch === null
                      ? "Not compared"
                      : `${formatMismatch(capture.mismatch)}% difference`}
                  </span>
                  {capture.mismatchPixels !== undefined && (
                    <span className="mono">
                      {capture.mismatchPixels.toLocaleString("en")} mismatched{" "}
                      {capture.mismatchPixels === 1 ? "pixel" : "pixels"}
                    </span>
                  )}
                  <span className="mono muted">{capture.threshold}% threshold</span>
                </div>
                {(capture.error || capture.comparisonError) && (
                  <p className="capture-error">{capture.error || capture.comparisonError}</p>
                )}
              </div>
              {list && (
                <div className="capture-comparison">
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
                    <figure className="capture-lane" key={label}>
                      <figcaption>{label}</figcaption>
                      <div className="capture-lane-preview">
                        <ReportImage
                          path={path ? preview || path : null}
                          alt={`${capture.label} ${label.toLowerCase()}`}
                          width={640}
                          height={480}
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

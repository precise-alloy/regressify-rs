import { ArrowLeft, ArrowRight, Image, List } from "lucide-react";
import { Link, useLocation, useParams, useSearchParams } from "react-router";
import { CaptureLog } from "../components/CaptureLog";
import { ComparisonViewer } from "../components/ComparisonViewer";
import {
  IconButton,
  iconButton,
  StatePanel,
  StatusBadge,
  segmented,
  segmentedButton,
  useFilters,
} from "../components/ui";
import { filterCaptures, imagePath, suiteLabel, suitePath } from "../data/report";
import { useSuite } from "../data/store";

export default function ImagePage() {
  const { suiteId, imageId } = useParams();
  const { summary, data, error, retry } = useSuite(suiteId);
  const { query, status, params, set } = useFilters();
  const [, setParams] = useSearchParams();
  const location = useLocation();
  if (!summary) return <StatePanel title="Suite not found" back />;
  if (error) return <StatePanel title="Comparison unavailable" detail={error} retry={retry} />;
  if (!data) return <StatePanel title="Loading comparison" loading />;
  const index = data.tests.findIndex((test) => test.id === imageId);
  const capture = data.tests[index];
  if (!capture)
    return (
      <StatePanel
        title="Image not found"
        detail="This capture is not included in this snapshot."
        back
      />
    );
  const filtered = filterCaptures(data.tests, query, status);
  const sequence = filtered.some((test) => test.id === capture.id) ? filtered : data.tests;
  const sequenceIndex = sequence.findIndex((test) => test.id === capture.id);
  const previous = sequence[sequenceIndex - 1];
  const next = sequence[sequenceIndex + 1];
  const showingLog = params.get("view") === "log";
  const suiteView =
    (showingLog ? params.get("suiteView") : params.get("view")) === "list" ? "list" : "";
  const setLogView = (showLog: boolean) => {
    const nextParams = new URLSearchParams(params);
    const nextView = showLog ? "log" : suiteView;
    if (nextView) nextParams.set("view", nextView);
    else nextParams.delete("view");
    if (showLog && suiteView) nextParams.set("suiteView", suiteView);
    else nextParams.delete("suiteView");
    setParams(nextParams, { replace: true, preventScrollReset: true });
  };
  const backParams = new URLSearchParams(params);
  backParams.delete("mode");
  if (showingLog) {
    if (suiteView) backParams.set("view", suiteView);
    else backParams.delete("view");
  }
  for (const key of ["suiteView", "logRole", "logLevel", "logStage", "logSearch", "logChunk"])
    backParams.delete(key);
  const backSearch = backParams.size ? `?${backParams}` : "";
  return (
    // `image-page` and `back-link` are test hooks. While the comparison viewer is
    // shown, the page is a column that hands its remaining height to the viewer.
    <div
      className={`page image-page max-w-none px-5 pt-2.5 pb-3.5 max-[601px]:px-3 print:p-0 ${showingLog ? "" : "flex h-full min-h-0 w-full flex-col overflow-hidden"}`}
    >
      <header
        className={`page-heading mb-2 flex-nowrap items-center gap-3 max-[601px]:flex-wrap ${showingLog ? "" : "shrink-0"}`}
      >
        <div className="flex items-center gap-4 max-[601px]:flex-col max-[601px]:items-start max-[601px]:gap-0.5">
          <Link
            className="back-link inline-flex shrink-0 items-center gap-1.5 text-[13px] text-muted hover:text-accent"
            to={`${suitePath(summary.id)}${backSearch}`}
          >
            <ArrowLeft size={14} />
            Results
          </Link>
          <h1 className="text-[18px] max-[601px]:text-[17px]">{capture.label}</h1>
        </div>
        <div className="ml-auto flex items-center gap-4 whitespace-nowrap max-[601px]:ml-0 max-[601px]:flex-col max-[601px]:items-start max-[601px]:gap-0.5">
          <StatusBadge status={capture.status} error={capture.error !== null} className="mr-2" />
          <span className="mono muted mr-2 text-[13px]">
            {sequenceIndex + 1} / {sequence.length}
          </span>
          {previous ? (
            <Link
              className={iconButton()}
              aria-label="Previous image"
              title="Previous image"
              to={`${imagePath(summary.id, previous.id)}${location.search}`}
            >
              <ArrowLeft size={17} />
            </Link>
          ) : (
            <IconButton label="Previous image" disabled>
              <ArrowLeft size={17} />
            </IconButton>
          )}
          {next ? (
            <Link
              className={iconButton()}
              aria-label="Next image"
              title="Next image"
              to={`${imagePath(summary.id, next.id)}${location.search}`}
            >
              <ArrowRight size={17} />
            </Link>
          ) : (
            <IconButton label="Next image" disabled>
              <ArrowRight size={17} />
            </IconButton>
          )}
        </div>
      </header>
      <fieldset
        className={`${segmented} mb-3 self-start print:hidden ${showingLog ? "" : "shrink-0"}`}
        aria-label="Capture review view"
      >
        <button
          type="button"
          className={segmentedButton}
          aria-pressed={!showingLog}
          onClick={() => setLogView(false)}
        >
          <Image size={15} />
          Comparison
        </button>
        <button
          type="button"
          className={segmentedButton}
          aria-pressed={showingLog}
          onClick={() => setLogView(true)}
        >
          <List size={15} />
          Capture Log
        </button>
      </fieldset>
      {showingLog ? (
        <CaptureLog
          key={`${summary.id}/${capture.id}/${params.get("logRole")}`}
          capture={capture}
          params={params}
          set={set}
        />
      ) : (
        <ComparisonViewer
          key={`${summary.id}/${capture.id}`}
          capture={capture}
          suiteId={summary.id}
          suiteName={suiteLabel(summary)}
          runId={data.runId}
          mode={params.get("mode")}
          onMode={(mode) => set("mode", mode)}
        />
      )}
    </div>
  );
}

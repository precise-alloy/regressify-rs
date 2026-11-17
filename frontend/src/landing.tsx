import { CheckCircle2, CircleAlert, LoaderCircle, RefreshCw } from "lucide-react";
import { useEffect, useState } from "react";
import { createRoot } from "react-dom/client";
import { ThemeControl } from "./App";
import { IconButton } from "./components/ui";
import { fetchSiteStatus, type SiteStatus } from "./data/landing";
import { passRate } from "./data/report";

const countLabel = "text-[11px] text-muted";
const countValue = "m-0 font-mono [overflow-wrap:anywhere]";

function SiteStatusPanel({ url, heading }: { url: string; heading: string }) {
  const [attempt, setAttempt] = useState(0);
  const [result, setResult] = useState<SiteStatus | "loading" | "unavailable">("loading");
  useEffect(() => {
    const controller = new AbortController();
    setResult("loading");
    void attempt;
    fetchSiteStatus(url, controller.signal).then(
      (status) => {
        if (!controller.signal.aborted) setResult(status);
      },
      () => {
        if (!controller.signal.aborted) setResult("unavailable");
      },
    );
    return () => controller.abort();
  }, [url, attempt]);
  const refresh = (
    <IconButton
      label={`Refresh status for ${heading}`}
      disabled={result === "loading"}
      onClick={() => setAttempt((value) => value + 1)}
      className="ml-auto"
    >
      <RefreshCw size={16} />
    </IconButton>
  );
  if (typeof result === "string") {
    return (
      <div className="muted flex min-h-[34px] items-center gap-2" aria-live="polite">
        {result === "loading" ? (
          <LoaderCircle size={17} className="animate-spin motion-reduce:animate-none" />
        ) : (
          <CircleAlert size={17} />
        )}
        <span>{result === "loading" ? "Loading status" : "Status unavailable"}</span>
        {refresh}
      </div>
    );
  }
  return (
    <div aria-live="polite">
      <div className="flex min-h-[34px] items-center gap-2">
        {result.total === 0 ? (
          <span className="muted flex min-h-[34px] items-center gap-2">No captures</span>
        ) : (
          <span
            className={`flex min-h-[34px] items-center gap-2 ${result.failed ? "text-fail" : "text-pass"}`}
          >
            {result.failed ? <CircleAlert size={17} /> : <CheckCircle2 size={17} />}
            <strong className="text-[22px] font-semibold">
              {passRate(result.passed, result.total)}%
            </strong>{" "}
            passed
          </span>
        )}
        {refresh}
      </div>
      <dl className="my-2.5 grid grid-cols-4 gap-2">
        <div>
          <dt className={countLabel}>Suites</dt>
          <dd className={countValue}>{result.suites}</dd>
        </div>
        <div>
          <dt className={countLabel}>Captures</dt>
          <dd className={countValue}>{result.total}</dd>
        </div>
        <div>
          <dt className={countLabel}>Failed</dt>
          <dd className={`${countValue} ${result.failed ? "text-fail" : ""}`}>{result.failed}</dd>
        </div>
        <div>
          <dt className={countLabel}>Errors</dt>
          <dd className={`${countValue} ${result.errors ? "text-fail" : ""}`}>{result.errors}</dd>
        </div>
      </dl>
      <time
        className="mono muted text-[10px]"
        dateTime={result.generatedAt}
        title="Report generated at"
      >
        {new Date(result.generatedAt).toLocaleString(undefined, {
          dateStyle: "medium",
          timeStyle: "short",
        })}
      </time>
    </div>
  );
}

export function enhanceLandingPage(): boolean {
  if (!document.querySelector("[data-regressify-landing]")) return false;
  const controls = document.getElementById("landing-controls");
  if (controls) createRoot(controls).render(<ThemeControl />);
  for (const element of document.querySelectorAll<HTMLElement>("[data-regressify-status]")) {
    createRoot(element).render(
      <SiteStatusPanel
        url={element.dataset.regressifyStatus || ""}
        heading={element.dataset.siteHeading || "site"}
      />,
    );
  }
  return true;
}

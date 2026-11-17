import {
  autoUpdate,
  FloatingPortal,
  flip,
  offset,
  safePolygon,
  shift,
  useClick,
  useDismiss,
  useFloating,
  useFocus,
  useHover,
  useInteractions,
  useRole,
} from "@floating-ui/react";
import { ArrowDown, ArrowUp, ArrowUpDown, Check, X } from "lucide-react";
import { useState } from "react";
import { Link, useSearchParams } from "react-router";
import { Filters, IconButton, StatePanel, statusDot, useFilters } from "../components/ui";
import { passRate, suiteLabel, suitePath } from "../data/report";
import { useManifest } from "../data/store";

const columns = [
  { key: "name", label: "Test suite" },
  { key: "total", label: "Total tests" },
  { key: "passed", label: "Passed" },
  { key: "failed", label: "Failed" },
  { key: "rate", label: "Pass rate" },
] as const;

function EngineErrorCount({ count }: { count: number }) {
  const [open, setOpen] = useState(false);
  const { refs, floatingStyles, context } = useFloating({
    open,
    onOpenChange: setOpen,
    placement: "top",
    strategy: "fixed",
    middleware: [offset(6), flip({ padding: 12 }), shift({ padding: 12 })],
    whileElementsMounted: autoUpdate,
  });
  const hover = useHover(context, { move: false, handleClose: safePolygon() });
  const focus = useFocus(context, { visibleOnly: false });
  const click = useClick(context);
  const dismiss = useDismiss(context);
  const role = useRole(context, { role: "tooltip" });
  const { getReferenceProps, getFloatingProps } = useInteractions([
    hover,
    focus,
    click,
    dismiss,
    role,
  ]);
  const label = `${count} engine ${count === 1 ? "error" : "errors"}`;
  return (
    <>
      <button
        type="button"
        ref={refs.setReference}
        className="table-error mono inline-flex h-7 min-w-7 shrink-0 items-center justify-center rounded-[4px] border border-[color-mix(in_srgb,var(--error)_30%,var(--border))] bg-error-soft px-1.5 text-[12px] text-error"
        aria-label={label}
        {...getReferenceProps()}
      >
        {count}
      </button>
      {open && (
        <FloatingPortal>
          <div
            ref={refs.setFloating}
            style={floatingStyles}
            className="z-40 max-w-[calc(100vw-24px)] rounded-[4px] bg-foreground px-2.5 py-1.5 text-[12px] text-surface [overflow-wrap:anywhere]"
            {...getFloatingProps()}
          >
            {label}
          </div>
        </FloatingPortal>
      )}
    </>
  );
}

// Table cells. Below 761px each row reflows into a card: the suite name on top,
// then one labelled line per value (the label comes from `data-label`).
const valueCell = `border-b border-b-border bg-surface px-3 py-[15px] group-hover/row:bg-subtle max-[761px]:min-h-[44px] max-[761px]:min-w-0 max-[761px]:items-center max-[761px]:justify-between max-[761px]:gap-3 max-[761px]:px-3 max-[761px]:py-2.5 text-center max-[761px]:before:shrink-0 max-[761px]:before:font-sans max-[761px]:before:text-left max-[761px]:before:text-[11px] max-[761px]:before:text-muted max-[761px]:before:uppercase max-[761px]:before:content-[attr(data-label)]`;
const countCell = `mono ${valueCell} max-[761px]:grid max-[761px]:grid-cols-[minmax(0,1fr)_64px]`;
const countValue =
  "max-[761px]:max-w-full max-[761px]:justify-self-center max-[761px]:[overflow-wrap:anywhere]";
// `result-count` with its `pass`/`fail` modifier is a test hook.
const resultCount = (tone: "pass" | "fail" | null) =>
  `result-count inline-block min-w-[34px] rounded-[4px] px-2 py-0.5 ${tone === "pass" ? "pass bg-accent-soft text-pass" : tone === "fail" ? "fail bg-fail-soft text-fail" : "text-muted"}`;
// The metric tiles form one row, or a 2x2 grid below 601px.
const metric = (index: number) =>
  [
    "flex flex-col items-center gap-1 border-r border-r-border px-3 text-center last:[border-right:0]",
    index % 2 === 1 && "max-[601px]:[border-right:0]",
    index < 2
      ? "max-[601px]:pb-2"
      : "max-[601px]:border-t max-[601px]:border-t-border max-[601px]:pt-2",
  ]
    .filter(Boolean)
    .join(" ");
const metricLabel = "flex items-center gap-2 text-[12px] text-muted";
const metricValue = "text-[28px] leading-[1.2] font-bold tabular-nums";

export function OverviewPage() {
  const manifest = useManifest();
  const { query, status } = useFilters();
  const [params, setParams] = useSearchParams();
  const totals = manifest.suites.reduce(
    (sum, suite) => ({
      total: sum.total + suite.total,
      passed: sum.passed + suite.passed,
      failed: sum.failed + suite.failed,
    }),
    { total: 0, passed: 0, failed: 0 },
  );
  const counts = {
    all: manifest.suites.length,
    pass: manifest.suites.filter((suite) => !suite.failed).length,
    fail: manifest.suites.filter((suite) => suite.failed).length,
    error: manifest.suites.filter((suite) => suite.errors).length,
  };
  const sort = columns.find((column) => column.key === params.get("sort"))?.key || "name";
  const descending = params.get("direction") === "desc";
  const needle = query.trim().toLowerCase();
  const suites = manifest.suites
    .filter(
      (suite) =>
        (!needle || `${suite.id} ${suiteLabel(suite)}`.toLowerCase().includes(needle)) &&
        (status === "all" ||
          (status === "error"
            ? suite.errors > 0
            : status === "fail"
              ? suite.failed > 0
              : suite.failed === 0)),
    )
    .sort((first, second) => {
      const order =
        sort === "name"
          ? suiteLabel(first).localeCompare(suiteLabel(second))
          : sort === "rate"
            ? first.passed / (first.total || 1) - second.passed / (second.total || 1)
            : first[sort] - second[sort];
      return descending ? -order : order;
    });
  const toggleSort = (key: (typeof columns)[number]["key"]) => {
    const next = new URLSearchParams(params);
    next.set("sort", key);
    if (sort === key && !descending) next.set("direction", "desc");
    else next.delete("direction");
    setParams(next, { replace: true, preventScrollReset: true });
  };
  return (
    <div className="page overview-page">
      <header className="page-heading">
        <div>
          <h1 className="text-[20px]">Snapshot Overview</h1>
          <time className="mono mt-1 block text-[11px] text-muted" dateTime={manifest.generatedAt}>
            {new Date(manifest.generatedAt).toLocaleString(undefined, {
              dateStyle: "medium",
              timeStyle: "short",
            })}
          </time>
        </div>
        <span
          className={`inline-flex items-center gap-[7px] rounded-[5px] border bg-surface px-2 py-1 text-[11px] font-medium whitespace-nowrap ${totals.failed ? "border-[color-mix(in_srgb,var(--fail)_24%,var(--border))] text-fail" : "border-border text-pass"}`}
        >
          {totals.failed ? <X size={16} /> : <Check size={16} />}
          {totals.failed
            ? "Changes detected"
            : totals.total
              ? "All checks passed"
              : "No comparisons"}
        </span>
      </header>
      <section
        className="metrics mt-4 mb-5 grid grid-cols-4 border-y border-y-border py-4 max-[601px]:grid-cols-2 max-[601px]:gap-0"
        aria-label="Snapshot totals"
      >
        <div className={metric(0)}>
          <span className={metricLabel}>TOTAL TESTS</span>
          <strong className={metricValue}>{totals.total}</strong>
        </div>
        <div className={metric(1)}>
          <span className={metricLabel}>PASSED</span>
          <strong
            className={`${metricValue}${totals.total > 0 && !totals.failed ? " text-pass" : ""}`}
          >
            {totals.passed}
          </strong>
        </div>
        <div className={metric(2)}>
          <span className={metricLabel}>FAILED</span>
          <strong className={`${metricValue}${totals.failed ? " text-fail" : ""}`}>
            {totals.failed}
          </strong>
        </div>
        <div className={metric(3)}>
          <span className={metricLabel}>PASS RATE</span>
          <strong className={metricValue}>{passRate(totals.passed, totals.total)}%</strong>
        </div>
      </section>
      <div className="section-title mb-[18px] flex items-center gap-3">
        <h2 className="text-[17px] font-semibold">Test suites</h2>
        <span className="mono muted">
          {suites.length} / {manifest.suites.length}
        </span>
      </div>
      <Filters counts={counts} />
      {suites.length ? (
        <section
          className="results-table-wrap overflow-x-auto border-t border-t-border"
          aria-label="Test suite results"
        >
          <div className="hidden max-[761px]:flex max-[761px]:items-center max-[761px]:gap-2 max-[761px]:py-3 max-[761px]:text-[12px] max-[761px]:text-muted">
            <label htmlFor="overview-sort">Sort by</label>
            <select
              id="overview-sort"
              aria-label="Sort suites by"
              className="max-[761px]:h-[34px] max-[761px]:min-w-0 max-[761px]:flex-1 max-[761px]:rounded-[4px] max-[761px]:border max-[761px]:border-border max-[761px]:bg-surface max-[761px]:px-2 max-[761px]:py-1 max-[761px]:text-foreground"
              value={sort}
              onChange={(event) =>
                toggleSort(
                  columns.find((column) => column.key === event.target.value)?.key || "name",
                )
              }
            >
              {columns.map(({ key, label }) => (
                <option key={key} value={key}>
                  {label}
                </option>
              ))}
            </select>
            <IconButton
              label={descending ? "Sort ascending" : "Sort descending"}
              className="max-[761px]:border max-[761px]:border-border max-[761px]:bg-surface"
              onClick={() => toggleSort(sort)}
            >
              {descending ? <ArrowDown size={16} /> : <ArrowUp size={16} />}
            </IconButton>
          </div>
          <table className="results-table w-full min-w-[620px] table-fixed border-collapse text-left [--suite-cell-padding:16px] [--suite-name-gap:12px] max-[761px]:block max-[761px]:min-w-0 max-[761px]:[--suite-cell-padding:12px] max-[761px]:[--suite-name-gap:8px]">
            <thead className="max-[761px]:sr-only">
              <tr>
                {columns.map(({ key, label }, index) => (
                  <th
                    key={key}
                    scope="col"
                    className={`border-b border-b-border px-3 py-[13px] text-[13px] font-medium text-muted ${index === 0 ? "w-[40%] pl-[calc(var(--suite-cell-padding)+7px+var(--suite-name-gap))] max-[960px]:w-[34%]" : "text-center"}${index === columns.length - 1 ? " w-[24%]" : ""}`}
                    aria-sort={sort === key ? (descending ? "descending" : "ascending") : "none"}
                  >
                    <button
                      type="button"
                      className={`flex items-center gap-2 [border:0] p-0 text-left max-[761px]:hidden ${index ? "mx-auto" : ""}`}
                      onClick={() => toggleSort(key)}
                    >
                      {label}
                      {sort !== key ? (
                        <ArrowUpDown size={13} aria-hidden="true" />
                      ) : descending ? (
                        <ArrowDown size={13} aria-hidden="true" />
                      ) : (
                        <ArrowUp size={13} aria-hidden="true" />
                      )}
                    </button>
                    <span className="hidden max-[761px]:inline">{label}</span>
                  </th>
                ))}
              </tr>
            </thead>
            <tbody className="max-[761px]:block">
              {suites.map((suite) => (
                <tr
                  key={suite.id}
                  className="group/row max-[761px]:mb-4 max-[761px]:grid max-[761px]:grid-cols-[minmax(0,1fr)] max-[761px]:border-t max-[761px]:border-t-border max-[761px]:last:mb-0"
                >
                  <td className="border-b border-b-border bg-surface px-3 py-[15px] group-hover/row:bg-subtle max-[761px]:min-h-[44px] max-[761px]:min-w-0 max-[761px]:items-center max-[761px]:justify-between max-[761px]:gap-3 max-[761px]:px-3 pl-[var(--suite-cell-padding)] max-[761px]:block max-[761px]:bg-subtle max-[761px]:py-3">
                    <div className="flex min-w-0 items-center gap-2">
                      <Link
                        className="suite-name inline-flex min-w-0 items-center gap-[var(--suite-name-gap)] text-[15px] font-medium hover:text-accent max-[601px]:text-[14px]"
                        to={suitePath(suite.id)}
                      >
                        <span className={statusDot(suite.failed > 0)} />
                        <span className="suite-label min-w-0">{suiteLabel(suite)}</span>
                      </Link>
                      {suite.errors > 0 && <EngineErrorCount count={suite.errors} />}
                    </div>
                  </td>
                  <td className={countCell} data-label="Total tests">
                    <span className={countValue}>{suite.total}</span>
                  </td>
                  <td className={countCell} data-label="Passed">
                    <span
                      className={`${countValue} ${resultCount(suite.total > 0 && !suite.failed ? "pass" : null)}`}
                    >
                      {suite.passed}
                    </span>
                  </td>
                  <td className={countCell} data-label="Failed">
                    <span className={`${countValue} ${resultCount(suite.failed ? "fail" : null)}`}>
                      {suite.failed}
                    </span>
                  </td>
                  <td className={`${valueCell} max-[761px]:flex`} data-label="Pass rate">
                    <div className="flex items-center gap-3 max-[761px]:w-[144px] max-[761px]:max-w-[60%]">
                      <progress
                        className={`h-1 w-[72px] flex-1 appearance-none rounded-none [border:0] bg-border [&::-moz-progress-bar]:bg-current [&::-webkit-progress-bar]:bg-border [&::-webkit-progress-value]:bg-current ${suite.failed ? "text-muted" : "text-pass"}`}
                        value={suite.passed}
                        max={suite.total || 1}
                        aria-label={`${suiteLabel(suite)} pass rate`}
                      />
                      <span className="mono w-[52px] text-right text-[13px]">
                        {passRate(suite.passed, suite.total)}%
                      </span>
                    </div>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </section>
      ) : (
        <StatePanel
          title={manifest.suites.length ? "No matching suites" : "No comparisons yet"}
          detail={
            manifest.suites.length
              ? "No suites match the current search and status."
              : "This snapshot contains no comparison results."
          }
        />
      )}
    </div>
  );
}

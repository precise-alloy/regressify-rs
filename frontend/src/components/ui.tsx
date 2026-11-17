import {
  AlertCircle,
  ArrowLeft,
  Check,
  ImageOff,
  LoaderCircle,
  RotateCcw,
  Search,
  X,
} from "lucide-react";
import { type ButtonHTMLAttributes, useEffect, useRef, useState } from "react";
import { Link, useSearchParams } from "react-router";
import type { StatusFilter } from "../data/report";

/** Pressed state for items inside a segmented control. */
export const segmentedItem =
  "aria-pressed:bg-surface aria-pressed:text-foreground aria-pressed:[box-shadow:0_1px_3px_#00000012]";

/** A bordered group of toggle buttons (theme, layout, filters, modes). */
export const segmented =
  "inline-flex items-center gap-0.5 rounded-[6px] border border-border bg-subtle p-[3px]";

/** Text buttons inside the review tabs and the log execution toggle. */
export const segmentedButton = `inline-flex min-h-8 items-center justify-center gap-1.5 rounded-[3px] [border:0] px-2.5 py-1.5 text-[12px] ${segmentedItem}`;

/** Bordered text button (Retry, Overview). */
export const button =
  "inline-flex items-center justify-center gap-2 rounded-[5px] border border-border bg-surface px-3.5 py-2 text-[12px] text-foreground";

/**
 * Square icon control, also used on links. `compact` is the smaller size used
 * inside segmented controls.
 */
export function iconButton(compact = false) {
  return `inline-flex shrink-0 items-center justify-center rounded-[4px] [border:0] bg-transparent text-muted hover:bg-surface hover:text-foreground aria-pressed:bg-surface aria-pressed:text-foreground ${compact ? `h-[28px] w-[30px] ${segmentedItem}` : "size-[34px]"}`;
}

export function IconButton({
  label,
  children,
  className,
  compact = false,
  ...props
}: ButtonHTMLAttributes<HTMLButtonElement> & { label: string; compact?: boolean }) {
  return (
    <button
      type="button"
      className={className ? `${iconButton(compact)} ${className}` : iconButton(compact)}
      aria-label={label}
      title={label}
      {...props}
    >
      {children}
    </button>
  );
}

/** Pass/fail dot; `status-dot` is a test hook. */
export function statusDot(failed: boolean) {
  return `status-dot inline-block size-[7px] shrink-0 rounded-[50%] ${failed ? "bg-fail" : "bg-pass"}`;
}

export function StatusBadge({
  status,
  error = false,
  className = "",
}: {
  status: "pass" | "fail";
  error?: boolean;
  className?: string;
}) {
  return (
    <span
      className={`status-badge inline-flex shrink-0 items-center justify-center gap-1 rounded-[4px] px-[7px] py-[3px] text-[13px] font-medium whitespace-nowrap ${error ? "bg-error-soft text-error" : status === "pass" ? "bg-accent-soft text-pass" : "bg-fail-soft text-fail"} ${className}`}
    >
      {error ? (
        <AlertCircle size={13} />
      ) : status === "pass" ? (
        <Check size={13} />
      ) : (
        <X size={13} />
      )}
      {error ? "Error" : status === "pass" ? "Passed" : "Failed"}
    </span>
  );
}

export function StatePanel({
  title,
  detail,
  retry,
  loading,
  back,
}: {
  title: string;
  detail?: string;
  retry?: () => void;
  loading?: boolean;
  back?: boolean;
}) {
  return (
    <section
      className="state-panel flex min-h-[300px] flex-col items-center justify-center gap-3 px-6 py-[72px] text-center text-muted"
      role={loading ? "status" : undefined}
      aria-live="polite"
    >
      {loading ? <LoaderCircle size={28} className="animate-spin" /> : <AlertCircle size={28} />}
      <h1 className="text-[21px] font-[550] text-foreground">{title}</h1>
      {detail && <p className="max-w-[480px] text-[13px]">{detail}</p>}
      {retry && (
        <button className={button} type="button" onClick={retry}>
          <RotateCcw size={16} /> Retry
        </button>
      )}
      {back && (
        <Link className={button} to="/">
          <ArrowLeft size={16} /> Overview
        </Link>
      )}
    </section>
  );
}

const previewObservers = new Map<
  Element | null,
  { observer: IntersectionObserver; targets: Map<Element, (visible: boolean) => void> }
>();

function observePreview(element: HTMLImageElement, update: (visible: boolean) => void) {
  const root = null;
  let group = previewObservers.get(root);
  if (!group) {
    const targets = new Map<Element, (visible: boolean) => void>();
    const observer = new IntersectionObserver(
      (entries) => {
        for (const entry of entries) targets.get(entry.target)?.(entry.isIntersecting);
      },
      { root, rootMargin: "600px 0px" },
    );
    group = { observer, targets };
    previewObservers.set(root, group);
  }
  const { observer, targets } = group;
  targets.set(element, update);
  observer.observe(element);
  return () => {
    observer.unobserve(element);
    targets.delete(element);
    if (!targets.size) {
      observer.disconnect();
      previewObservers.delete(root);
    }
  };
}

export function ReportImage({
  path,
  alt,
  width,
  height,
  className,
  eager = false,
  lane = false,
}: {
  path: string | null;
  alt: string;
  width: number;
  height: number;
  className?: string;
  eager?: boolean;
  /** Smaller fallback for the list view's preview lanes. */
  lane?: boolean;
}) {
  const [failed, setFailed] = useState<string>();
  const [visible, setVisible] = useState(false);
  const image = useRef<HTMLImageElement>(null);
  useEffect(() => {
    if (eager || !path || failed === path || !image.current) return;
    return observePreview(image.current, setVisible);
  }, [eager, path, failed]);
  const active = eager || visible;
  return path && failed !== path ? (
    <img
      ref={image}
      className={className}
      src={active ? `/${path}` : undefined}
      alt={alt}
      width={width}
      height={height}
      style={active ? undefined : { visibility: "hidden" }}
      loading={eager ? "eager" : "lazy"}
      decoding="async"
      onError={() => setFailed(path)}
    />
  ) : (
    <div
      className={`image-unavailable flex size-full flex-col items-center justify-center gap-2 text-[12px] text-muted ${lane ? "min-h-0 p-1 text-center [overflow-wrap:anywhere]" : "min-h-[120px]"}`}
    >
      <ImageOff size={24} />
      <span>{path ? "Image unavailable" : "No image"}</span>
    </div>
  );
}

export function useFilters() {
  const [params, setParams] = useSearchParams();
  const statusParam = params.get("status");
  const status: StatusFilter =
    statusParam === "pass" || statusParam === "fail" || statusParam === "error"
      ? statusParam
      : "all";
  const set = (key: string, value: string) => {
    const next = new URLSearchParams(params);
    if (value && value !== "all") next.set(key, value);
    else next.delete(key);
    setParams(next, { replace: true, preventScrollReset: true });
  };
  return { query: params.get("q") || "", status, params, set };
}

export function Filters({
  counts,
  suite = false,
}: {
  counts: { all: number; pass: number; fail: number; error: number };
  suite?: boolean;
}) {
  const { query, status, set } = useFilters();
  return (
    <div
      className={`mb-5 flex min-w-0 flex-wrap items-center gap-3 print:hidden ${suite ? "flex-1 max-[601px]:mb-3 max-[601px]:basis-full" : ""}`}
    >
      <label className="flex h-[38px] min-w-[150px] flex-1 items-center gap-2 rounded-[5px] border border-border bg-surface px-3 text-muted focus-within:outline-2 focus-within:outline-offset-1 focus-within:outline-accent max-[601px]:basis-full">
        <Search size={16} />
        <input
          className="w-full min-w-0 [border:0] bg-transparent text-[14px] text-foreground [outline:0]"
          type="search"
          aria-label={suite ? "Search captures" : "Search suites"}
          placeholder={suite ? "Search captures..." : "Search suites..."}
          value={query}
          onChange={(event) => set("q", event.target.value)}
        />
      </label>
      <fieldset className={`${segmented} max-[601px]:w-full`} aria-label="Filter results">
        {(
          [
            ["all", "All"],
            ["fail", "Failed"],
            ["pass", "Passed"],
            ["error", "Errors"],
          ] as const
        ).map(([value, label]) => (
          <button
            type="button"
            key={value}
            aria-pressed={status === value}
            onClick={() => set("status", value)}
            className={`flex min-h-[30px] items-center justify-center gap-[7px] rounded-[3px] [border:0] bg-transparent px-[11px] py-[5px] text-[13px] text-muted max-[601px]:flex-1 max-[601px]:gap-[3px] max-[601px]:p-1.5 max-[601px]:text-[12px] ${segmentedItem}`}
          >
            {label}
            <span className="mono min-w-[15px] px-[3px] text-[12px] max-[601px]:text-[11px]">
              {counts[value]}
            </span>
          </button>
        ))}
      </fieldset>
    </div>
  );
}

import {
  autoUpdate,
  FloatingPortal,
  flip,
  offset,
  safePolygon,
  shift,
  useDismiss,
  useFloating,
  useFocus,
  useHover,
  useInteractions,
  useRole,
} from "@floating-ui/react";
import { ExternalLink, Info, X } from "lucide-react";
import { useId, useState } from "react";
import { type Capture, formatMismatch, safePageUrl } from "../data/report";
import { IconButton, iconButton } from "./ui";

// Metadata terms and values.
const term = "mb-[7px] text-[13px] text-muted";
const value = "m-0 [overflow-wrap:anywhere]";

export function CaptureInfo({
  capture,
  suiteId,
  suiteName,
  runId,
  dimensions,
}: {
  capture: Capture;
  suiteId: string;
  suiteName: string;
  runId: string;
  dimensions: { label: string; width: number; height: number }[];
}) {
  const [open, setOpen] = useState(false);
  const [pinned, setPinned] = useState(false);
  const titleId = useId();
  const { refs, floatingStyles, context } = useFloating({
    open,
    onOpenChange(next, _event, reason) {
      if (pinned && (reason === "hover" || reason === "safe-polygon" || reason === "focus")) return;
      setOpen(next);
      if (!next) setPinned(false);
    },
    placement: "bottom-end",
    strategy: "fixed",
    middleware: [offset(8), flip({ padding: 12 }), shift({ padding: 12, crossAxis: true })],
    whileElementsMounted: autoUpdate,
  });
  const hover = useHover(context, {
    move: false,
    handleClose: safePolygon({ blockPointerEvents: false }),
  });
  const focus = useFocus(context, { visibleOnly: false });
  const dismiss = useDismiss(context);
  const role = useRole(context, { role: "dialog" });
  const { getReferenceProps, getFloatingProps } = useInteractions([hover, focus, dismiss, role]);
  const close = () => {
    setOpen(false);
    setPinned(false);
  };
  return (
    <>
      <button
        type="button"
        className={iconButton()}
        ref={refs.setReference}
        aria-label="Capture information"
        {...getReferenceProps({
          onClick: () => {
            setPinned(!pinned);
            setOpen(!pinned);
          },
        })}
      >
        <Info size={18} />
      </button>
      {open && (
        <FloatingPortal>
          <dialog
            open
            ref={refs.setFloating}
            style={floatingStyles}
            className="z-50 m-0 max-h-[min(640px,calc(100dvh-24px))] w-[min(460px,calc(100vw-24px))] overflow-y-auto overscroll-contain rounded-[6px] border border-border bg-surface p-4 text-foreground select-text [box-shadow:0_8px_32px_#00000026]"
            aria-labelledby={titleId}
            {...getFloatingProps()}
          >
            <div className="mb-3 flex items-center justify-between gap-3">
              <h2 id={titleId} className="text-[15px] font-semibold">
                Capture information
              </h2>
              <IconButton label="Close capture information" onClick={close}>
                <X size={16} />
              </IconButton>
            </div>
            {/* `capture-metadata` is a test hook. */}
            <dl className="capture-metadata m-0 grid grid-cols-2 gap-x-5 gap-y-3.5 text-[14px] max-[601px]:grid-cols-[minmax(0,1fr)] max-[601px]:gap-4">
              <div>
                <dt className={term}>Difference</dt>
                <dd
                  className={`mono ${value} ${capture.status === "fail" ? "text-fail" : "text-pass"}`}
                >
                  {capture.mismatch === null
                    ? "Not compared"
                    : `${formatMismatch(capture.mismatch)}%`}
                </dd>
              </div>
              {capture.mismatchPixels !== undefined && (
                <div>
                  <dt className={term}>Mismatched pixels</dt>
                  <dd className={`mono ${value}`}>{capture.mismatchPixels.toLocaleString("en")}</dd>
                </div>
              )}
              <div>
                <dt className={term}>Threshold</dt>
                <dd className={`mono ${value}`}>{capture.threshold}%</dd>
              </div>
              <div>
                <dt className={term}>Suite</dt>
                <dd title={suiteId} className={value}>
                  {suiteName}
                </dd>
              </div>
              <div>
                <dt className={term}>Scenario</dt>
                <dd className={value}>{capture.scenario || "Unspecified"}</dd>
              </div>
              <div>
                <dt className={term}>Capture</dt>
                <dd className={`mono ${value}`}>{capture.id}</dd>
              </div>
              <div>
                <dt className={term}>Run</dt>
                <dd className={`mono ${value}`}>{runId || "Unknown"}</dd>
              </div>
              <div>
                <dt className={term}>Selector</dt>
                <dd className={`mono ${value}`}>{capture.selector || "Document"}</dd>
              </div>
              {dimensions.map((size) => (
                <div key={size.label}>
                  <dt className={term}>{size.label} size</dt>
                  <dd className={`mono ${value}`}>
                    {size.width} x {size.height}
                  </dd>
                </div>
              ))}
              {capture.dimensions && !capture.dimensions.same && (
                <div className="col-span-full">
                  <dt className={term}>Dimension difference</dt>
                  <dd className={`mono ${value}`}>
                    {capture.dimensions.width > 0 ? "+" : ""}
                    {capture.dimensions.width}px width, {capture.dimensions.height > 0 ? "+" : ""}
                    {capture.dimensions.height}px height
                  </dd>
                </div>
              )}
              {(
                [
                  ["Reference page", capture.referenceUrl],
                  ["Test page", capture.testUrl],
                ] as const
              ).map(
                ([label, url]) =>
                  safePageUrl(url) && (
                    <div className="col-span-full" key={label}>
                      <dt className={term}>{label}</dt>
                      <dd className={value}>
                        <a
                          href={safePageUrl(url)}
                          target="_blank"
                          rel="noopener noreferrer"
                          className="link inline-flex items-center gap-1.5"
                        >
                          {url}
                          <ExternalLink size={13} />
                        </a>
                      </dd>
                    </div>
                  ),
              )}
            </dl>
            {(capture.error || capture.comparisonError) && (
              <div
                className="mt-4 border-l-[3px] border-l-error bg-error-soft p-4 text-[12px] text-error"
                role="alert"
              >
                <strong>{capture.error ? "Engine error" : "Comparison failed"}</strong>
                <pre className="mt-2 [font:11px_var(--font-mono)] whitespace-pre-wrap">
                  {capture.error || capture.comparisonError}
                </pre>
              </div>
            )}
          </dialog>
        </FloatingPortal>
      )}
    </>
  );
}

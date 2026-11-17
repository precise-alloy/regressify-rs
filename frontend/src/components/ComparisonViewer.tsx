import {
  Columns2,
  Contrast,
  Download,
  Focus,
  FoldHorizontal,
  Image,
  Layers2,
  Minus,
  Plus,
  Scan,
  SquareSplitHorizontal,
} from "lucide-react";
import { useEffect, useEffectEvent, useLayoutEffect, useRef, useState } from "react";
import {
  alignedRowToImageRow,
  type Capture,
  imageAlignmentGaps,
  imageRowToAlignedRow,
} from "../data/report";
import { CaptureInfo } from "./CaptureInfo";
import { IconButton, iconButton, ReportImage, segmented, segmentedItem } from "./ui";

// One-screen-pixel white crosshair strokes with a dark outline.
const guideLine = "absolute bg-[#ffffff] [box-shadow:0_0_0_1px_#000000b3]";
const modes = [
  ["side", "Side by side", Columns2],
  ["wipe", "Wipe", SquareSplitHorizontal],
  ["overlay", "Overlay", Layers2],
  ["diff", "Diff", Contrast],
  ["reference", "Reference", Image],
  ["test", "Test", Image],
] as const;
type Mode = (typeof modes)[number][0];
type Size = { width: number; height: number };
type Fit = "width" | "area" | "manual";
type View = {
  fit: Fit;
  scale: number;
  // Fractions of unused lane space keep fitted dragging bounded after resize.
  horizontalPosition?: number;
  verticalPosition?: number;
  padding?: { left: number; top: number; right: number; bottom: number }[];
};
const fallback: Size = { width: 640, height: 400 };

const zoomLevels = [
  0.25,
  1 / 3,
  0.5,
  2 / 3,
  0.75,
  0.8,
  0.9,
  1,
  1.1,
  1.25,
  1.5,
  1.75,
  2,
  2.5,
  3,
  4,
  5,
  6,
  8,
];

export function nextZoom(current: number, increase: boolean): number {
  return increase
    ? (zoomLevels.find((value) => value > current + 0.001) ?? 8)
    : (zoomLevels.findLast((value) => value < current - 0.001) ?? Math.min(current, 0.25));
}

export function imageFitScale(image: Size, viewport: Size, fit: Fit): number {
  return Math.max(
    0.001,
    Math.min(1, viewport.width / image.width, fit === "area" ? viewport.height / image.height : 1),
  );
}

function useImageSizes(capture: Capture) {
  const [sizes, setSizes] = useState<Record<string, Size>>({});
  useEffect(() => {
    const paths = new Set(
      [capture.reference, capture.test, capture.diff].filter(
        (path): path is string => path !== null,
      ),
    );
    const images = Array.from(paths, (path) => {
      const image = new window.Image();
      image.onload = () =>
        setSizes((current) => {
          const existing = current[path];
          return existing?.width === image.naturalWidth && existing?.height === image.naturalHeight
            ? current
            : {
                ...current,
                [path]: { width: image.naturalWidth, height: image.naturalHeight },
              };
        });
      image.src = `/${path}`;
      return image;
    });
    return () => {
      for (const image of images) {
        image.onload = null;
        image.src = "";
      }
    };
  }, [capture.reference, capture.test, capture.diff]);
  return {
    getSize: (path: string | null) =>
      path
        ? sizes[path] ||
          (path === capture.reference
            ? capture.imageSizes?.reference
            : path === capture.test
              ? capture.imageSizes?.test
              : capture.imageSizes?.diff)
        : undefined,
    isLoaded: (path: string | null) => !!path && !!sizes[path],
  };
}

export function ComparisonViewer({
  capture,
  mode: requestedMode,
  onMode,
  suiteId,
  suiteName,
  runId,
}: {
  capture: Capture;
  mode: string | null;
  onMode: (mode: string) => void;
  suiteId: string;
  suiteName: string;
  runId: string;
}) {
  const { getSize, isLoaded } = useImageSizes(capture);
  const [blend, setBlend] = useState(50);
  const [zoom, setZoom] = useState<{
    capture: Capture;
    views: Partial<Record<Mode, View>>;
  } | null>(null);
  const [crosshair, setCrosshair] = useState<{
    capture: Capture;
    scale: number;
    positions: ({
      x: number;
      y: number;
      left: number;
      top: number;
      width: number;
      height: number;
    } | null)[];
  } | null>(null);
  const [viewport, setViewport] = useState<Size>({ width: 0, height: 0 });
  const [panning, setPanning] = useState(false);
  const world = useRef<HTMLDivElement>(null);
  const viewer = useRef<HTMLElement>(null);
  const canvas = useRef<HTMLElement>(null);
  const activePane = useRef<HTMLElement | null>(null);
  const crosshairPointer = useRef<{ capture: Capture; index: number; x: number; y: number } | null>(
    null,
  );
  const scrollPositions = useRef(new WeakMap<HTMLElement, { left: number; top: number }>());
  const toolbar = useRef<HTMLDivElement>(null);
  const gestureScope = useRef({ capture, mode: requestedMode });
  const anchors = useRef<
    {
      x: number;
      y: number;
      imageX: number;
      imageY: number;
      element: HTMLElement;
      scroll: HTMLElement;
    }[]
  >([]);
  const pan = useRef<{
    pointer: number;
    x: number;
    y: number;
    left: number;
    top: number;
    imageLeft: number;
    horizontalSpace: number;
    imageTop: number;
    verticalSpace: number;
    moved: boolean;
    scroll: HTMLElement;
  } | null>(null);
  const [compact, setCompact] = useState(() => matchMedia("(max-width: 600px)").matches);
  useEffect(() => {
    const media = matchMedia("(max-width: 600px)");
    const update = () => setCompact(media.matches);
    media.addEventListener("change", update);
    return () => media.removeEventListener("change", update);
  }, []);
  const available = (value: string) =>
    value === "side" ||
    (value === "reference"
      ? !!capture.reference
      : value === "test"
        ? !!capture.test
        : value === "diff"
          ? !!capture.diff
          : !!capture.reference && !!capture.test);
  const mode: Mode =
    modes.find(([value]) => value === requestedMode && available(value))?.[0] ||
    (compact && capture.reference && capture.test ? "wipe" : "side");
  const referenceSize = getSize(capture.reference) || fallback;
  const testSize = getSize(capture.test) || fallback;
  const diffSize = getSize(capture.diff);
  const lanes = [
    {
      label: "Reference",
      path: capture.reference,
      size: referenceSize,
      gap: imageAlignmentGaps(
        capture.alignment?.referenceGaps ?? capture.alignment?.reference,
        referenceSize.height,
        diffSize?.height,
      ),
    },
    {
      label: "Test",
      path: capture.test,
      size: testSize,
      gap: imageAlignmentGaps(
        capture.alignment?.testGaps ?? capture.alignment?.test,
        testSize.height,
        diffSize?.height,
      ),
    },
    ...(capture.diff
      ? [{ label: "Diff", path: capture.diff, size: diffSize || fallback, gap: null }]
      : []),
  ];
  const laneWidth = Math.max(...lanes.map((lane) => lane.size.width));
  const single =
    mode === "reference" ? capture.reference : mode === "diff" ? capture.diff : capture.test;
  const size =
    mode === "side"
      ? {
          width: laneWidth,
          height: Math.max(...lanes.map((lane) => lane.size.height)),
        }
      : mode === "wipe" || mode === "overlay"
        ? {
            width: Math.max(referenceSize.width, testSize.width),
            height: Math.max(referenceSize.height, testSize.height),
          }
        : getSize(single) || fallback;
  const panes = mode === "side" ? lanes : [{ label: "Comparison", path: single, size, gap: null }];
  const paneCount = panes.length;
  useEffect(() => {
    const element = canvas.current;
    if (!element) return;
    const elements = Array.from(element.querySelectorAll<HTMLElement>(".image-pane"));
    if (elements.length !== paneCount) return;
    const observer = new ResizeObserver(() => {
      const nextWidth = Math.max(1, Math.min(...elements.map((pane) => pane.clientWidth)));
      const nextHeight = Math.max(1, Math.min(...elements.map((pane) => pane.clientHeight)));
      setViewport((current) =>
        current.width === nextWidth && current.height === nextHeight
          ? current
          : { width: nextWidth, height: nextHeight },
      );
      refreshCrosshairLayout();
    });
    observer.observe(element);
    for (const pane of elements) observer.observe(pane);
    return () => observer.disconnect();
  }, [paneCount]);
  const view = zoom?.capture === capture ? zoom.views[mode] : undefined;
  const fit = view?.fit ?? "width";
  const scale =
    fit === "manual" && view
      ? view.scale
      : imageFitScale(
          size,
          { width: viewport.width || size.width, height: viewport.height || size.height },
          fit,
        );
  const clearCrosshair = () => {
    crosshairPointer.current = null;
    setCrosshair(null);
  };
  const updateCrosshair = (index: number, point: { x: number; y: number }) => {
    const elements = Array.from(canvas.current?.querySelectorAll<HTMLElement>(".image-pane") ?? []);
    const source = elements[index];
    const sourceLane = lanes[index];
    const image = source?.querySelector("img");
    if (
      mode !== "side" ||
      capture.error !== null ||
      !source ||
      !sourceLane ||
      !isLoaded(sourceLane.path) ||
      !image?.naturalWidth ||
      !image.naturalHeight
    ) {
      clearCrosshair();
      return;
    }
    const viewport = source.getBoundingClientRect();
    if (
      point.x < viewport.left + source.clientLeft ||
      point.x >= viewport.left + source.clientLeft + source.clientWidth ||
      point.y < viewport.top + source.clientTop ||
      point.y >= viewport.top + source.clientTop + source.clientHeight
    ) {
      clearCrosshair();
      return;
    }
    const bounds = image.getBoundingClientRect();
    if (bounds.width <= 0 || bounds.height <= 0) return;
    crosshairPointer.current = { capture, index, ...point };
    const imageX = ((point.x - bounds.left) / bounds.width) * image.naturalWidth;
    const alignedY = imageRowToAlignedRow(
      ((point.y - bounds.top) / bounds.height) * image.naturalHeight,
      lanes[index]?.gap ?? null,
    );
    setCrosshair({
      capture,
      scale,
      positions: elements.map((pane, targetIndex) => {
        const target = pane.querySelector("img");
        const lane = lanes[targetIndex];
        if (!lane) return null;
        const row = alignedRowToImageRow(alignedY, lane.gap);
        if (!target?.naturalWidth || !target.naturalHeight || !isLoaded(lane.path) || row === null)
          return null;
        const image = target.getBoundingClientRect();
        const viewport = pane.getBoundingClientRect();
        return {
          left: pane.clientLeft,
          top: pane.clientTop,
          width: pane.clientWidth,
          height: pane.clientHeight,
          x:
            image.left -
            viewport.left -
            pane.clientLeft +
            (imageX / target.naturalWidth) * image.width,
          y:
            image.top - viewport.top - pane.clientTop + (row / target.naturalHeight) * image.height,
        };
      }),
    });
  };
  const refreshCrosshair = () => {
    const pointer = crosshairPointer.current;
    if (pointer?.capture === capture) updateCrosshair(pointer.index, pointer);
  };
  const refreshCrosshairLayout = useEffectEvent(refreshCrosshair);
  const scrollRoot = (target?: EventTarget | null) => {
    const pane = target instanceof Element ? target.closest<HTMLElement>(".image-pane") : null;
    if (pane && canvas.current?.contains(pane)) activePane.current = pane;
    return activePane.current?.isConnected
      ? activePane.current
      : canvas.current?.querySelector<HTMLElement>(".image-pane");
  };
  const syncScroll = (source: HTMLElement) => {
    if (mode !== "side") return;
    const left = source.scrollLeft;
    const top = source.scrollTop;
    const previous = scrollPositions.current.get(source);
    if (previous?.left === left && previous.top === top) return;
    activePane.current = source;
    scrollPositions.current.set(source, { left, top });
    for (const pane of canvas.current?.querySelectorAll<HTMLElement>(".image-pane") ?? []) {
      if (pane === source) continue;
      pane.scrollTo({ left, top, behavior: "instant" });
      // Record native clamping so mirrored events cannot scroll their source back.
      scrollPositions.current.set(pane, { left: pane.scrollLeft, top: pane.scrollTop });
    }
  };
  const cancelPan = () => {
    const pointer = pan.current?.pointer;
    pan.current = null;
    setPanning(false);
    if (pointer !== undefined && canvas.current?.hasPointerCapture(pointer)) {
      canvas.current.releasePointerCapture(pointer);
    }
  };
  const setView = (next: View) => {
    cancelPan();
    setCrosshair(null);
    setZoom((current) => ({
      capture,
      views: { ...(current?.capture === capture ? current.views : {}), [mode]: next },
    }));
  };
  const setScale = (value: number, point?: { x: number; y: number }) => {
    const next = Math.max(0.001, Math.min(8, value));
    if (!Number.isFinite(next) || (fit === "manual" && next === scale)) return;
    let padding: View["padding"];
    anchors.current = [];
    const selected = point && scrollRoot(document.elementFromPoint(point.x, point.y));
    if (point && selected) {
      const selectedBounds = selected.getBoundingClientRect();
      padding = Array.from(
        canvas.current?.querySelectorAll<HTMLElement>(".image-pane") ?? [],
        (scroll) => {
          const source = scroll.querySelector<HTMLElement>(".image-surface");
          const stage = scroll.querySelector<HTMLElement>(".comparison-stage");
          if (!source || !stage) return { left: 0, top: 0, right: 0, bottom: 0 };
          const bounds = scroll.getBoundingClientRect();
          const surface = stage.getBoundingClientRect();
          const image = source.getBoundingClientRect();
          const position = {
            x: bounds.left + point.x - selectedBounds.left,
            y: bounds.top + point.y - selectedBounds.top,
          };
          const imageX = (position.x - image.left) / scale;
          const imageY = (position.y - image.top) / scale;
          const originX = surface.left + scroll.scrollLeft;
          const originY = surface.top + scroll.scrollTop;
          const left = position.x - originX - imageX * next;
          const top = position.y - originY - imageY * next;
          anchors.current.push({ ...position, imageX, imageY, element: source, scroll });
          // Full inner bounds retain anchor space as scrollbars appear or disappear.
          return {
            left: Math.max(0, left),
            top: Math.max(0, top),
            right: Math.max(
              0,
              bounds.right - scroll.clientLeft - originX - left - size.width * next,
            ),
            bottom: Math.max(
              0,
              bounds.bottom - scroll.clientTop - originY - top - size.height * next,
            ),
          };
        },
      );
    }
    setView({
      fit: "manual",
      scale: next,
      horizontalPosition: point ? undefined : view?.horizontalPosition,
      verticalPosition: point ? undefined : view?.verticalPosition,
      padding,
    });
  };
  const setFit = (next: "width" | "area") => {
    anchors.current = [];
    setView({ fit: next, scale: 1 });
    for (const pane of canvas.current?.querySelectorAll<HTMLElement>(".image-pane") ?? []) {
      pane.scrollTo(0, 0);
      scrollPositions.current.set(pane, { left: pane.scrollLeft, top: pane.scrollTop });
    }
  };
  // biome-ignore lint/correctness/useExhaustiveDependencies: Layout inputs change the DOM bounds and native scroll clamping read here.
  useLayoutEffect(() => {
    const pending = view?.fit === "manual" ? anchors.current : [];
    anchors.current = [];
    for (const anchor of pending) {
      if (!anchor.element.isConnected) continue;
      const bounds = anchor.element.getBoundingClientRect();
      const scroll = anchor.scroll;
      scroll.scrollLeft += bounds.left + anchor.imageX * scale - anchor.x;
      scroll.scrollTop += bounds.top + anchor.imageY * scale - anchor.y;
    }
    // Zoom anchoring and layout clamping are not user scroll gestures.
    for (const pane of canvas.current?.querySelectorAll<HTMLElement>(".image-pane") ?? []) {
      scrollPositions.current.set(pane, { left: pane.scrollLeft, top: pane.scrollTop });
    }
    refreshCrosshairLayout();
  }, [
    scale,
    view?.fit,
    view?.horizontalPosition,
    view?.verticalPosition,
    viewport.width,
    viewport.height,
    size.width,
    size.height,
  ]);
  const wheelZoom = useEffectEvent((event: WheelEvent) => {
    if ((!event.ctrlKey && !event.metaKey) || !Number.isFinite(event.deltaY) || event.deltaY === 0)
      return;
    event.preventDefault();
    event.stopPropagation();
    setScale(nextZoom(scale, event.deltaY < 0), { x: event.clientX, y: event.clientY });
  });
  const clearGesture = useEffectEvent(() => {
    cancelPan();
    clearCrosshair();
    anchors.current = [];
    activePane.current = null;
  });
  useEffect(() => {
    const element = viewer.current?.closest<HTMLElement>(".review-scroll");
    if (!element) return;
    element.addEventListener("wheel", wheelZoom, { passive: false });
    return () => element.removeEventListener("wheel", wheelZoom);
  }, []);
  useEffect(() => {
    if (gestureScope.current.capture !== capture || gestureScope.current.mode !== mode) {
      clearGesture();
      gestureScope.current = { capture, mode };
    }
  }, [capture, mode]);
  useEffect(() => {
    const clear = () => clearGesture();
    window.addEventListener("blur", clear);
    return () => {
      window.removeEventListener("blur", clear);
      clear();
    };
  }, []);
  useEffect(() => {
    const clear = () => {
      crosshairPointer.current = null;
      setCrosshair(null);
    };
    setCrosshair((current) =>
      mode === "side" && current?.capture === capture && current.scale === scale ? current : null,
    );
    window.addEventListener("scroll", refreshCrosshairLayout, true);
    window.addEventListener("blur", clear);
    return () => {
      window.removeEventListener("scroll", refreshCrosshairLayout, true);
      window.removeEventListener("blur", clear);
    };
  }, [capture, mode, scale]);
  return (
    <section
      ref={viewer}
      className="comparison-viewer flex min-h-0 min-w-0 flex-1 flex-col overflow-hidden"
      aria-label="Image comparison"
    >
      <div
        ref={toolbar}
        className="comparison-modes sticky top-0 z-3 mb-2 flex shrink-0 flex-wrap items-center gap-x-3 gap-y-1.5 bg-bg py-[5px] print:hidden"
      >
        <fieldset
          className={`${segmented} flex-wrap max-[601px]:hidden`}
          aria-label="Comparison mode"
        >
          {modes.map(([value, label, Icon]) => (
            <button
              key={value}
              type="button"
              aria-pressed={mode === value}
              disabled={!available(value)}
              onClick={() => onMode(value)}
              className={`inline-flex items-center justify-center gap-1.5 rounded-[3px] [border:0] bg-transparent px-2.5 py-[7px] text-[13px] text-muted max-[1201px]:p-[7px] ${segmentedItem}`}
            >
              <Icon size={15} className="max-[1201px]:hidden" />
              <span>{label}</span>
            </button>
          ))}
        </fieldset>
        <select
          className="hidden rounded-[4px] border border-border bg-surface px-3 py-[9px] text-foreground max-[601px]:block max-[601px]:min-w-0 max-[601px]:flex-1"
          aria-label="Comparison mode"
          value={mode}
          onChange={(event) => onMode(event.target.value)}
        >
          {modes.map(([value, label]) => (
            <option key={value} value={value} disabled={!available(value)}>
              {label}
            </option>
          ))}
        </select>
        <div className="flex items-center gap-1">
          <IconButton
            label="Zoom out"
            disabled={scale <= 0.25}
            onClick={() => setScale(nextZoom(scale, false))}
          >
            <Minus size={16} />
          </IconButton>
          <output className="mono w-14 shrink-0 text-center text-[13px]" aria-label="Zoom level">
            {Math.round(scale * 1000) / 10}%
          </output>
          <IconButton
            label="Zoom in"
            disabled={scale >= 8}
            onClick={() => setScale(nextZoom(scale, true))}
          >
            <Plus size={16} />
          </IconButton>
          <IconButton
            label="Fit images"
            title="Fit width (W)"
            aria-pressed={fit === "width"}
            onClick={() => setFit("width")}
          >
            <FoldHorizontal size={17} />
          </IconButton>
          <IconButton
            label="Fit to window"
            title="Fit to window (0)"
            aria-pressed={fit === "area"}
            onClick={() => setFit("area")}
          >
            <Scan size={17} />
          </IconButton>
          <IconButton
            label="Actual size"
            title="Actual size (1)"
            aria-pressed={fit === "manual" && scale === 1}
            onClick={() => setScale(1)}
          >
            <Focus size={17} />
          </IconButton>
        </div>
        <div className="flex items-center gap-1">
          {(
            [
              ["reference", capture.reference],
              ["test", capture.test],
              ["diff", capture.diff],
            ] as const
          ).map(
            ([label, path]) =>
              path && (
                <a
                  key={label}
                  className={`${iconButton()} relative`}
                  href={`/${path}`}
                  download={`${capture.id}-${label}.png`}
                  title={`Download ${label}`}
                  aria-label={`Download ${label}`}
                >
                  <Download size={16} />
                  <span
                    className="absolute right-0 bottom-0 [font:8px_var(--font-mono)]"
                    aria-hidden="true"
                  >
                    {label[0]?.toUpperCase()}
                  </span>
                </a>
              ),
          )}
        </div>
        <CaptureInfo
          capture={capture}
          suiteId={suiteId}
          suiteName={suiteName}
          runId={runId}
          dimensions={lanes
            .filter((lane) => lane.path)
            .map((lane) => ({ label: lane.label, ...lane.size }))}
        />
        {(mode === "wipe" || mode === "overlay") && (
          <label className="flex items-center gap-2.5 py-1.5 text-[13px] text-muted max-[601px]:flex-1">
            <span>{mode === "wipe" ? "Wipe" : "Opacity"}</span>
            <input
              type="range"
              min="0"
              max="100"
              value={blend}
              aria-label={mode === "wipe" ? "Wipe position" : "Test image opacity"}
              className="w-[120px] accent-accent max-[601px]:min-w-[60px] max-[601px]:flex-1"
              onChange={(event) => setBlend(Number(event.target.value))}
            />
            <output className="mono w-[34px] text-[12px]">{blend}%</output>
          </label>
        )}
      </div>
      {/* `zoom-canvas`, `mode-*`, and `is-panning` are test hooks. */}
      <section
        ref={canvas}
        className={`zoom-canvas mode-${mode} grid min-h-0 min-w-0 flex-1 gap-3 overflow-hidden select-none ${panning ? "is-panning cursor-grabbing" : "cursor-grab"}`}
        aria-label="Zoomable comparison images"
        aria-keyshortcuts="+ - 0 1 W"
        // biome-ignore lint/a11y/noNoninteractiveTabindex: This image region owns scoped zoom and pan keys while retaining native scrolling.
        tabIndex={0}
        style={{ gridTemplateColumns: `repeat(${panes.length}, minmax(0, 1fr))` }}
        onFocusCapture={(event) => {
          scrollRoot(event.target);
        }}
        onPointerDown={(event) => {
          clearCrosshair();
          if (event.button !== 0 || event.pointerType === "touch") return;
          const scroll = scrollRoot(event.target);
          if (!scroll) return;
          const bounds = scroll.getBoundingClientRect();
          if (
            event.clientX >= bounds.left + scroll.clientLeft + scroll.clientWidth ||
            event.clientY >= bounds.top + scroll.clientTop + scroll.clientHeight
          )
            return;
          event.preventDefault();
          scroll.focus({ preventScroll: true });
          const image = scroll.querySelector(".image-surface")?.getBoundingClientRect();
          pan.current = {
            pointer: event.pointerId,
            x: event.clientX,
            y: event.clientY,
            left: scroll.scrollLeft,
            top: scroll.scrollTop,
            imageLeft: image ? image.left - bounds.left - scroll.clientLeft : 0,
            horizontalSpace:
              mode === "side" && image ? Math.max(0, viewport.width - image.width) : 0,
            imageTop: image ? image.top - bounds.top - scroll.clientTop : 0,
            verticalSpace:
              mode === "side" && image ? Math.max(0, viewport.height - image.height) : 0,
            moved: false,
            scroll,
          };
          event.currentTarget.setPointerCapture(event.pointerId);
        }}
        onPointerMove={(event) => {
          const drag = pan.current;
          if (drag && drag.pointer === event.pointerId) {
            if (!(event.buttons & 1)) {
              cancelPan();
              return;
            }
            const deltaX = event.clientX - drag.x;
            const deltaY = event.clientY - drag.y;
            if (!drag.moved && Math.max(Math.abs(deltaX), Math.abs(deltaY)) < 4) return;
            drag.moved = true;
            setPanning(true);
            setCrosshair(null);
            if (drag.horizontalSpace > 0 || drag.verticalSpace > 0) {
              setZoom((current) => {
                const views = current?.capture === capture ? current.views : {};
                const placement = { fit, scale, ...views[mode] };
                if (drag.horizontalSpace > 0) {
                  placement.horizontalPosition = Math.max(
                    0,
                    Math.min(1, (drag.imageLeft + deltaX) / drag.horizontalSpace),
                  );
                }
                if (drag.verticalSpace > 0) {
                  placement.verticalPosition = Math.max(
                    0,
                    Math.min(1, (drag.imageTop + deltaY) / drag.verticalSpace),
                  );
                }
                return {
                  capture,
                  views: {
                    ...views,
                    [mode]: placement,
                  },
                };
              });
            }
            drag.scroll.scrollTo({
              left: drag.horizontalSpace > 0 ? 0 : drag.left - deltaX,
              top: drag.verticalSpace > 0 ? 0 : drag.top - deltaY,
            });
            return;
          }
          if (mode !== "wipe" || event.pointerType === "touch" || event.buttons !== 0) return;
          const bounds = world.current?.getBoundingClientRect();
          if (bounds && bounds.width > 0) {
            setBlend(
              Math.round(
                Math.max(0, Math.min(100, ((event.clientX - bounds.left) / bounds.width) * 100)),
              ),
            );
          }
        }}
        onPointerUp={(event) => {
          cancelPan();
          if (event.pointerType === "touch") return;
          const pane = document
            .elementFromPoint(event.clientX, event.clientY)
            ?.closest(".image-pane");
          if (!pane) return;
          const elements = Array.from(canvas.current?.querySelectorAll(".image-pane") ?? []);
          const index = elements.indexOf(pane);
          if (index >= 0) updateCrosshair(index, { x: event.clientX, y: event.clientY });
        }}
        onPointerCancel={cancelPan}
        onLostPointerCapture={cancelPan}
        onContextMenu={cancelPan}
        onDoubleClick={(event) => {
          if (event.button !== 0) return;
          event.preventDefault();
          if (fit === "manual" && scale === 1) setFit("area");
          else setScale(1, { x: event.clientX, y: event.clientY });
        }}
        onKeyDown={(event) => {
          if (
            (event.target !== event.currentTarget &&
              !(event.target instanceof HTMLElement && event.target.matches(".image-pane"))) ||
            event.ctrlKey ||
            event.metaKey ||
            event.altKey
          )
            return;
          if (event.key === "+" || event.key === "=") setScale(nextZoom(scale, true));
          else if (event.key === "-") setScale(nextZoom(scale, false));
          else if (event.key === "0") setFit("area");
          else if (event.key === "1") setScale(1);
          else if (event.key.toLowerCase() === "w") setFit("width");
          else if (event.key === "Escape") cancelPan();
          else if (["ArrowLeft", "ArrowRight", "ArrowUp", "ArrowDown"].includes(event.key)) {
            const step = event.shiftKey ? 160 : 40;
            scrollRoot()?.scrollBy({
              left: event.key === "ArrowLeft" ? -step : event.key === "ArrowRight" ? step : 0,
              top: event.key === "ArrowUp" ? -step : event.key === "ArrowDown" ? step : 0,
            });
          } else return;
          event.preventDefault();
          event.stopPropagation();
        }}
      >
        {panes.map((lane, index) => {
          const horizontalPosition = mode === "side" ? view?.horizontalPosition : undefined;
          const verticalPosition = mode === "side" ? view?.verticalPosition : undefined;
          const horizontalSpace = Math.max(0, viewport.width - size.width * scale);
          const verticalSpace = Math.max(0, viewport.height - size.height * scale);
          const anchorPadding = view?.padding?.[index];
          const padding =
            anchorPadding || horizontalPosition !== undefined || verticalPosition !== undefined
              ? {
                  left:
                    horizontalPosition !== undefined
                      ? horizontalSpace * horizontalPosition
                      : (anchorPadding?.left ?? horizontalSpace / 2),
                  right:
                    horizontalPosition !== undefined
                      ? horizontalSpace * (1 - horizontalPosition)
                      : (anchorPadding?.right ?? horizontalSpace / 2),
                  top:
                    verticalPosition !== undefined
                      ? verticalSpace * verticalPosition
                      : (anchorPadding?.top ?? 0),
                  bottom:
                    verticalPosition !== undefined
                      ? verticalSpace * (1 - verticalPosition)
                      : (anchorPadding?.bottom ?? 0),
                }
              : undefined;
          const guide =
            crosshair?.capture === capture && crosshair.scale === scale
              ? crosshair.positions[index]
              : null;
          return (
            // The viewport positions the crosshair outside the native scroller so the
            // guides cannot extend its scroll range.
            <div key={lane.label} className="relative min-h-0 min-w-0 overflow-hidden">
              <section
                className={`image-pane group/lane size-full min-h-0 min-w-0 overflow-auto overscroll-contain border border-border [scrollbar-gutter:stable] ${mode === "side" ? "comparison-lane has-[.comparison-crosshair]:cursor-crosshair" : ""}`}
                aria-label={`${lane.label} image viewport`}
                // biome-ignore lint/a11y/noNoninteractiveTabindex: Native scroll panes support scoped keyboard zoom and pan.
                tabIndex={0}
                onScroll={(event) => {
                  syncScroll(event.currentTarget);
                  refreshCrosshair();
                }}
                onPointerMove={(event) => {
                  if (event.pointerType === "touch" || event.buttons !== 0) clearCrosshair();
                  else updateCrosshair(index, { x: event.clientX, y: event.clientY });
                }}
                onPointerLeave={clearCrosshair}
                onPointerCancel={clearCrosshair}
              >
                <div
                  className="comparison-stage [overflow-anchor:none]"
                  style={{
                    width: padding ? size.width * scale + padding.left + padding.right : undefined,
                    paddingLeft: padding?.left,
                    paddingTop: padding?.top,
                    paddingRight: padding?.right,
                    paddingBottom: padding?.bottom,
                  }}
                >
                  <div
                    className={`image-surface relative mx-auto overflow-clip bg-canvas [background-image:linear-gradient(45deg,#7d938610_25%,transparent_25%),linear-gradient(-45deg,#7d938610_25%,transparent_25%),linear-gradient(45deg,transparent_75%,#7d938610_75%),linear-gradient(-45deg,transparent_75%,#7d938610_75%)] [background-position:0_0,0_10px,10px_-10px,-10px_0] [background-size:20px_20px] select-none ${panning ? "cursor-grabbing" : "cursor-grab"} ${mode === "side" ? "group-has-[.comparison-crosshair]/lane:cursor-crosshair" : ""}`}
                    style={{
                      width: size.width * scale,
                      height: size.height * scale,
                    }}
                  >
                    <div
                      ref={index === 0 ? world : undefined}
                      className="image-world absolute top-0 left-0 shrink-0 [&>div]:shrink-0"
                      style={{
                        width: size.width,
                        height: size.height,
                        transform: `scale(${scale})`,
                        transformOrigin: "top left",
                      }}
                    >
                      {mode === "side" ? (
                        <div className="comparison-image relative" style={lane.size}>
                          <ReportImage
                            path={lane.path}
                            alt={`${lane.label}: ${capture.label}`}
                            width={lane.size.width}
                            height={lane.size.height}
                            eager
                            className="pointer-events-none size-full max-w-none object-contain object-[top_left] select-none"
                          />
                        </div>
                      ) : mode === "wipe" || mode === "overlay" ? (
                        <>
                          <div className="absolute top-0 left-0" style={referenceSize}>
                            <ReportImage
                              path={capture.reference}
                              alt={`Reference: ${capture.label}`}
                              width={referenceSize.width}
                              height={referenceSize.height}
                              eager
                              className="pointer-events-none size-full max-w-none object-contain object-[top_left] select-none"
                            />
                          </div>
                          <div
                            className="absolute top-0 left-0"
                            style={{
                              ...size,
                              opacity: mode === "overlay" ? blend / 100 : 1,
                              clipPath: mode === "wipe" ? `inset(0 0 0 ${blend}%)` : undefined,
                            }}
                          >
                            <div style={testSize}>
                              <ReportImage
                                path={capture.test}
                                alt={`Test: ${capture.label}`}
                                width={testSize.width}
                                height={testSize.height}
                                eager
                                className="pointer-events-none size-full max-w-none object-contain object-[top_left] select-none"
                              />
                            </div>
                          </div>
                          {mode === "wipe" && (
                            <div
                              className="absolute top-0 bottom-0 w-[3px] bg-[#008f74] [box-shadow:0_0_0_1px_#ffffffa0]"
                              style={{ left: `${blend}%` }}
                            />
                          )}
                        </>
                      ) : (
                        <ReportImage
                          path={single}
                          alt={`${mode}: ${capture.label}`}
                          width={size.width}
                          height={size.height}
                          eager
                          className="pointer-events-none size-full max-w-none object-contain object-[top_left] select-none"
                        />
                      )}
                    </div>
                  </div>
                </div>
                {guide && (
                  <span
                    className="comparison-crosshair pointer-events-none absolute top-0 left-0 z-1 overflow-clip [transform-origin:top_left]"
                    aria-hidden="true"
                    style={{
                      left: guide.left,
                      top: guide.top,
                      width: guide.width,
                      height: guide.height,
                    }}
                  >
                    <span
                      className={`crosshair-horizontal left-0 h-px w-full [transform:translateY(-50%)] ${guideLine}`}
                      style={{ top: guide.y }}
                    />
                    <span
                      className={`crosshair-vertical top-0 h-full w-px [transform:translateX(-50%)] ${guideLine}`}
                      style={{ left: guide.x }}
                    />
                  </span>
                )}
              </section>
            </div>
          );
        })}
      </section>
    </section>
  );
}

import {
  ChevronRight,
  Monitor,
  Moon,
  PanelLeftClose,
  PanelLeftOpen,
  Settings,
  Sun,
  X,
} from "lucide-react";
import { type CSSProperties, useEffect, useLayoutEffect, useRef, useState } from "react";
import Markdown from "react-markdown";
import { BrowserRouter, Link, Navigate, Route, Routes, useLocation, useMatch } from "react-router";
import desktopIcon from "../../assets/icon.svg?url";
import { ReviewNavigation } from "./components/ReviewNavigation";
import { IconButton, StatePanel, segmented } from "./components/ui";
import { safePageUrl, suiteLabel, suitePath } from "./data/report";
import { ReportProvider, useManifest } from "./data/store";
import ImagePage from "./pages/ImagePage";
import { OverviewPage } from "./pages/OverviewPage";
import { SuitePage } from "./pages/SuitePage";

const iconUrl = `${desktopIcon}?v=${import.meta.env.VITE_REGRESSIFY_ICON_VERSION}`;

function readTheme(): "light" | "dark" | "system" {
  try {
    const saved = localStorage.getItem("regressify-theme");
    return saved === "light" || saved === "dark" ? saved : "system";
  } catch {
    return "system";
  }
}

export function ThemeControl() {
  const [theme, setTheme] = useState(readTheme);
  useEffect(() => {
    let storage: Storage;
    try {
      storage = localStorage;
    } catch {
      return;
    }
    const sync = (event: StorageEvent) => {
      if (
        event.storageArea === storage &&
        (event.key === "regressify-theme" || event.key === null)
      ) {
        setTheme(readTheme());
      }
    };
    window.addEventListener("storage", sync);
    return () => window.removeEventListener("storage", sync);
  }, []);
  useEffect(() => {
    const media = matchMedia("(prefers-color-scheme: dark)");
    const apply = () => {
      document.documentElement.dataset.theme =
        theme === "system" ? (media.matches ? "dark" : "light") : theme;
    };
    apply();
    if (theme !== "system") return;
    media.addEventListener("change", apply);
    return () => media.removeEventListener("change", apply);
  }, [theme]);
  return (
    <fieldset className={segmented} aria-label="Color theme">
      {(
        [
          ["light", Sun],
          ["dark", Moon],
          ["system", Monitor],
        ] as const
      ).map(([value, Icon]) => (
        <IconButton
          key={value}
          label={`${value[0]?.toUpperCase()}${value.slice(1)} theme`}
          compact
          aria-pressed={theme === value}
          onClick={() => {
            setTheme(value);
            try {
              localStorage.setItem("regressify-theme", value);
            } catch {
              /* Theme changes remain usable when storage is unavailable. */
            }
          }}
        >
          <Icon size={16} />
        </IconButton>
      ))}
    </fieldset>
  );
}

// Later sections and rows are separated from the previous one.
const settingsSection =
  "not-first:mt-5 not-first:border-t not-first:border-t-border not-first:pt-4";
const settingsRow =
  "flex flex-wrap items-center justify-between gap-x-4 gap-y-2 not-first-of-type:mt-3";
// Shared by the side panel and the mobile drawer; `suite-panel-scroll` is a test hook.
const panelScroll =
  "suite-panel-scroll min-h-0 flex-1 overflow-y-auto overscroll-contain px-3.5 py-[18px] [scrollbar-gutter:stable]";

function SettingsControl() {
  const dialog = useRef<HTMLDialogElement>(null);
  const [open, setOpen] = useState(false);
  return (
    <>
      <IconButton
        label="Settings"
        aria-haspopup="dialog"
        aria-expanded={open}
        aria-controls="snapshot-settings"
        onClick={() => {
          dialog.current?.showModal();
          setOpen(true);
        }}
      >
        <Settings size={18} />
      </IconButton>
      <dialog
        ref={dialog}
        id="snapshot-settings"
        className="m-auto max-h-[calc(100dvh-32px)] w-[min(360px,calc(100vw-32px))] rounded-[6px] border border-border bg-surface p-0 text-foreground [box-shadow:0_8px_32px_#00000026] backdrop:bg-[#00000038] open:flex open:flex-col"
        aria-labelledby="settings-title"
        onClose={() => setOpen(false)}
        onKeyDown={(event) => {
          if (event.key === "Escape" && !event.defaultPrevented) {
            event.preventDefault();
            event.currentTarget.close();
          }
        }}
        onClick={(event) => {
          if (event.target !== event.currentTarget) return;
          const bounds = event.currentTarget.getBoundingClientRect();
          if (
            event.clientX < bounds.left ||
            event.clientX > bounds.right ||
            event.clientY < bounds.top ||
            event.clientY > bounds.bottom
          ) {
            event.currentTarget.close();
          }
        }}
      >
        <div className="flex shrink-0 items-center justify-between gap-4 border-b border-b-border px-4 py-2">
          <h2 id="settings-title" className="text-[16px] font-semibold">
            Settings
          </h2>
          <IconButton label="Close settings" onClick={() => dialog.current?.close()}>
            <X size={18} />
          </IconButton>
        </div>
        <div className="settings-content min-h-0 overflow-y-auto overscroll-contain p-4">
          <section className={settingsSection} aria-labelledby="appearance-settings">
            <h3
              id="appearance-settings"
              className="mb-2 text-[11px] font-semibold text-muted uppercase"
            >
              Appearance
            </h3>
            <div className={settingsRow}>
              <span>Theme</span>
              <ThemeControl />
            </div>
          </section>
        </div>
      </dialog>
    </>
  );
}

function Shell() {
  const manifest = useManifest();
  const location = useLocation();
  const isOverview = location.pathname === "/" || location.pathname === "/index.html";
  const match = useMatch("/suites/:suiteId/*");
  const imageMatch = useMatch("/suites/:suiteId/images/:imageId");
  const suiteId = match?.params.suiteId;
  const activeSuite = manifest.suites.find((suite) => suite.id === suiteId);
  const suiteName = activeSuite ? suiteLabel(activeSuite) : suiteId;
  const [panelOpen, setPanelOpen] = useState(true);
  const [viewportWidth, setViewportWidth] = useState(window.innerWidth);
  const compact = viewportWidth <= 959;
  const [drawerOpen, setDrawerOpen] = useState(false);
  const [preferredPanelWidth, setPanelWidth] = useState(() => {
    try {
      const saved = Number(localStorage.getItem("regressify-panel-width"));
      return Number.isFinite(saved) && saved >= 220 ? Math.min(480, saved) : 280;
    } catch {
      return 280;
    }
  });
  const drag = useRef<{ x: number; width: number } | null>(null);
  const [resizing, setResizing] = useState(false);
  const dialog = useRef<HTMLDialogElement>(null);
  const header = useRef<HTMLElement>(null);
  const main = useRef<HTMLElement>(null);
  const maximumWidth = Math.max(220, Math.min(480, Math.floor(viewportWidth * 0.45)));
  const panelWidth = Math.min(preferredPanelWidth, maximumWidth);
  const resizePanel = (width: number) =>
    setPanelWidth(Math.max(220, Math.min(maximumWidth, width)));
  useLayoutEffect(() => {
    const element = header.current;
    if (!element) return;
    const update = () =>
      element.parentElement?.style.setProperty(
        "--header-height",
        `${element.getBoundingClientRect().height}px`,
      );
    update();
    const observer = new ResizeObserver(update);
    observer.observe(element);
    return () => observer.disconnect();
  }, []);
  useEffect(() => {
    const update = () => {
      setViewportWidth(window.innerWidth);
      if (window.innerWidth > 959) {
        dialog.current?.close();
        setDrawerOpen(false);
      }
    };
    window.addEventListener("resize", update);
    return () => window.removeEventListener("resize", update);
  }, []);
  useEffect(() => {
    try {
      localStorage.setItem("regressify-panel-width", String(preferredPanelWidth));
    } catch {
      /* Navigation width remains usable without storage. */
    }
  }, [preferredPanelWidth]);
  useEffect(() => {
    document.title = suiteName
      ? [suiteName, manifest.title].filter(Boolean).join(" | ")
      : manifest.title;
  }, [manifest.title, suiteName]);
  useEffect(() => {
    if (location.pathname) main.current?.focus({ preventScroll: true });
    window.scrollTo(0, 0);
  }, [location.pathname]);
  const togglePanel = () => {
    if (compact) {
      setDrawerOpen(true);
      dialog.current?.showModal();
    } else setPanelOpen(!panelOpen);
  };
  // Semantic class names left on elements here (`report-identity`,
  // `report-title`, `settings-content`, `suite-panel`, `suite-panel-scroll`,
  // `breadcrumb-bar`, `snapshot-date`, `review-scroll`) carry no styles: tests
  // and the comparison viewer select by them. While a comparison viewer is
  // shown, it scrolls internally and the review column hides its overflow.
  return (
    <div className={`flex min-h-dvh flex-col ${match ? "has-[.comparison-viewer]:h-dvh" : ""}`}>
      <a href="#main" className="skip-link">
        Skip to results
      </a>
      <header
        ref={header}
        className="app-header grid h-auto min-h-[60px] shrink-0 grid-cols-[auto_minmax(0,1fr)_auto] px-6 py-2 max-[601px]:gap-3 max-[601px]:px-4"
      >
        <Link to="/" className="brand" aria-label="Regressify overview">
          <img src={iconUrl} width={24} height={24} alt="" />
          <span className="max-[601px]:hidden">Regressify</span>
        </Link>
        <div className="report-identity min-w-0 border-l border-l-border pl-4 [overflow-wrap:anywhere] empty:[border:0] empty:p-0 max-[601px]:pl-0 max-[601px]:[border-left:0]">
          {manifest.heading && (
            <p className="report-title text-[16px] leading-[1.25] font-semibold max-[601px]:text-[14px]">
              {manifest.heading}
            </p>
          )}
          {manifest.subheading && (
            <div className="report-subheading mt-0.5 max-w-none text-[12px] leading-[1.4]">
              <Markdown
                skipHtml
                allowedElements={["p", "strong", "em", "a", "code", "br", "ul", "ol", "li"]}
                components={{
                  a: ({ children, href }) => (
                    <a href={safePageUrl(href || null)} target="_blank" rel="noopener noreferrer">
                      {children}
                    </a>
                  ),
                }}
              >
                {manifest.subheading}
              </Markdown>
            </div>
          )}
        </div>
        <div className="header-actions">
          <SettingsControl />
        </div>
      </header>
      <div
        className={`grid min-h-0 flex-1 ${match && panelOpen && !compact ? "grid-cols-[var(--panel-width,280px)_minmax(0,1fr)] max-[960px]:grid-cols-[minmax(0,1fr)] print:block" : "grid-cols-[minmax(0,1fr)]"} ${resizing ? "cursor-col-resize select-none" : ""}`}
        style={{ "--panel-width": `${panelWidth}px` } as CSSProperties}
      >
        {match && panelOpen && !compact && (
          <aside
            className="suite-panel sticky [align-self:start] top-[var(--header-height,60px)] flex h-[calc(100dvh-var(--header-height,60px))] min-h-0 min-w-0 flex-col border-r border-r-border bg-surface max-[960px]:hidden print:hidden"
            id="suite-panel"
          >
            <div className={panelScroll}>
              <ReviewNavigation close={() => {}} />
            </div>
            <hr
              className={`absolute top-0 right-[-3px] bottom-0 z-5 m-0 h-full w-[7px] cursor-col-resize touch-none [border:0] hover:bg-accent focus-visible:bg-accent ${resizing ? "bg-accent" : ""}`}
              tabIndex={0}
              aria-label="Resize review navigation"
              aria-orientation="vertical"
              aria-valuemin={220}
              aria-valuemax={maximumWidth}
              aria-valuenow={panelWidth}
              aria-controls="suite-panel"
              onPointerDown={(event) => {
                if (event.button !== 0) return;
                event.preventDefault();
                event.currentTarget.focus();
                event.currentTarget.setPointerCapture(event.pointerId);
                drag.current = { x: event.clientX, width: panelWidth };
                setResizing(true);
              }}
              onPointerMove={(event) => {
                if (drag.current) resizePanel(drag.current.width + event.clientX - drag.current.x);
              }}
              onPointerUp={(event) => {
                drag.current = null;
                setResizing(false);
                if (event.currentTarget.hasPointerCapture(event.pointerId))
                  event.currentTarget.releasePointerCapture(event.pointerId);
              }}
              onLostPointerCapture={() => {
                drag.current = null;
                setResizing(false);
              }}
              onKeyDown={(event) => {
                const next =
                  event.key === "Home"
                    ? 220
                    : event.key === "End"
                      ? maximumWidth
                      : event.key === "ArrowLeft"
                        ? panelWidth - 16
                        : event.key === "ArrowRight"
                          ? panelWidth + 16
                          : null;
                if (next !== null) {
                  event.preventDefault();
                  resizePanel(next);
                }
              }}
            />
          </aside>
        )}
        <div className={`flex min-w-0 flex-col ${match ? "min-h-0" : ""}`}>
          {!isOverview && (
            <section
              className={`breadcrumb-bar flex min-h-[40px] items-center gap-3 border-b border-b-border px-6 py-[3px] max-[960px]:px-4 max-[601px]:px-3 print:hidden ${match ? "shrink-0" : ""}`}
              aria-label="Snapshot navigation"
            >
              {match && (
                <IconButton
                  label="Toggle suite index"
                  aria-expanded={compact ? drawerOpen : panelOpen}
                  aria-controls={
                    compact ? "review-navigation-drawer" : panelOpen ? "suite-panel" : undefined
                  }
                  onClick={togglePanel}
                >
                  {panelOpen ? <PanelLeftClose size={18} /> : <PanelLeftOpen size={18} />}
                </IconButton>
              )}
              <nav
                aria-label="Breadcrumb"
                className="flex min-w-0 items-center gap-2 text-[12px] text-muted max-[601px]:gap-1.5 max-[601px]:text-[11px]"
              >
                <Link to="/" className="hover:text-accent">
                  Overview
                </Link>
                {match && (
                  <>
                    <ChevronRight size={14} />
                    <Link to={suitePath(match.params.suiteId || "")} className="hover:text-accent">
                      {suiteName}
                    </Link>
                    {imageMatch && (
                      <>
                        <ChevronRight size={14} />
                        <span>Comparison</span>
                      </>
                    )}
                  </>
                )}
              </nav>
              <span className="snapshot-date mono ml-auto text-[11px] whitespace-nowrap text-muted max-[1151px]:text-[10px] max-[960px]:hidden">
                {new Date(manifest.generatedAt).toLocaleString(undefined, {
                  dateStyle: "medium",
                  timeStyle: "short",
                })}
              </span>
            </section>
          )}
          <div
            className={`group/content flex min-h-0 flex-1 flex-col ${match ? "review-scroll has-[.comparison-viewer]:overflow-hidden" : ""}`}
          >
            <main
              id="main"
              ref={main}
              tabIndex={-1}
              className={`flex-1 outline-none ${match ? "group-has-[.comparison-viewer]/content:flex group-has-[.comparison-viewer]/content:min-h-0 group-has-[.comparison-viewer]/content:flex-col group-has-[.comparison-viewer]/content:overflow-hidden" : ""}`}
            >
              <Routes>
                <Route path="/" element={<OverviewPage />} />
                <Route path="/index.html" element={<Navigate to="/" replace />} />
                <Route path="/suites/:suiteId" element={<SuitePage />} />
                <Route path="/suites/:suiteId/images/:imageId" element={<ImagePage />} />
                <Route
                  path="*"
                  element={
                    <StatePanel
                      title="Page not found"
                      detail="This URL is not part of this snapshot."
                      back
                    />
                  }
                />
              </Routes>
            </main>
            {!imageMatch && (
              <footer className="app-footer">
                <span>
                  <a href="https://regressify.eshn.dev/">Regressify</a>
                  {manifest.regressifyVersion && ` ${manifest.regressifyVersion}`}
                </span>
                <span>Visual regression report</span>
              </footer>
            )}
          </div>
        </div>
      </div>
      <dialog
        ref={dialog}
        id="review-navigation-drawer"
        aria-label="Suite index"
        className="m-0 h-dvh max-h-dvh w-[min(85vw,340px)] [border:0] bg-surface p-0 text-foreground backdrop:bg-[#10191880]"
        onClose={() => setDrawerOpen(false)}
        onClick={(event) => {
          if (event.target === dialog.current) dialog.current?.close();
        }}
        onKeyDown={(event) => {
          if (event.key === "Escape") dialog.current?.close();
        }}
      >
        <div className="flex h-full flex-col pt-2">
          <div className="flex shrink-0 justify-end px-3.5">
            <IconButton label="Close suite index" onClick={() => dialog.current?.close()}>
              <X size={20} />
            </IconButton>
          </div>
          {drawerOpen && (
            <div className={panelScroll}>
              <ReviewNavigation close={() => dialog.current?.close()} />
            </div>
          )}
        </div>
      </dialog>
    </div>
  );
}

export default function App() {
  return (
    <BrowserRouter>
      <link rel="icon" type="image/svg+xml" sizes="any" href={iconUrl} />
      <ReportProvider>
        <Shell />
      </ReportProvider>
    </BrowserRouter>
  );
}

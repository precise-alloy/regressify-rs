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
import { IconButton, StatePanel } from "./components/ui";
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
    <fieldset className="segmented theme-control" aria-label="Color theme">
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
        className="settings-dialog"
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
        <div className="settings-heading">
          <h2 id="settings-title">Settings</h2>
          <IconButton label="Close settings" onClick={() => dialog.current?.close()}>
            <X size={18} />
          </IconButton>
        </div>
        <div className="settings-content">
          <section className="settings-section" aria-labelledby="appearance-settings">
            <h3 id="appearance-settings">Appearance</h3>
            <div className="settings-row">
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
  return (
    <div className={`snapshot-app${match ? " snapshot-review" : ""}`}>
      <a href="#main" className="skip-link">
        Skip to results
      </a>
      <header ref={header} className="app-header snapshot-header">
        <Link to="/" className="brand" aria-label="Regressify overview">
          <img src={iconUrl} width={24} height={24} alt="" />
          <span>Regressify</span>
        </Link>
        <div className="report-identity">
          {manifest.heading && <p className="report-title">{manifest.heading}</p>}
          {manifest.subheading && (
            <div className="report-subheading">
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
        className={`app-layout ${match ? "review-layout" : ""} ${match && panelOpen && !compact ? "with-panel" : ""} ${resizing ? "resizing" : ""}`}
        style={{ "--panel-width": `${panelWidth}px` } as CSSProperties}
      >
        {match && panelOpen && !compact && (
          <aside className="suite-panel" id="suite-panel">
            <div className="suite-panel-scroll">
              <ReviewNavigation close={() => {}} />
            </div>
            <hr
              className="panel-resizer"
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
        <div className="workspace">
          {!isOverview && (
            <section className="breadcrumb-bar" aria-label="Snapshot navigation">
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
              <nav aria-label="Breadcrumb">
                <Link to="/">Overview</Link>
                {match && (
                  <>
                    <ChevronRight size={14} />
                    <Link to={suitePath(match.params.suiteId || "")}>{suiteName}</Link>
                    {imageMatch && (
                      <>
                        <ChevronRight size={14} />
                        <span>Comparison</span>
                      </>
                    )}
                  </>
                )}
              </nav>
              <span className="snapshot-date mono">
                {new Date(manifest.generatedAt).toLocaleString(undefined, {
                  dateStyle: "medium",
                  timeStyle: "short",
                })}
              </span>
            </section>
          )}
          <div className={`workspace-content${match ? " review-scroll" : ""}`}>
            <main id="main" ref={main} tabIndex={-1}>
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
        className="mobile-navigation"
        onClose={() => setDrawerOpen(false)}
        onClick={(event) => {
          if (event.target === dialog.current) dialog.current?.close();
        }}
        onKeyDown={(event) => {
          if (event.key === "Escape") dialog.current?.close();
        }}
      >
        <div className="mobile-nav-content">
          <div className="mobile-nav-close">
            <IconButton label="Close suite index" onClick={() => dialog.current?.close()}>
              <X size={20} />
            </IconButton>
          </div>
          {drawerOpen && (
            <div className="suite-panel-scroll">
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

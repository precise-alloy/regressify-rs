import {
  autoUpdate,
  FloatingFocusManager,
  FloatingPortal,
  flip,
  offset,
  shift,
  size,
  useClick,
  useDismiss,
  useFloating,
  useInteractions,
  useListNavigation,
  useRole,
} from "@floating-ui/react";
import { Check, ChevronDown } from "lucide-react";
import { useId, useRef, useState } from "react";
import { Link, useMatch } from "react-router";
import { type Capture, imagePath, suiteLabel, suitePath } from "../data/report";
import { useManifest, useSuite } from "../data/store";
import { button, statusDot } from "./ui";

// Shared by the suite and scenario sections; the dropdown variants keep the
// heading and search fixed while the list scrolls.
const section = "not-first:mt-5 not-first:border-t not-first:border-t-border not-first:pt-5";
const heading = (dropdown = false) =>
  `flex items-center justify-between px-2.5 text-[12px] font-[550] ${dropdown ? "mt-1 mb-3 shrink-0" : "mb-4"}`;
const search = (dropdown = false) =>
  `w-full rounded-[4px] border border-border bg-bg px-2.5 py-2 text-[14px] text-foreground ${dropdown ? "mb-2 shrink-0" : "mb-4"}`;
const linkItem =
  "group/link flex items-center gap-2.5 rounded-[4px] p-2.5 text-[14px] text-muted hover:bg-subtle hover:text-foreground aria-[current=page]:bg-accent-soft aria-[current=page]:text-foreground";
const empty = "p-2.5 text-[12px]";

function ScenarioNavigation({ suiteId, close }: { suiteId: string; close: () => void }) {
  const { data, error, retry } = useSuite(suiteId);
  const selected = useMatch("/suites/:suiteId/images/:imageId");
  const [query, setQuery] = useState("");
  const groups = new Map<string, Capture[]>();
  for (const capture of data?.tests || []) {
    const name = capture.scenario || capture.label || capture.id;
    const group = groups.get(name) || [];
    group.push(capture);
    groups.set(name, group);
  }
  const needle = query.trim().toLowerCase();
  const visible = [...groups].filter(
    ([name, captures]) =>
      !needle ||
      `${name} ${captures.map((capture) => capture.label).join(" ")}`
        .toLowerCase()
        .includes(needle),
  );
  const captureLink = (capture: Capture, label: string, grouped = false) => (
    <Link
      key={capture.id}
      to={imagePath(suiteId, capture.id)}
      onClick={close}
      className={grouped ? `${linkItem} ml-4` : linkItem}
      aria-current={selected?.params.imageId === capture.id ? "page" : undefined}
    >
      <span className={statusDot(capture.status === "fail")} />
      <span className="min-w-0 flex-1">{label}</span>
    </Link>
  );
  return (
    <section className={section}>
      <div className={heading()}>
        <h2 className="[font:inherit]">Test scenarios</h2>
        <span className="mono muted">{groups.size}</span>
      </div>
      <input
        className={search()}
        aria-label="Find a scenario"
        placeholder="Find a scenario..."
        type="search"
        value={query}
        onChange={(event) => setQuery(event.target.value)}
      />
      <nav aria-label="Test scenarios" className="flex flex-col gap-1">
        {error ? (
          <div className={empty}>
            <p role="alert">{error}</p>
            <button type="button" className={button} onClick={retry}>
              Retry
            </button>
          </div>
        ) : !data ? (
          <p className={`muted ${empty}`} role="status">
            Loading scenarios...
          </p>
        ) : visible.length ? (
          visible.map(([name, captures]) => {
            const first = captures[0];
            if (!first) return null;
            return captures.length === 1 ? (
              captureLink(first, name)
            ) : (
              <details className="scenario-group" key={name} open>
                <summary className="flex cursor-pointer items-center gap-2.5 p-2.5 text-[13px]">
                  <span
                    className={statusDot(captures.some((capture) => capture.status === "fail"))}
                  />
                  <span className="min-w-0 flex-1 [overflow-wrap:anywhere]">{name}</span>
                  <span className="mono muted text-[12px]">{captures.length}</span>
                </summary>
                {captures.map((capture) => captureLink(capture, capture.label, true))}
              </details>
            );
          })
        ) : (
          <p className={`muted ${empty}`}>
            {groups.size ? "No matching scenarios" : "No scenarios"}
          </p>
        )}
      </nav>
    </section>
  );
}

export function ReviewNavigation({ close }: { close: () => void }) {
  const manifest = useManifest();
  const match = useMatch("/suites/:suiteId/*");
  const [query, setQuery] = useState("");
  const [open, setOpen] = useState(false);
  const [activeIndex, setActiveIndex] = useState<number | null>(null);
  const searchRef = useRef<HTMLInputElement>(null);
  const listRef = useRef<Array<HTMLAnchorElement | null>>([]);
  const titleId = useId();
  const currentSuite = manifest.suites.find((suite) => suite.id === match?.params.suiteId);
  const currentLabel = currentSuite
    ? suiteLabel(currentSuite)
    : match?.params.suiteId || "Select a suite";
  const needle = query.trim().toLowerCase();
  const suites = manifest.suites.filter(
    (suite) => !needle || `${suite.id} ${suiteLabel(suite)}`.toLowerCase().includes(needle),
  );
  const changeOpen = (next: boolean) => {
    setOpen(next);
    if (!next) {
      setQuery("");
      setActiveIndex(null);
    }
  };
  const { refs, floatingStyles, context } = useFloating({
    open,
    onOpenChange(next, event, reason) {
      if (reason === "escape-key") event?.preventDefault();
      changeOpen(next);
    },
    placement: "bottom-start",
    strategy: "fixed",
    middleware: [
      offset(6),
      flip({ padding: 12 }),
      shift({ padding: 12 }),
      size({
        padding: 12,
        apply({ rects, availableHeight, availableWidth, elements }) {
          Object.assign(elements.floating.style, {
            width: `${Math.min(Math.max(rects.reference.width, 280), availableWidth)}px`,
            maxHeight: `${Math.min(400, availableHeight)}px`,
          });
        },
      }),
    ],
    whileElementsMounted: autoUpdate,
  });
  const click = useClick(context);
  const dismiss = useDismiss(context, { capture: { escapeKey: true } });
  const role = useRole(context, { role: "dialog" });
  const navigation = useListNavigation(context, {
    listRef,
    activeIndex,
    onNavigate: setActiveIndex,
    loop: true,
    focusItemOnOpen: false,
  });
  const { getReferenceProps, getFloatingProps, getItemProps } = useInteractions([
    click,
    dismiss,
    role,
    navigation,
  ]);
  return (
    <>
      <section className={section}>
        <div className={heading()}>
          <h2 className="[font:inherit]">Test suite</h2>
          <span className="mono muted">{manifest.suites.length}</span>
        </div>
        <button
          type="button"
          className="suite-selector group/selector flex w-full items-center gap-2.5 rounded-[4px] border border-border bg-bg p-2.5 text-left text-[14px] font-semibold text-foreground hover:border-accent hover:bg-subtle aria-expanded:border-accent aria-expanded:bg-subtle"
          ref={refs.setReference}
          aria-label={`Test suite: ${currentLabel}`}
          {...getReferenceProps()}
        >
          {currentSuite && <span className={statusDot(currentSuite.failed > 0)} />}
          <span className="min-w-0 flex-1 [overflow-wrap:anywhere]">{currentLabel}</span>
          <ChevronDown
            size={16}
            aria-hidden="true"
            className="shrink-0 group-aria-expanded/selector:[transform:rotate(180deg)]"
          />
        </button>
        {open && (
          <FloatingPortal root={refs.domReference.current?.closest("dialog") || undefined}>
            <FloatingFocusManager context={context} modal={false} initialFocus={searchRef}>
              <dialog
                open
                ref={refs.setFloating}
                style={floatingStyles}
                className="suite-dropdown z-60 m-0 flex flex-col overflow-hidden rounded-[6px] border border-border bg-surface p-2 text-foreground [box-shadow:0_10px_28px_#00000029]"
                {...getFloatingProps()}
                aria-labelledby={titleId}
                aria-orientation={undefined}
              >
                <div className={heading(true)}>
                  <h2 id={titleId} className="[font:inherit]">
                    Test suites
                  </h2>
                  <span className="mono muted">{manifest.suites.length}</span>
                </div>
                <input
                  ref={searchRef}
                  className={search(true)}
                  aria-label="Find a suite"
                  placeholder="Find a suite..."
                  type="search"
                  value={query}
                  onChange={(event) => {
                    setQuery(event.target.value);
                    setActiveIndex(null);
                  }}
                />
                <nav
                  aria-label="Test suites"
                  className="flex min-h-0 flex-col gap-1 overflow-y-auto overscroll-contain"
                >
                  {suites.map((suite, index) => (
                    <Link
                      key={suite.id}
                      to={suitePath(suite.id)}
                      ref={(element) => {
                        listRef.current[index] = element;
                      }}
                      tabIndex={activeIndex === index ? 0 : -1}
                      className={linkItem}
                      aria-current={match?.params.suiteId === suite.id ? "page" : undefined}
                      {...getItemProps({
                        onClick: () => {
                          changeOpen(false);
                          close();
                        },
                      })}
                    >
                      <span className={statusDot(suite.failed > 0)} />
                      <span className="min-w-0 flex-1 [overflow-wrap:anywhere]">
                        {suiteLabel(suite)}
                      </span>
                      <span className="mono muted text-[12px]">{suite.total}</span>
                      <Check
                        size={14}
                        className="suite-dropdown-check invisible shrink-0 text-accent group-aria-[current=page]/link:visible"
                        aria-hidden="true"
                      />
                    </Link>
                  ))}
                  {!suites.length && (
                    <p className={`muted ${empty}`} role="status">
                      {manifest.suites.length ? "No matching suites" : "No suites"}
                    </p>
                  )}
                </nav>
              </dialog>
            </FloatingFocusManager>
          </FloatingPortal>
        )}
      </section>
      {match?.params.suiteId && (
        <ScenarioNavigation
          key={match.params.suiteId}
          suiteId={match.params.suiteId}
          close={close}
        />
      )}
    </>
  );
}

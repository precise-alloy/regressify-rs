use std::time::{Duration, Instant};

use anyhow::{Result, anyhow};
use chromiumoxide::Page;

use crate::config::ResolvedConfig;
use crate::diagnostics::{
    record::{DiagnosticEvent, Field, FieldValue, SafeValue, Severity, Stage},
    runtime as diagnostics,
};

const DOM_READY_POLL: Duration = Duration::from_millis(100);
const EVALUATION_TIMEOUT: Duration = Duration::from_secs(5);
const SETTLE_QUIET_WINDOW_MS: u64 = 500;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SettleResult {
    pub settled: bool,
    pub elapsed_ms: u64,
}

pub fn is_dom_ready(state: &str) -> bool {
    matches!(state, "interactive" | "complete")
}

pub async fn wait_for_screenshot_delay(delay: Option<f64>, _log_prefix: &str) {
    if let Some(delay) = delay
        && delay > 0.0
    {
        diagnostics::event(
            Stage::ScreenshotSettling,
            DiagnosticEvent::Waiting,
            Severity::Debug,
            [(Field::TimeoutMs, FieldValue::Number(delay as u64))],
        );
        tokio::time::sleep(Duration::from_millis(delay as u64)).await;
    }
}

pub async fn prepare_screenshot(
    page: &Page,
    config: &ResolvedConfig,
    log_prefix: &str,
) -> Result<()> {
    diagnostics::stage(
        Stage::ScreenshotSettling,
        [(
            Field::TimeoutMs,
            FieldValue::Number(config.settle_timeout_ms),
        )],
        prepare_screenshot_inner(page, config, log_prefix),
    )
    .await
}

async fn prepare_screenshot_inner(
    page: &Page,
    config: &ResolvedConfig,
    log_prefix: &str,
) -> Result<()> {
    wait_for_screenshot_delay(config.delay, log_prefix).await;

    if config.freeze_animations {
        stabilize_capture_animations(page, log_prefix).await?;
    }

    if config.settle_timeout_ms > 0 {
        match wait_for_page_settle(page, Duration::from_millis(config.settle_timeout_ms)).await {
            Ok(result) if result.settled => {
                diagnostics::event(
                    Stage::ScreenshotSettling,
                    DiagnosticEvent::Decision,
                    Severity::Debug,
                    [
                        (Field::State, FieldValue::Kind(SafeValue::Complete)),
                        (Field::ElapsedMs, FieldValue::Number(result.elapsed_ms)),
                    ],
                );
            }
            Ok(result) => {
                diagnostics::event(
                    Stage::ScreenshotSettling,
                    DiagnosticEvent::Decision,
                    Severity::Warn,
                    [
                        (Field::Reason, FieldValue::Kind(SafeValue::Timeout)),
                        (Field::ElapsedMs, FieldValue::Number(result.elapsed_ms)),
                    ],
                );
            }
            Err(_) => {
                diagnostics::event(
                    Stage::ScreenshotSettling,
                    DiagnosticEvent::Decision,
                    Severity::Warn,
                    [(Field::State, FieldValue::Kind(SafeValue::Failed))],
                );
            }
        }
    }

    if config.freeze_animations {
        // Resources that completed during settlement can start new animations.
        stabilize_capture_animations(page, log_prefix).await?;
    }

    // Flag images only now, on the settled page: no observers or timers keep
    // them updated, and the settle observer never watches data attributes.
    // Callers remove the flags after their screenshot (`restore_after`).
    if let Some(simplifier) = super::image_simplification::ImageSimplifier::from_config(config)? {
        simplifier.prepare(page).await?;
    }

    diagnostics::stage(
        Stage::ScreenshotSettling,
        [(Field::Condition, FieldValue::Kind(SafeValue::Caret))],
        hide_text_carets(page),
    )
    .await?;

    Ok(())
}

async fn hide_text_carets(page: &Page) -> Result<()> {
    tokio::time::timeout(
        EVALUATION_TIMEOUT,
        super::runtime::evaluate_main_document(
            page,
            r#"(() => {
                const styleId = '__regressify_capture_stability__';
                if (!document.getElementById(styleId)) {
                    const style = document.createElement('style');
                    style.id = styleId;
                    style.textContent = '*, *::before, *::after { caret-color: transparent !important; }';
                    (document.head || document.documentElement).appendChild(style);
                }
            })()"#,
        ),
    )
    .await
    .map_err(|_| anyhow!("Timed out while hiding text carets before capture"))??;
    Ok(())
}

pub async fn stabilize_capture_animations(page: &Page, _log_prefix: &str) -> Result<()> {
    let (completed, frozen) = freeze_animations(page).await?;
    if completed > 0 || frozen > 0 {
        diagnostics::event(
            Stage::ScreenshotSettling,
            DiagnosticEvent::Decision,
            Severity::Debug,
            [
                (Field::Condition, FieldValue::Kind(SafeValue::Animation)),
                (Field::CompletedAnimations, FieldValue::Number(completed)),
                (Field::FrozenAnimations, FieldValue::Number(frozen)),
            ],
        );
    }
    Ok(())
}

/// Wait until the page has an interactive DOM after a navigation response.
pub async fn wait_for_dom_ready(page: &Page, timeout: Duration) -> Result<()> {
    let start = Instant::now();

    loop {
        let remaining = timeout.saturating_sub(start.elapsed());
        if remaining.is_zero() {
            return Err(anyhow!(
                "Timed out after {}ms waiting for the document to become interactive",
                timeout.as_millis()
            ));
        }
        let evaluation_timeout = remaining.min(EVALUATION_TIMEOUT);
        let result = tokio::time::timeout(
            evaluation_timeout,
            super::runtime::evaluate_main_document(page, "document.readyState"),
        )
        .await
        .map_err(|_| anyhow!("Timed out reading document readiness"))??;
        let state: String = result.into_value().unwrap_or_default();
        if is_dom_ready(&state) {
            return Ok(());
        }

        tokio::time::sleep(remaining.min(DOM_READY_POLL)).await;
    }
}

/// Scroll through the document to trigger viewport-based lazy loading.
pub(super) async fn trigger_lazy_loading(
    runtime: &super::runtime::Runtime,
    config: &ResolvedConfig,
) -> Result<()> {
    diagnostics::stage(
        Stage::ScreenshotSettling,
        [
            (Field::Condition, FieldValue::Kind(SafeValue::LazyLoading)),
            (
                Field::TimeoutMs,
                FieldValue::Number(config.lazy_load_timeout_ms),
            ),
        ],
        trigger_lazy_loading_inner(runtime, config),
    )
    .await
}

async fn trigger_lazy_loading_inner(
    runtime: &super::runtime::Runtime,
    config: &ResolvedConfig,
) -> Result<()> {
    let timeout = Duration::from_millis(config.lazy_load_timeout_ms);
    let script = build_lazy_load_script(
        config.lazy_load_scroll_delay_ms,
        config.lazy_load_timeout_ms,
    );
    let budget = timeout.saturating_add(Duration::from_secs(2));
    tokio::time::timeout(
        budget,
        runtime.evaluate_main_document_with_timeout(script, budget),
    )
    .await
    .map_err(|_| {
        anyhow!(
            "Timed out after {}ms while triggering lazy loading",
            timeout.as_millis()
        )
    })??;
    Ok(())
}

fn build_lazy_load_script(scroll_delay_ms: u64, timeout_ms: u64) -> String {
    let options = serde_json::json!({ "scrollDelay": scroll_delay_ms, "timeout": timeout_ms });
    format!(
        "(async () => {{ const options = {options}; {} }})()",
        r#"
                const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
                const MAX_STEPS = 150;
                const STEP = Math.max(100, Math.floor(window.innerHeight * 0.8));
                const deadline = performance.now() + options.timeout;
                const documentBottom = () => {
                    const root = document.scrollingElement || document.documentElement;
                    return Math.max(0, document.body?.scrollHeight || 0,
                        document.documentElement.scrollHeight, root.scrollHeight) - innerHeight;
                };
                const decoded = new WeakMap();
                let intersections = new WeakMap();
                let observed = new WeakSet();
                const observer = new IntersectionObserver(entries => {
                    for (const entry of entries) intersections.set(entry.target,
                        entry.isIntersecting && entry.intersectionRect.width > 0 && entry.intersectionRect.height > 0);
                });
                const candidates = () => Array.from(document.querySelectorAll(
                    'img, [data-bgset], [data-bg], [data-background]'
                ));
                const visible = (element) => {
                    const rect = element.getBoundingClientRect();
                    if (rect.width <= 0 || rect.height <= 0 || rect.bottom <= 0 ||
                        rect.top >= innerHeight || rect.right <= 0 || rect.left >= innerWidth) return false;
                    const style = getComputedStyle(element);
                    if (style.visibility === 'hidden' || style.visibility === 'collapse') return false;
                    if (!observed.has(element)) {
                        observed.add(element);
                        observer.observe(element);
                    }
                    // Bounding boxes alone include slides clipped by an overflow ancestor.
                    return intersections.get(element) ?? null;
                };
                const resourceReady = (element) => {
                    if (element.matches('.lazyload, .lazyloading') &&
                        element.matches('[data-src], [data-srcset], [data-bgset], [data-bg], [data-background]'))
                        return false;
                    if (!(element instanceof HTMLImageElement)) return true;
                    const deferred = element.getAttribute('data-src') || element.getAttribute('data-lazy-src');
                    const source = element.currentSrc || element.src;
                    const deferredPicture = element.parentElement?.tagName === 'PICTURE' &&
                        Array.from(element.parentElement.querySelectorAll('source[data-srcset]'))
                            .some(source => source.getAttribute('data-srcset') && !source.getAttribute('srcset'));
                    // A decoded placeholder is not proof that the deferred source was requested.
                    if ((!source || source.startsWith('data:')) &&
                        ((deferred && deferred !== element.getAttribute('src')) ||
                         (element.hasAttribute('data-srcset') && !element.getAttribute('srcset')) ||
                         deferredPicture))
                        return false;
                    if (!element.complete) return false;
                    if (element.naturalWidth === 0 || typeof element.decode !== 'function') return true;
                    let state = decoded.get(element);
                    if (!state || state.source !== source) {
                        const entry = { source, ready: false, promise: null };
                        entry.promise = Promise.resolve()
                            .then(() => element.decode())
                            .catch(() => {})
                            .then(() => { entry.ready = true; });
                        decoded.set(element, entry);
                        return false;
                    }
                    return state.ready;
                };
                const waitForVisibleResources = async () => {
                    observer.disconnect();
                    observer.takeRecords();
                    intersections = new WeakMap();
                    observed = new WeakSet();
                    candidates().forEach(visible);
                    await sleep(Math.min(options.scrollDelay, Math.max(0, deadline - performance.now())));
                    while (performance.now() < deadline) {
                        const checkReady = () => candidates().every(element => {
                            const inView = visible(element);
                            return inView === null ? false : !inView || resourceReady(element);
                        });
                        if (checkReady()) return;
                        const pending = candidates()
                            .map(element => {
                                const state = decoded.get(element);
                                return state && !state.ready ? state.promise : null;
                            })
                            .filter(Boolean);
                        if (pending.length > 0) {
                            await Promise.race([
                                Promise.all(pending),
                                sleep(Math.min(100, Math.max(0, deadline - performance.now()))),
                            ]);
                            if (checkReady()) return;
                        }
                        await sleep(Math.min(100, Math.max(0, deadline - performance.now())));
                    }
                    throw new Error('Timed out waiting for visible lazy-loaded resources');
                };
                let steps = 0;

                try {
                    await waitForVisibleResources();
                    while (true) {
                        if (window.scrollY >= documentBottom()) break;
                        if (steps >= MAX_STEPS) throw new Error('Exceeded the lazy-load scroll step limit');

                        const previousY = window.scrollY;
                        window.scrollBy({ left: 0, top: STEP, behavior: 'instant' });
                        steps++;
                        await waitForVisibleResources();
                        if (window.scrollY <= previousY && window.scrollY < documentBottom())
                            throw new Error('Lazy-load scrolling stopped before reaching the bottom');
                    }
                } finally {
                    observer.disconnect();
                    window.scrollTo({ left: 0, top: 0, behavior: 'instant' });
                }

                // Lazy-load handlers are often debounced after scrolling stops.
                const returnDeadline = Math.min(deadline, performance.now() + 3000);
                let stableSince = null;
                while (performance.now() < returnDeadline) {
                    const now = performance.now();
                    const atTop = Math.abs(window.scrollX) < 1 && Math.abs(window.scrollY) < 1;
                    if (atTop) {
                        if (stableSince === null) stableSince = now;
                        if (now - stableSince >= 500) return { steps };
                    } else {
                        stableSince = null;
                        window.scrollTo({ left: 0, top: 0, behavior: 'instant' });
                    }
                    await sleep(Math.min(100, Math.max(0, returnDeadline - performance.now())));
                }
                throw new Error('Timed out waiting for lazy-load scrolling to return to the top');
        "#,
    )
}

/// Freeze active CSS/Web Animations before capture.
pub async fn freeze_animations(page: &Page) -> Result<(u64, u64)> {
    let result = tokio::time::timeout(
        EVALUATION_TIMEOUT,
        super::runtime::evaluate_main_document(
            page,
            r#"(() => {
                let completed = 0;
                let frozen = 0;
                for (const animation of document.getAnimations({ subtree: true })) {
                    if (animation.playState !== 'running' && animation.playState !== 'pending') continue;
                    const timing = animation.effect && animation.effect.getTiming();
                    if (timing && timing.iterations === Infinity) {
                        try {
                            animation.pause();
                            animation.currentTime = 0;
                            frozen++;
                        } catch (_) {}
                        continue;
                    }
                    try {
                        animation.finish();
                        completed++;
                    } catch (_) {
                        animation.pause();
                        frozen++;
                    }
                }

                return { completed, frozen };
            })()"#,
        ),
    )
    .await
    .map_err(|_| anyhow!("Timed out while freezing page animations"))??;
    let value: serde_json::Value = result.into_value().unwrap_or(serde_json::Value::Null);
    let completed = value
        .get("completed")
        .and_then(|field| field.as_u64())
        .unwrap_or(0);
    let frozen = value
        .get("frozen")
        .and_then(|field| field.as_u64())
        .unwrap_or(0);
    Ok((completed, frozen))
}

/// Wait for fonts, image decoding, DOM quiet, and stable document dimensions.
pub async fn wait_for_page_settle(page: &Page, timeout: Duration) -> Result<SettleResult> {
    let script = build_settle_script(timeout.as_millis() as u64, SETTLE_QUIET_WINDOW_MS);
    let budget = timeout.saturating_add(Duration::from_secs(2));
    let result = tokio::time::timeout(budget, super::runtime::evaluate_main_document(page, script))
        .await
        .map_err(|_| anyhow!("Settle wait exceeded {}ms budget", budget.as_millis()))??;
    let value = result.value().unwrap_or(&serde_json::Value::Null);

    Ok(SettleResult {
        settled: value
            .get("settled")
            .and_then(|field| field.as_bool())
            .unwrap_or(false),
        elapsed_ms: value
            .get("elapsed")
            .and_then(|field| field.as_u64())
            .unwrap_or(0),
    })
}

fn build_settle_script(timeout_ms: u64, quiet_window_ms: u64) -> String {
    // Use performance.now() rather than Date.now() so pages or ready hooks that
    // freeze Date.now() for deterministic timestamps cannot stall settling.
    format!(
        r#"(async () => {{
            const SETTLE_TIMEOUT = {timeout_ms};
            const QUIET_WINDOW = {quiet_window_ms};
            const POLL = 100;
            const start = performance.now();
            const deadline = start + SETTLE_TIMEOUT;
            const remaining = () => Math.max(0, deadline - performance.now());
            const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
            const waitWithinDeadline = (promise) => new Promise((resolve) => {{
                const ms = remaining();
                if (ms === 0) {{ resolve(false); return; }}
                let done = false;
                const finish = (completed) => {{
                    if (done) return;
                    done = true;
                    clearTimeout(timer);
                    resolve(completed);
                }};
                const timer = setTimeout(() => finish(false), ms);
                Promise.resolve(promise).then(() => finish(true), () => finish(true));
            }});

            let lastMutation = performance.now();
            let resourceGeneration = 0;
            const relevantAttributes = new Set([
                'class', 'style', 'src', 'srcset', 'href', 'width', 'height', 'hidden', 'open',
            ]);
            const observer = new MutationObserver((mutations) => {{
                const relevant = mutations.some((mutation) =>
                    mutation.type !== 'attributes' || relevantAttributes.has(mutation.attributeName)
                );
                if (relevant) {{
                    lastMutation = performance.now();
                    resourceGeneration++;
                }}
            }});
            observer.observe(document.documentElement, {{
                attributes: true, childList: true, subtree: true, characterData: true,
                attributeFilter: Array.from(relevantAttributes),
            }});

            const waitForImage = async (image) => {{
                if (!image.complete) {{
                    const loaded = new Promise((resolve) => {{
                        image.addEventListener('load', resolve, {{ once: true }});
                        image.addEventListener('error', resolve, {{ once: true }});
                    }});
                    if (!await waitWithinDeadline(loaded)) return false;
                }}
                if (image.complete && image.naturalWidth > 0 && typeof image.decode === 'function')
                    return waitWithinDeadline(image.decode());
                return true;
            }};
            const waitForResources = async () => {{
                let fontsReady = true;
                if (document.fonts && document.fonts.ready)
                    fontsReady = await waitWithinDeadline(document.fonts.ready);
                const imageResults = await Promise.all(Array.from(document.images).map(waitForImage));
                return fontsReady && imageResults.every(Boolean);
            }};

            const signature = () => {{
                const root = document.documentElement;
                const body = document.body || root;
                return [root.scrollWidth, root.scrollHeight, body.scrollWidth, body.scrollHeight,
                    window.innerWidth, window.innerHeight].join('x');
            }};

            let lastSignature = '';
            let stableCount = 0;
            let settled = false;
            let checkedGeneration = -1;
            let resourcesReady = false;
            while (remaining() > 0) {{
                if (checkedGeneration !== resourceGeneration) {{
                    const generationBeforeCheck = resourceGeneration;
                    resourcesReady = await waitForResources();
                    checkedGeneration = generationBeforeCheck;
                    if (checkedGeneration !== resourceGeneration) {{
                        resourcesReady = false;
                        continue;
                    }}
                }}
                await sleep(Math.min(POLL, remaining()));
                const currentSignature = signature();
                const quiet = performance.now() - lastMutation >= QUIET_WINDOW;
                if (resourcesReady && currentSignature === lastSignature && quiet) {{
                    stableCount++;
                    if (stableCount >= 2) {{ settled = true; break; }}
                }} else {{
                    stableCount = 0;
                }}
                lastSignature = currentSignature;
            }}

            observer.disconnect();
            return {{ settled, elapsed: Math.round(performance.now() - start) }};
        }})()"#
    )
}

#[cfg(test)]
mod tests {
    use std::{fs, process::Command};

    use super::*;

    #[tokio::test]
    async fn screenshot_delay_waits_for_configured_milliseconds() {
        let start = Instant::now();

        wait_for_screenshot_delay(Some(25.0), "delay-test").await;

        assert!(start.elapsed() >= Duration::from_millis(25));
    }

    #[tokio::test]
    async fn screenshot_delay_skips_missing_or_non_positive_values() {
        for delay in [None, Some(0.0), Some(-1.0)] {
            tokio::time::timeout(
                Duration::from_millis(1),
                wait_for_screenshot_delay(delay, "delay-test"),
            )
            .await
            .expect("Missing or non-positive screenshot delays must not wait");
        }
    }

    #[tokio::test]
    #[ignore = "requires managed Chromium; run regressify install"]
    async fn screenshot_preparation_hides_carets_without_blurring_inputs() -> Result<()> {
        use chromiumoxide::{
            cdp::browser_protocol::page::CaptureScreenshotFormat, page::ScreenshotParams,
        };

        let (mut browser, handler, directory) =
            crate::test::browser_tests::launch_browser("capture-caret", Default::default()).await?;
        let result: Result<()> = async {
            for (freeze_animations, tag) in [
                (false, "input"),
                (true, "input"),
                (false, "textarea"),
                (true, "textarea"),
                (false, "div"),
                (true, "div"),
            ] {
                let page = browser.new_page("about:blank").await?;
                page.evaluate(format!(
                    "window.fixtureTag = {}",
                    serde_json::to_string(tag)?
                ))
                .await?;
                page.evaluate(
                    r#"(() => {
                        const input = document.createElement(window.fixtureTag);
                        input.style.cssText = 'caret-color: rgb(255, 0, 255); width: 240px; height: 50px; font: 20px monospace';
                        if (window.fixtureTag === 'div') {
                            input.contentEditable = 'true';
                            input.textContent = 'test@example.com';
                        } else {
                            input.value = 'test@example.com';
                        }
                        document.body.appendChild(input);
                        input.focus();
                        if (window.fixtureTag === 'div') {
                            document.getSelection().setBaseAndExtent(input.firstChild, 4, input.firstChild, 4);
                        } else {
                            input.setSelectionRange(4, 4);
                        }
                        window.fixtureInput = input;
                        window.fixtureBlurEvents = 0;
                        input.addEventListener('blur', () => window.fixtureBlurEvents++);
                        const animated = document.createElement('div');
                        animated.style.cssText = 'position: fixed; left: -100px; width: 10px; height: 10px';
                        document.body.appendChild(animated);
                        window.fixtureAnimation = animated.animate(
                            [{ opacity: 1 }, { opacity: 0.5 }],
                            { duration: 60000, iterations: Infinity }
                        );
                    })()"#,
                )
                .await?;
                let config = ResolvedConfig {
                    freeze_animations,
                    delay: None,
                    settle_timeout_ms: 0,
                    ..Default::default()
                };
                let mut previous_pixels = None;
                for capture_index in 0..2 {
                    if capture_index > 0 {
                        tokio::time::sleep(Duration::from_millis(350)).await;
                    }
                    prepare_screenshot(&page, &config, "caret-test").await?;
                    let state: serde_json::Value = page
                        .evaluate(
                            r#"({
                                caret: getComputedStyle(window.fixtureInput).caretColor,
                                focused: document.activeElement === window.fixtureInput,
                                selection: window.fixtureTag === 'div'
                                    ? [document.getSelection().anchorOffset, document.getSelection().focusOffset]
                                    : [window.fixtureInput.selectionStart, window.fixtureInput.selectionEnd],
                                value: window.fixtureInput.value || window.fixtureInput.textContent,
                                blurEvents: window.fixtureBlurEvents,
                                animation: window.fixtureAnimation.playState,
                            })"#,
                        )
                        .await?
                        .into_value()?;
                    anyhow::ensure!(
                        state["caret"] == "rgba(0, 0, 0, 0)",
                        "{tag} caret remains visible with freezeAnimations={freeze_animations}: {state}"
                    );
                    anyhow::ensure!(
                        state["focused"] == true
                            && state["selection"] == serde_json::json!([4, 4])
                            && state["value"] == "test@example.com"
                            && state["blurEvents"] == 0,
                        "Screenshot preparation changed the focused input: {state}"
                    );
                    let expected_animation = if freeze_animations { "paused" } else { "running" };
                    anyhow::ensure!(state["animation"] == expected_animation);
                    let png = page
                        .screenshot(
                            ScreenshotParams::builder()
                                .format(CaptureScreenshotFormat::Png)
                                .from_surface(true)
                                .omit_background(false)
                                .build(),
                        )
                        .await?;
                    let image = lodepng::decode32(png)?;
                    anyhow::ensure!(
                        image.buffer.iter().any(|pixel| (pixel.r, pixel.g, pixel.b) != (255, 255, 255)),
                        "{tag} screenshot is blank"
                    );
                    anyhow::ensure!(
                        !image.buffer.iter().any(|pixel| (pixel.r, pixel.g, pixel.b) == (255, 0, 255)),
                        "{tag} screenshot contains caret pixels"
                    );
                    if let Some(previous) = &previous_pixels {
                        anyhow::ensure!(
                            previous == &image.buffer,
                            "{tag} screenshots changed across the caret blink interval"
                        );
                    }
                    previous_pixels = Some(image.buffer);
                }
                page.close().await?;
            }
            Ok(())
        }
        .await;
        browser.close().await?;
        handler.await?;
        let _ = std::fs::remove_dir_all(directory);
        result
    }

    #[tokio::test]
    #[ignore = "requires managed Chromium; run regressify install"]
    async fn lazy_loading_waits_for_stable_offsets_and_bounds_stalled_scroll() -> Result<()> {
        let (mut browser, handler, directory) =
            crate::test::browser_tests::launch_browser("scroll-completion", Default::default())
                .await?;
        let result: Result<()> = async {
            for mode in ["blocked", "short", "delayed", "interrupted", "stalled"] {
                let page = browser.new_page("about:blank").await?;
                let runtime = super::super::runtime::Runtime::new(&browser, &page).await?;
                page.evaluate(format!(
                    r#"(() => {{
                        const mode = {};
                        document.body.style.cssText = mode === 'short'
                            ? 'margin: 0; height: 50px; width: 50px'
                            : 'margin: 0; height: 2500px; width: 5000px';
                        window.fixture = {{ lastMove: null }};
                        const scrollTo = window.scrollTo.bind(window);
                        let requested = false;
                        if (mode === 'delayed') {{
                            window.scrollTo = (...args) => {{
                                if (requested) return;
                                requested = true;
                                setTimeout(() => {{
                                    scrollTo(...args);
                                    window.fixture.lastMove = performance.now();
                                }}, 700);
                            }};
                        }} else if (mode === 'interrupted') {{
                            window.scrollTo = (...args) => {{
                                scrollTo(...args);
                                if (requested) return;
                                requested = true;
                                setTimeout(() => {{
                                    scrollTo({{ left: 200, top: 150, behavior: 'instant' }});
                                    window.fixture.lastMove = performance.now();
                                }}, 250);
                            }};
                        }} else if (mode === 'stalled') {{
                            window.scrollTo = () => {{}};
                        }} else if (mode === 'blocked') {{
                            window.scrollBy = () => {{}};
                        }}
                    }})()"#,
                    serde_json::to_string(mode)?
                ))
                .await?;
                let start = Instant::now();
                let scrolling = trigger_lazy_loading(&runtime, &ResolvedConfig::default()).await;
                if mode == "stalled" || mode == "blocked" {
                    let error = scrolling
                        .err()
                        .ok_or_else(|| anyhow!("Blocked scrolling must fail instead of capturing"))?;
                    let expected = if mode == "blocked" {
                        "before reaching the bottom"
                    } else {
                        "return to the top"
                    };
                    anyhow::ensure!(
                        format!("{error:#}").contains(expected),
                        "Unexpected blocked-scroll error: {error:#}"
                    );
                    anyhow::ensure!(start.elapsed() < Duration::from_millis(
                        crate::config::DEFAULT_LAZY_LOAD_TIMEOUT_MS
                    ));
                } else {
                    scrolling?;
                    let state: serde_json::Value = page
                        .evaluate(
                            "({ x: window.scrollX, y: window.scrollY, quietMs: window.fixture.lastMove === null ? null : performance.now() - window.fixture.lastMove })",
                        )
                        .await?
                        .into_value()?;
                    anyhow::ensure!(
                        state["x"] == 0 && state["y"] == 0,
                        "{mode} returned before reaching the top: {state}"
                    );
                    anyhow::ensure!(start.elapsed() >= Duration::from_millis(500));
                    if mode != "short" {
                        anyhow::ensure!(
                            state["quietMs"].as_f64().is_some_and(|quiet| quiet >= 500.0),
                            "{mode} returned without a quiet window after movement: {state}"
                        );
                    }
                }
                page.close().await?;
            }
            Ok(())
        }
        .await;
        browser.close().await?;
        handler.await?;
        let _ = std::fs::remove_dir_all(directory);
        result
    }

    #[tokio::test]
    #[ignore = "requires managed Chromium; run regressify install"]
    async fn lazy_loading_waits_for_deferred_image_pixels_before_scrolling_away() -> Result<()> {
        use chromiumoxide::{
            cdp::browser_protocol::page::CaptureScreenshotFormat, page::ScreenshotParams,
        };

        let (mut browser, handler, directory) =
            crate::test::browser_tests::launch_browser("slow-lazy-image", Default::default())
                .await?;
        let result: Result<()> = async {
            let page = browser.new_page("about:blank").await?;
            let runtime = super::super::runtime::Runtime::new(&browser, &page).await?;
            let top: usize = page
                .evaluate(
                    r#"(() => {
                        const top = Math.ceil(innerHeight * 1.2);
                        document.body.style.cssText = `margin: 0; height: ${top + innerHeight * 2}px`;
                        const canvas = document.createElement('canvas');
                        canvas.width = canvas.height = 64;
                        const context = canvas.getContext('2d');
                        context.fillStyle = 'rgb(255, 255, 255)';
                        context.fillRect(0, 0, 64, 64);
                        const image = document.createElement('img');
                        image.src = canvas.toDataURL();
                        context.fillStyle = 'rgb(14, 160, 80)';
                        context.fillRect(0, 0, 64, 64);
                        image.dataset.src = canvas.toDataURL();
                        image.className = 'lazyload';
                        image.style.cssText = `position: absolute; left: 24px; top: ${top}px; width: 64px; height: 64px`;
                        document.body.appendChild(image);
                        window.fixtureImage = image;
                        let timer;
                        const observer = new IntersectionObserver(entries => {
                            clearTimeout(timer);
                            if (!entries[0].isIntersecting) return;
                            timer = setTimeout(async () => {
                                image.src = image.dataset.src;
                                image.className = 'lazyloading';
                                await image.decode();
                                image.className = 'lazyloaded';
                                observer.disconnect();
                            }, 650);
                        });
                        observer.observe(image);
                        return top;
                    })()"#,
                )
                .await?
                .into_value()?;

            trigger_lazy_loading(&runtime, &ResolvedConfig::default()).await?;

            let state: serde_json::Value = page
                .evaluate(
                    "({ loaded: window.fixtureImage.className === 'lazyloaded', x: scrollX, y: scrollY })",
                )
                .await?
                .into_value()?;
            anyhow::ensure!(state["loaded"] == true, "Captured a lazy placeholder: {state}");
            anyhow::ensure!(state["x"] == 0 && state["y"] == 0);
            let png = page
                .screenshot(
                    ScreenshotParams::builder()
                        .format(CaptureScreenshotFormat::Png)
                        .full_page(true)
                        .from_surface(true)
                        .build(),
                )
                .await?;
            let image = lodepng::decode32(png)?;
            let pixel = image.buffer[(top + 32) * image.width + 56];
            anyhow::ensure!(
                (pixel.r, pixel.g, pixel.b) == (14, 160, 80),
                "Deferred image pixels were not ready: {pixel:?}"
            );
            page.close().await?;
            Ok(())
        }
        .await;
        browser.close().await?;
        handler.await?;
        let _ = fs::remove_dir_all(directory);
        result
    }

    #[tokio::test]
    #[ignore = "requires managed Chromium; run regressify install"]
    async fn lazy_loading_waits_for_resources_and_page_growth_without_waiting_for_clipped_slides()
    -> Result<()> {
        use chromiumoxide::{
            cdp::browser_protocol::page::CaptureScreenshotFormat, page::ScreenshotParams,
        };
        let pixels = vec![rgb::RGBA8::new(14, 160, 80, 255); 64 * 64];
        let png = lodepng::encode32(&pixels, 64, 64)?;
        let site = crate::test::browser_tests::Site::new(move |request| {
            std::thread::sleep(Duration::from_millis(800));
            let _ = request.respond(
                tiny_http::Response::from_data(png.clone())
                    .with_header(
                        tiny_http::Header::from_bytes("Content-Type", "image/png").unwrap(),
                    )
                    .with_header(
                        tiny_http::Header::from_bytes("Cache-Control", "no-store").unwrap(),
                    ),
            );
        })?;
        let (mut browser, handler, directory) =
            crate::test::browser_tests::launch_browser("lazy-resource-pixels", Default::default())
                .await?;
        let result: Result<()> = async {
            for mode in ["native", "picture", "background", "growing", "clipped"] {
                let page = browser.new_page("about:blank").await?;
                let runtime = super::super::runtime::Runtime::new(&browser, &page).await?;
                page.evaluate(format!(
                    "window.fixtureOptions = {}",
                    serde_json::json!({ "mode": mode, "url": format!("{}/image.png?mode={mode}", site.origin) }),
                )).await?;
                page.evaluate(r#"(() => {
                    const { mode, url } = window.fixtureOptions;
                    const top = Math.ceil(innerHeight * 1.2);
                    document.body.style.cssText = `margin: 0; height: ${top + innerHeight * 2}px`;
                    const canvas = document.createElement('canvas');
                    canvas.width = canvas.height = 64;
                    const placeholder = canvas.toDataURL();
                    window.fixtureResources = [];
                    window.fixtureMaxY = 0;
                    addEventListener('scroll', () => window.fixtureMaxY = Math.max(window.fixtureMaxY, scrollY));
                    const create = (kind, offset, resourceUrl) => {
                        const element = document.createElement(kind === 'background' ? 'div' : 'img');
                        element.style.cssText = `position: absolute; left: 24px; top: ${offset}px; width: 64px; height: 64px`;
                        let source;
                        if (kind === 'picture') {
                            const picture = document.createElement('picture');
                            source = document.createElement('source');
                            source.dataset.srcset = resourceUrl;
                            element.src = placeholder;
                            picture.append(source, element);
                            document.body.appendChild(picture);
                        } else {
                            if (kind === 'background') element.dataset.bgset = resourceUrl;
                            else if (kind === 'native') {
                                element.loading = 'lazy';
                                element.src = resourceUrl;
                            } else {
                                element.src = placeholder;
                                element.dataset.src = resourceUrl;
                            }
                            document.body.appendChild(element);
                        }
                        window.fixtureResources.push(element);
                        if (kind === 'native') return;
                        element.className = 'lazyload';
                        let timer;
                        const observer = new IntersectionObserver(entries => {
                            clearTimeout(timer);
                            if (!entries[0].isIntersecting) return;
                            timer = setTimeout(async () => {
                                element.className = 'lazyloading';
                                if (kind === 'background') {
                                    const image = new Image();
                                    image.src = resourceUrl;
                                    await image.decode();
                                    element.style.backgroundImage = `url("${resourceUrl}")`;
                                } else {
                                    if (source) source.srcset = source.dataset.srcset;
                                    else element.src = element.dataset.src;
                                    await element.decode();
                                }
                                element.className = 'lazyloaded';
                                observer.disconnect();
                                if (kind === 'growing') {
                                    const tailTop = document.documentElement.scrollHeight + 100;
                                    document.body.style.height = `${tailTop + innerHeight}px`;
                                    create('native', tailTop, resourceUrl + '&tail=1');
                                }
                            }, 650);
                        });
                        observer.observe(element);
                    };
                    create(mode === 'clipped' ? 'native' : mode, top, url);
                    if (mode === 'clipped') {
                        const container = document.createElement('div');
                        container.style.cssText = 'position: absolute; top: 24px; left: 24px; width: 1px; height: 1px; overflow: hidden';
                        const hidden = document.createElement('img');
                        hidden.className = 'lazyload';
                        hidden.src = placeholder;
                        hidden.dataset.src = url + '&hidden=1';
                        hidden.style.cssText = 'position: absolute; top: 80px; left: 80px; width: 64px; height: 64px';
                        container.appendChild(hidden);
                        document.body.appendChild(container);
                        const detached = hidden.cloneNode();
                        detached.style.display = 'none';
                        document.body.appendChild(detached);
                    }
                })()"#).await?;

                trigger_lazy_loading(&runtime, &ResolvedConfig::default()).await?;

                let state: serde_json::Value = page.evaluate(r#"({
                    positions: window.fixtureResources.map(element => Math.round(element.getBoundingClientRect().top + scrollY)),
                    loaded: window.fixtureResources.every(element => element instanceof HTMLImageElement
                        ? element.complete && element.naturalWidth === 64 && !element.currentSrc.startsWith('data:')
                        : element.className === 'lazyloaded'),
                    maxY: window.fixtureMaxY, height: innerHeight, x: scrollX, y: scrollY,
                })"#).await?.into_value()?;
                anyhow::ensure!(state["loaded"] == true, "{mode} resources are incomplete: {state}");
                anyhow::ensure!(state["x"] == 0 && state["y"] == 0, "{mode}: {state}");
                let positions: Vec<usize> = serde_json::from_value(state["positions"].clone())?;
                if mode == "growing" {
                    anyhow::ensure!(positions.len() == 2, "Did not append the deferred section: {state}");
                    anyhow::ensure!(state["maxY"].as_f64().unwrap() + state["height"].as_f64().unwrap() > positions[1] as f64);
                }
                let png = page.screenshot(ScreenshotParams::builder()
                    .format(CaptureScreenshotFormat::Png)
                    .full_page(true)
                    .from_surface(true)
                    .build()).await?;
                let image = lodepng::decode32(png)?;
                for top in positions {
                    let pixel = image.buffer[(top + 32) * image.width + 56];
                    anyhow::ensure!((pixel.r, pixel.g, pixel.b) == (14, 160, 80), "{mode} captured missing pixels: {pixel:?}");
                }
                page.close().await?;
            }
            Ok(())
        }.await;
        browser.close().await?;
        handler.await?;
        let _ = fs::remove_dir_all(directory);
        result
    }

    #[tokio::test]
    #[ignore = "requires managed Chromium; run regressify install"]
    async fn lazy_loading_bounds_unresolved_resources_and_restores_scroll_on_error() -> Result<()> {
        let (mut browser, handler, directory) =
            crate::test::browser_tests::launch_browser("lazy-resource-timeout", Default::default())
                .await?;
        let result: Result<()> = async {
            for scroll_delay in [0, u64::MAX] {
                let page = browser.new_page("about:blank").await?;
                let runtime = super::super::runtime::Runtime::new(&browser, &page).await?;
                page.evaluate(r#"(() => {
                    document.body.style.height = `${innerHeight * 3}px`;
                    const image = document.createElement('img');
                    image.className = 'lazyload';
                    image.dataset.src = 'never-activated.png';
                    image.style.cssText = `position: absolute; top: ${innerHeight * 1.2}px; left: 24px; width: 64px; height: 64px`;
                    document.body.appendChild(image);
                    const scrollBy = window.scrollBy.bind(window);
                    window.fixtureScrolls = 0;
                    window.scrollBy = (...args) => { window.fixtureScrolls++; scrollBy(...args); };
                })()"#).await?;
                let config = ResolvedConfig {
                    lazy_load_scroll_delay_ms: scroll_delay,
                    lazy_load_timeout_ms: 700,
                    ..Default::default()
                };
                let started = Instant::now();
                let error = trigger_lazy_loading(&runtime, &config).await.err()
                    .ok_or_else(|| anyhow!("Unresolved lazy resources must fail"))?;
                anyhow::ensure!(format!("{error:#}").contains("visible lazy-loaded resources"));
                anyhow::ensure!(started.elapsed() >= Duration::from_millis(700) && started.elapsed() < Duration::from_secs(4));
                let state: serde_json::Value = page.evaluate("({ x: scrollX, y: scrollY, scrolls: window.fixtureScrolls })").await?.into_value()?;
                anyhow::ensure!(state["x"] == 0 && state["y"] == 0, "{state}");
                tokio::time::sleep(Duration::from_millis(250)).await;
                let scrolls: u64 = page.evaluate("window.fixtureScrolls").await?.into_value()?;
                anyhow::ensure!(state["scrolls"] == scrolls, "Scrolling continued after timeout");
                page.close().await?;
            }
            Ok(())
        }.await;
        browser.close().await?;
        handler.await?;
        let _ = fs::remove_dir_all(directory);
        result
    }

    #[tokio::test]
    #[ignore = "requires managed Chromium; run regressify install"]
    async fn lazy_loading_honors_timeouts_beyond_the_cdp_default() -> Result<()> {
        let (mut browser, handler, directory) = crate::test::browser_tests::launch_browser(
            "lazy-load-long-timeout",
            Default::default(),
        )
        .await?;
        let result: Result<()> = async {
            let page = browser.new_page("about:blank").await?;
            let runtime = super::super::runtime::Runtime::new(&browser, &page).await?;
            let config = ResolvedConfig {
                lazy_load_scroll_delay_ms: 61_000,
                lazy_load_timeout_ms: 70_000,
                ..Default::default()
            };
            let start = Instant::now();
            trigger_lazy_loading(&runtime, &config).await?;
            anyhow::ensure!(start.elapsed() >= Duration::from_secs(61));
            page.close().await?;
            Ok(())
        }
        .await;
        browser.close().await?;
        handler.await?;
        let _ = fs::remove_dir_all(directory);
        result
    }

    #[test]
    fn dom_ready_accepts_interactive_and_complete_states() {
        assert!(!is_dom_ready("loading"));
        assert!(is_dom_ready("interactive"));
        assert!(is_dom_ready("complete"));
    }

    #[test]
    fn settle_script_has_one_deadline_and_decodes_images() {
        let script = build_settle_script(5_000, 500);

        assert!(script.contains("const deadline = start + SETTLE_TIMEOUT"));
        assert!(script.contains("document.images"));
        assert!(script.contains("image.decode"));
        assert!(script.contains("resourceGeneration"));
        assert!(script.contains("checkedGeneration = generationBeforeCheck"));
        assert!(script.contains("attributeFilter"));
        assert!(!script.contains("requestAnimationFrame"));
    }

    #[test]
    fn settle_script_is_valid_javascript_when_node_is_available() {
        if Command::new("node").arg("--version").output().is_err() {
            return;
        }

        let temp = tempfile::tempdir().unwrap();
        let script_path = temp.path().join("settle.js");
        fs::write(&script_path, build_settle_script(5_000, 500)).unwrap();
        let output = Command::new("node")
            .arg("--check")
            .arg(&script_path)
            .output()
            .unwrap();

        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn settle_script_completes_when_date_now_is_frozen() {
        if Command::new("node").arg("--version").output().is_err() {
            return;
        }

        let temp = tempfile::tempdir().unwrap();
        let script_path = temp.path().join("settle-frozen-date.js");
        let settle_script = build_settle_script(600, 100);
        let harness = format!(
            r#"Date.now = () => 1700000000000;
global.MutationObserver = class {{
    observe() {{}}
    disconnect() {{}}
}};
global.window = {{ innerWidth: 800, innerHeight: 600 }};
global.document = {{
    documentElement: {{ scrollWidth: 800, scrollHeight: 600 }},
    body: {{ scrollWidth: 800, scrollHeight: 600 }},
    fonts: {{ ready: Promise.resolve() }},
    images: [],
}};
(async () => {{
    const result = await {settle_script};
    if (!result.settled || typeof result.elapsed !== 'number') {{
        throw new Error(JSON.stringify(result));
    }}
}})().catch((error) => {{
    console.error(error);
    process.exitCode = 1;
}});
"#
        );
        fs::write(&script_path, harness).unwrap();
        let output = Command::new("node").arg(&script_path).output().unwrap();

        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

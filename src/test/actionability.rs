use std::{future::Future, time::Duration};

use super::runtime::Target as Page;
use anyhow::{Result, anyhow};

use crate::types::Frame;

/// Information about an actionable element's position.
#[derive(Debug, Clone)]
pub struct ElementInfo {
    /// X coordinate of element center relative to its document (frame or main).
    pub center_x: f64,
    /// Y coordinate of element center relative to its document (frame or main).
    pub center_y: f64,
}

/// Configuration for actionability timeouts.
#[derive(Debug, Clone)]
pub struct ActionabilityConfig {
    /// Timeout (ms) for waiting for element to exist in the DOM.
    pub element_timeout_ms: u64,
    /// Timeout (ms) for actionability checks (visible, stable, not obscured).
    pub actionability_timeout_ms: u64,
    /// Polling interval (ms) between checks.
    pub poll_interval_ms: u64,
}

impl Default for ActionabilityConfig {
    fn default() -> Self {
        Self {
            element_timeout_ms: 30_000,
            actionability_timeout_ms: 5_000,
            poll_interval_ms: 100,
        }
    }
}

fn actionability_timeout(config: &ActionabilityConfig) -> Duration {
    Duration::from_millis(config.actionability_timeout_ms)
}

pub(crate) async fn cdp_with_timeout<T, E, F>(
    timeout: Duration,
    operation: &str,
    future: F,
) -> Result<T>
where
    E: std::fmt::Display,
    F: Future<Output = std::result::Result<T, E>>,
{
    match tokio::time::timeout(timeout, future).await {
        Ok(result) => result.map_err(|error| anyhow!("{}: {}", operation, error)),
        Err(_) => Err(anyhow!(
            "Timed out after {}ms while {}",
            timeout.as_millis(),
            operation
        )),
    }
}

async fn evaluate_value(
    page: &Page,
    script: impl Into<chromiumoxide::js::Evaluation>,
    timeout: Duration,
    operation: &str,
) -> Result<serde_json::Value> {
    let result = cdp_with_timeout(timeout, operation, page.evaluate(script)).await?;
    Ok(result.into_value().unwrap_or(serde_json::Value::Null))
}

// ─── Escaping Helpers ────────────────────────────────────────────────────────

/// Escape a string for use inside a JS single-quoted string literal.
pub fn escape_js_string(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('\'', "\\'")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
        .replace('\t', "\\t")
        .replace('\0', "")
}

/// Sanitize a selector for safe insertion into `<selector> { <declaration>; }`
/// inside a `<style>` element's `textContent`.
///
/// Preserves valid CSS syntax (including `>` combinators, attribute quotes, and
/// CSS escapes) while escaping unquoted `{`, `}`, `;`, `@`, comments (`/*`),
/// unclosed quotes, and trailing backslashes so a selector cannot break out of
/// its rule or inject additional CSS rules.
pub fn escape_css_rule(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    let mut in_quote: Option<char> = None;
    let mut escaped = false;

    while let Some(ch) = chars.next() {
        if ch == '\0' {
            continue;
        }
        if escaped {
            match ch {
                '\n' | '\r' => out.push(' '),
                _ => out.push(ch),
            }
            escaped = false;
            continue;
        }
        if ch == '\\' {
            out.push('\\');
            escaped = true;
            continue;
        }
        if let Some(quote) = in_quote {
            match ch {
                c if c == quote => {
                    in_quote = None;
                    out.push(ch);
                }
                '\n' => out.push_str("\\a "),
                '\r' => {}
                _ => out.push(ch),
            }
            continue;
        }
        match ch {
            '\'' | '"' => {
                in_quote = Some(ch);
                out.push(ch);
            }
            '/' if chars.peek() == Some(&'*') => {
                chars.next();
                out.push_str("/\\*");
            }
            '{' | '}' | ';' | '@' => {
                out.push('\\');
                out.push(ch);
            }
            '\n' | '\r' | '\t' => out.push(' '),
            _ => out.push(ch),
        }
    }
    if escaped {
        out.push('\\');
    }
    if let Some(quote) = in_quote {
        out.push(quote);
    }
    out
}

// ─── Frame Helpers ───────────────────────────────────────────────────────────

/// Extract frame selectors from a Frame option.
pub fn get_frame_selectors(frame: &Option<Frame>) -> Option<Vec<String>> {
    match frame {
        Some(Frame::Single(s)) => Some(vec![s.clone()]),
        Some(Frame::Multiple(v)) => Some(v.clone()),
        None => None,
    }
}

/// Build a JS expression that navigates into nested iframes and returns
/// the target document at the deepest level.
pub fn build_frame_document_js(selectors: &[String]) -> String {
    let mut js = "document".to_string();
    for sel in selectors {
        js = format!(
            "{}.querySelector('{}').contentDocument",
            js,
            escape_js_string(sel)
        );
    }
    js
}

/// Build a JS expression that resolves the target element, optionally inside frames.
fn build_element_js(selector: &str, frame_selectors: &Option<Vec<String>>) -> String {
    let escaped_sel = escape_js_string(selector);
    if let Some(frames) = frame_selectors {
        let doc = build_frame_document_js(frames);
        format!("{}.querySelector('{}')", doc, escaped_sel)
    } else {
        format!("document.querySelector('{}')", escaped_sel)
    }
}

fn frame_selectors_js(selectors: &[String]) -> String {
    selectors
        .iter()
        .map(|selector| format!("'{}'", escape_js_string(selector)))
        .collect::<Vec<_>>()
        .join(", ")
}

#[derive(Debug, PartialEq, Eq)]
enum FrameAccess {
    Ready,
    WaitingForFrame(String),
}

fn parse_frame_access(value: &serde_json::Value) -> Result<FrameAccess> {
    if value.get("ok").and_then(|field| field.as_bool()) == Some(true) {
        return Ok(FrameAccess::Ready);
    }

    let error = value
        .get("error")
        .and_then(|field| field.as_str())
        .unwrap_or("unknown");
    let selector = value
        .get("selector")
        .and_then(|field| field.as_str())
        .unwrap_or("unknown");

    match error {
        "frame_not_found" => Ok(FrameAccess::WaitingForFrame(selector.to_string())),
        "not_a_frame" => {
            let tag = value
                .get("tag")
                .and_then(|field| field.as_str())
                .unwrap_or("unknown")
                .to_lowercase();
            Err(anyhow!(
                "Frame selector '{}' matches a <{}>, not an <iframe>",
                selector,
                tag
            ))
        }
        "cross_origin" => Err(anyhow!(
            "Cannot access cross-origin frame '{}' (its document is not reachable from the parent)",
            selector
        )),
        _ => Err(anyhow!("Frame '{}' is not accessible: {}", selector, error)),
    }
}

fn required_finite_coordinate(
    value: &serde_json::Value,
    coordinate: &str,
    operation: &str,
) -> Result<f64> {
    let value = value
        .get(coordinate)
        .and_then(|field| field.as_f64())
        .filter(|value| value.is_finite())
        .ok_or_else(|| {
            anyhow!(
                "{} returned no finite '{}' coordinate",
                operation,
                coordinate
            )
        })?;
    Ok(value)
}

/// Check whether every frame in the chain is ready for target polling.
async fn frame_access(
    page: &Page,
    frame: &Option<Frame>,
    timeout: Duration,
) -> Result<FrameAccess> {
    let Some(frame_selectors) = get_frame_selectors(frame) else {
        return Ok(FrameAccess::Ready);
    };
    if frame_selectors.is_empty() {
        return Err(anyhow!("Frame selector chain cannot be empty"));
    }

    let selectors_js = frame_selectors_js(&frame_selectors);
    let script = format!(
        r#"(() => {{
            let doc = document;
            const selectors = [{selectors_js}];
            for (const sel of selectors) {{
                const frame = doc.querySelector(sel);
                if (!frame) return {{ error: 'frame_not_found', selector: sel }};
                if (frame.tagName !== 'IFRAME' && frame.tagName !== 'FRAME')
                    return {{ error: 'not_a_frame', selector: sel, tag: frame.tagName }};
                let inner = null;
                try {{ inner = frame.contentDocument; }} catch (e) {{ inner = null; }}
                if (!inner) return {{ error: 'cross_origin', selector: sel }};
                doc = inner;
            }}
            return {{ ok: true }};
        }})()"#
    );

    let value = evaluate_value(page, script, timeout, "checking iframe accessibility").await?;
    parse_frame_access(&value)
}

// ─── Actionability Checks ────────────────────────────────────────────────────

/// Wait for an element to exist and then become visible, recording the unmet
/// condition under `stage`.
pub async fn wait_for_selector(
    page: &Page,
    selector: &str,
    frame: &Option<Frame>,
    config: &ActionabilityConfig,
    stage: crate::diagnostics::record::Stage,
) -> Result<()> {
    use crate::diagnostics::record::SafeValue;
    diagnostic_condition_in(
        stage,
        SafeValue::Exists,
        selector,
        wait_for_attached_selector(page, selector, frame, config),
    )
    .await?;
    let visibility = ActionabilityConfig {
        actionability_timeout_ms: config.element_timeout_ms,
        ..config.clone()
    };
    diagnostic_condition_in(
        stage,
        SafeValue::Visible,
        selector,
        wait_for_visible(page, selector, frame, &visibility),
    )
    .await
}

async fn wait_for_attached_selector(
    page: &Page,
    selector: &str,
    frame: &Option<Frame>,
    config: &ActionabilityConfig,
) -> Result<()> {
    let frame_selectors = get_frame_selectors(frame);
    let element_js = build_element_js(selector, &frame_selectors);
    wait_for_attachment(page, selector, frame, config, element_js, "the DOM").await
}

async fn wait_for_attachment(
    page: &Page,
    selector: &str,
    frame: &Option<Frame>,
    config: &ActionabilityConfig,
    element_js: String,
    scope: &str,
) -> Result<()> {
    let script = format!("(() => {{ return !!({element_js}); }})()");

    let timeout = std::time::Duration::from_millis(config.element_timeout_ms);
    let poll = std::time::Duration::from_millis(config.poll_interval_ms);
    let start = std::time::Instant::now();
    let exists_operation = format!("checking whether element '{selector}' exists in {scope}");
    let mut waiting_for_frame = None;

    loop {
        if start.elapsed() >= timeout {
            if let Some(frame_selector) = waiting_for_frame {
                return Err(anyhow!(
                    "Timed out after {}ms waiting for frame '{}' before finding element '{}'",
                    config.element_timeout_ms,
                    frame_selector,
                    selector
                ));
            }
            return Err(anyhow!(
                "Timed out after {}ms waiting for element '{}' to exist in {scope}",
                config.element_timeout_ms,
                selector
            ));
        }
        let remaining = timeout.saturating_sub(start.elapsed());
        let command_timeout = remaining.min(actionability_timeout(config));
        waiting_for_frame = match frame_access(page, frame, command_timeout).await? {
            FrameAccess::Ready => {
                let value =
                    evaluate_value(page, script.as_str(), command_timeout, &exists_operation)
                        .await?;
                if value.as_bool().unwrap_or(false) {
                    return Ok(());
                }
                None
            }
            FrameAccess::WaitingForFrame(frame_selector) => Some(frame_selector),
        };
        tokio::time::sleep(poll).await;
    }
}

/// Wait for element to be visible: non-zero bounding box, not CSS/content-hidden, opacity > 0.
pub async fn wait_for_visible(
    page: &Page,
    selector: &str,
    frame: &Option<Frame>,
    config: &ActionabilityConfig,
) -> Result<()> {
    let script = visibility_script(selector, frame);

    let timeout = std::time::Duration::from_millis(config.actionability_timeout_ms);
    let poll = std::time::Duration::from_millis(config.poll_interval_ms);
    let start = std::time::Instant::now();
    let visible_operation = format!("checking visibility of element '{}'", selector);
    let mut last_reason = "unknown".to_string();

    loop {
        if start.elapsed() >= timeout {
            return Err(anyhow!(
                "Element '{}' is not visible after {}ms (reason: {})",
                selector,
                config.actionability_timeout_ms,
                last_reason
            ));
        }
        let remaining = timeout.saturating_sub(start.elapsed());
        let val = evaluate_value(page, script.as_str(), remaining, &visible_operation).await?;

        if val
            .get("visible")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
        {
            return Ok(());
        }

        last_reason = val
            .get("reason")
            .and_then(|value| value.as_str())
            .unwrap_or("unknown")
            .to_string();
        tokio::time::sleep(poll).await;
    }
}

pub(super) fn visibility_script(selector: &str, frame: &Option<Frame>) -> String {
    let frame_selectors = get_frame_selectors(frame);
    let element_js = build_element_js(selector, &frame_selectors);
    format!(
        r#"(() => {{
            const el = {element_js};
            if (!el) return {{ visible: false, reason: 'not_found' }};
            const rect = el.getBoundingClientRect();
            const view = el.ownerDocument.defaultView;
            if (!view) return {{ visible: false, reason: 'no_window' }};
            if (rect.width === 0 || rect.height === 0)
                return {{ visible: false, reason: 'zero_size' }};
            for (let current = el; current; current = current.parentElement || current.getRootNode?.()?.host || null) {{
                const style = current.ownerDocument.defaultView.getComputedStyle(current);
                if (style.visibility === 'hidden' || style.visibility === 'collapse')
                    return {{ visible: false, reason: 'visibility_hidden' }};
                if (style.display === 'none')
                    return {{ visible: false, reason: 'display_none' }};
                if (style.contentVisibility === 'hidden')
                    return {{ visible: false, reason: 'content_visibility_hidden' }};
                if (parseFloat(style.opacity) === 0)
                    return {{ visible: false, reason: 'zero_opacity' }};
            }}
            return {{ visible: true }};
        }})()"#
    )
}

/// Wait for element to be stable (bounding box not changing between 2 consecutive checks).
pub async fn wait_for_stable(
    page: &Page,
    selector: &str,
    frame: &Option<Frame>,
    config: &ActionabilityConfig,
) -> Result<()> {
    let frame_selectors = get_frame_selectors(frame);
    let element_js = build_element_js(selector, &frame_selectors);
    let script = format!(
        r#"(() => {{
            const el = {element_js};
            if (!el) return null;
            const rect = el.getBoundingClientRect();
            return {{ x: rect.x, y: rect.y, width: rect.width, height: rect.height }};
        }})()"#
    );

    let timeout = std::time::Duration::from_millis(config.actionability_timeout_ms);
    let poll = std::time::Duration::from_millis(config.poll_interval_ms);
    let start = std::time::Instant::now();
    let stable_operation = format!("checking stability of element '{}'", selector);

    let mut prev_rect: Option<(f64, f64, f64, f64)> = None;
    let mut stable_streak: u32 = 0;
    const REQUIRED_STABLE_STREAK: u32 = 2;

    loop {
        if start.elapsed() >= timeout {
            return Err(anyhow!(
                "Element '{}' is not stable after {}ms (bounding box still changing)",
                selector,
                config.actionability_timeout_ms,
            ));
        }
        let remaining = timeout.saturating_sub(start.elapsed());
        let val = evaluate_value(page, script.as_str(), remaining, &stable_operation).await?;

        if let (Some(x), Some(y), Some(w), Some(h)) = (
            val.get("x").and_then(|v| v.as_f64()),
            val.get("y").and_then(|v| v.as_f64()),
            val.get("width").and_then(|v| v.as_f64()),
            val.get("height").and_then(|v| v.as_f64()),
        ) {
            if let Some((px, py, pw, ph)) = prev_rect
                && (x - px).abs() < 1.0
                && (y - py).abs() < 1.0
                && (w - pw).abs() < 1.0
                && (h - ph).abs() < 1.0
            {
                stable_streak += 1;
                if stable_streak >= REQUIRED_STABLE_STREAK {
                    return Ok(());
                }
            } else {
                stable_streak = 0;
            }
            prev_rect = Some((x, y, w, h));
        } else {
            stable_streak = 0;
        }

        tokio::time::sleep(poll).await;
    }
}

/// Scroll element into view (center of viewport).
pub async fn scroll_into_view(
    page: &Page,
    selector: &str,
    frame: &Option<Frame>,
    config: &ActionabilityConfig,
) -> Result<()> {
    cdp_with_timeout(
        actionability_timeout(config),
        "scrolling parent frames into view",
        page.scroll_frames(),
    )
    .await?;
    let frame_selectors = get_frame_selectors(frame);
    let selectors_js = frame_selectors
        .as_deref()
        .map(frame_selectors_js)
        .unwrap_or_default();
    let target_selector = escape_js_string(selector);
    let script = format!(
        r#"(() => {{
            const options = {{ block: 'center', inline: 'center', behavior: 'instant' }};
            const selectors = [{selectors_js}];
            let doc = document;
            for (const frameSelector of selectors) {{
                const frame = doc.querySelector(frameSelector);
                if (!frame) return {{ error: 'frame_not_found', selector: frameSelector }};
                frame.scrollIntoView(options);
                if (!frame.contentDocument)
                    return {{ error: 'cross_origin', selector: frameSelector }};
                doc = frame.contentDocument;
            }}
            const el = doc.querySelector('{target_selector}');
            if (!el) return {{ error: 'not_found' }};
            el.scrollIntoView(options);
            return {{ ok: true }};
        }})()"#
    );

    let operation = format!("scrolling element '{}' into view", selector);
    let value = evaluate_value(page, script, actionability_timeout(config), &operation).await?;
    if value.get("ok").and_then(|field| field.as_bool()) != Some(true) {
        let error = value
            .get("error")
            .and_then(|field| field.as_str())
            .unwrap_or("unknown");
        let frame_selector = value
            .get("selector")
            .and_then(|field| field.as_str())
            .unwrap_or("unknown");
        return Err(anyhow!(
            "Cannot scroll element '{}' into view: {} ('{}')",
            selector,
            error,
            frame_selector
        ));
    }
    // Brief wait for scroll to settle
    tokio::time::sleep(Duration::from_millis(50).min(actionability_timeout(config))).await;
    Ok(())
}

/// Check that the element is not obscured by another element at its center point.
/// Returns the element's bounding rect info on success.
pub async fn check_not_obscured(
    page: &Page,
    selector: &str,
    frame: &Option<Frame>,
    config: &ActionabilityConfig,
) -> Result<ElementInfo> {
    check_not_obscured_at(page, selector, frame, None, config).await
}

pub(super) async fn check_not_obscured_at(
    page: &Page,
    selector: &str,
    frame: &Option<Frame>,
    position: Option<crate::types::ActionPoint>,
    config: &ActionabilityConfig,
) -> Result<ElementInfo> {
    let script = match position {
        None => obscured_script(selector, frame),
        Some(_) => obscured_script_at(selector, frame, position),
    };

    let operation = format!("checking whether element '{}' is obscured", selector);
    let val = evaluate_value(page, script, actionability_timeout(config), &operation).await?;

    if val.get("ok").and_then(|v| v.as_bool()).unwrap_or(false) {
        return Ok(ElementInfo {
            center_x: required_finite_coordinate(&val, "center_x", "Obscured check")?,
            center_y: required_finite_coordinate(&val, "center_y", "Obscured check")?,
        });
    }

    let error = val
        .get("error")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");

    match error {
        "obscured" => {
            let by = val
                .get("by")
                .and_then(|field| field.as_str())
                .unwrap_or("unknown");
            Err(anyhow!(
                "Element '{}' is obscured by '{}' at the requested point",
                selector,
                by
            ))
        }
        "not_found" => Err(anyhow!(
            "Element '{}' not found for obscured check",
            selector
        )),
        "no_element_at_point" => Err(anyhow!(
            "No element found at center point of '{}' (element may be outside viewport)",
            selector
        )),
        _ => Err(anyhow!(
            "Obscured check failed for '{}': {}",
            selector,
            error
        )),
    }
}

fn obscured_script(selector: &str, frame: &Option<Frame>) -> String {
    obscured_script_at(selector, frame, None)
}

fn obscured_script_at(
    selector: &str,
    frame: &Option<Frame>,
    position: Option<crate::types::ActionPoint>,
) -> String {
    let frame_selectors = get_frame_selectors(frame);
    let element_js = build_element_js(selector, &frame_selectors);
    let doc_js = if let Some(frames) = &frame_selectors {
        build_frame_document_js(frames)
    } else {
        "document".to_string()
    };

    format!(
        r#"(() => {{
            const el = {element_js};
            if (!el) return {{ error: 'not_found' }};
            const rect = el.getBoundingClientRect();
            const position = {position};
            if (position && (position.x < 0 || position.y < 0 || position.x >= rect.width || position.y >= rect.height))
                return {{ error: 'position_outside_element' }};
            const cx = rect.x + (position ? position.x : rect.width / 2);
            const cy = rect.y + (position ? position.y : rect.height / 2);
            const topEl = {doc_js}.elementFromPoint(cx, cy);
            if (!topEl) return {{ error: 'no_element_at_point' }};
            for (let current = topEl; current; current = current.parentElement || current.getRootNode?.()?.host || null) {{
                if (current === el || el.contains?.(current)) {{
                    return {{
                        ok: true,
                        center_x: cx,
                        center_y: cy
                    }};
                }}
            }}
            // Element is obscured — report what's covering it
            const tag = topEl.tagName.toLowerCase();
            const id = topEl.id ? '#' + topEl.id : '';
            const cls = topEl.className && typeof topEl.className === 'string'
                ? '.' + topEl.className.trim().split(/\s+/).join('.')
                : '';
            return {{ error: 'obscured', by: tag + id + cls }};
        }})()"#,
        position = serde_json::json!(position),
    )
}

/// Check that a form element is enabled (not disabled).
pub async fn check_enabled(
    page: &Page,
    selector: &str,
    frame: &Option<Frame>,
    config: &ActionabilityConfig,
) -> Result<()> {
    let script = enabled_script(selector, frame);
    let operation = format!("checking whether element '{}' is enabled", selector);
    let val = evaluate_value(page, script, actionability_timeout(config), &operation).await?;

    if val
        .get("disabled")
        .and_then(|value| value.as_bool())
        .unwrap_or(false)
    {
        let state = match val.get("reason").and_then(|value| value.as_str()) {
            Some("inert") => "inert",
            _ => "disabled",
        };
        return Err(anyhow!("Element '{}' is {}", selector, state));
    }
    if val.get("error").is_some() {
        return Err(anyhow!(
            "Element '{}' not found for enabled check",
            selector
        ));
    }
    Ok(())
}

pub(super) fn enabled_script(selector: &str, frame: &Option<Frame>) -> String {
    let frame_selectors = get_frame_selectors(frame);
    let element_js = build_element_js(selector, &frame_selectors);
    format!(
        r#"(() => {{
            const el = {element_js};
            if (!el) return {{ error: 'not_found' }};
            for (let current = el; current; current = current.parentElement || current.getRootNode?.()?.host || null) {{
                if (current.inert === true || current.hasAttribute?.('inert'))
                    return {{ disabled: true, reason: 'inert' }};
            }}
            const disabled = el.matches(':disabled') || el.getAttribute('aria-disabled') === 'true';
            if (disabled) return {{ disabled: true, reason: 'disabled' }};
            return {{ disabled: false }};
        }})()"#
    )
}

/// Calculate the cumulative content-box offset of nested iframes relative to the
/// top-level viewport. Each frame has already been centered by `scroll_into_view`.
pub async fn get_frame_offset(
    page: &Page,
    frame: &Option<Frame>,
    config: &ActionabilityConfig,
) -> Result<(f64, f64)> {
    if page.has_frames() {
        return cdp_with_timeout(
            actionability_timeout(config),
            "calculating iframe offset",
            page.frame_offset(),
        )
        .await;
    }
    let frame_selectors = match get_frame_selectors(frame) {
        Some(s) => s,
        None => return Ok((0.0, 0.0)),
    };

    if frame_selectors.is_empty() {
        return Err(anyhow!("Frame selector chain cannot be empty"));
    }

    let script = frame_offset_script(&frame_selectors);

    let val = evaluate_value(
        page,
        script,
        actionability_timeout(config),
        "calculating iframe offset",
    )
    .await?;

    if let Some(error) = val.get("error").and_then(|v| v.as_str()) {
        let sel = val
            .get("selector")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        match error {
            "frame_not_visible" => return Err(anyhow!("Frame '{}' is not visible", sel)),
            "not_a_frame" => {
                let tag = val
                    .get("tag")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown")
                    .to_lowercase();
                return Err(anyhow!(
                    "Frame selector '{}' matches a <{}>, not an <iframe>",
                    sel,
                    tag
                ));
            }
            "cross_origin_frame" => {
                return Err(anyhow!(
                    "Cannot access cross-origin frame '{}' while calculating its offset",
                    sel
                ));
            }
            _ => {
                return Err(anyhow!(
                    "Frame offset calculation failed: {} ('{}')",
                    error,
                    sel
                ));
            }
        }
    }

    let x = required_finite_coordinate(&val, "x", "Frame offset calculation")?;
    let y = required_finite_coordinate(&val, "y", "Frame offset calculation")?;
    Ok((x, y))
}

fn frame_offset_script(frame_selectors: &[String]) -> String {
    format!(
        r#"(() => {{
            let totalX = 0, totalY = 0;
            let currentDoc = document;
            const selectors = [{}];
            for (const sel of selectors) {{
                const frame = currentDoc.querySelector(sel);
                if (!frame) return {{ error: 'frame_not_found', selector: sel }};
                if (frame.tagName !== 'IFRAME' && frame.tagName !== 'FRAME')
                    return {{ error: 'not_a_frame', selector: sel, tag: frame.tagName }};
                const rect = frame.getBoundingClientRect();
                const style = currentDoc.defaultView.getComputedStyle(frame);
                if (rect.width === 0 || rect.height === 0 || style.display === 'none'
                    || style.visibility === 'hidden' || style.visibility === 'collapse'
                    || style.contentVisibility === 'hidden'
                    || parseFloat(style.opacity) === 0) {{
                    return {{ error: 'frame_not_visible', selector: sel }};
                }}
                totalX += rect.x + frame.clientLeft;
                totalY += rect.y + frame.clientTop;
                currentDoc = frame.contentDocument;
                if (!currentDoc) return {{ error: 'cross_origin_frame', selector: sel }};
            }}
            return {{ x: totalX, y: totalY }};
        }})()"#,
        frame_selectors_js(frame_selectors)
    )
}

/// When the target element lives inside an iframe, verify that each iframe is
/// rendered and not obscured in its parent-document chain.
/// `check_not_obscured` only inspects the target frame's own document, so an
/// overlay covering an outer or intermediate iframe would otherwise go
/// undetected and a CDP click would land on that overlay.
///
/// `center_x`/`center_y` are the element's center relative to its own frame
/// document; this projects them through each frame boundary and runs
/// `elementFromPoint` in every parent document.
async fn check_frame_not_obscured(
    page: &Page,
    frame: &Option<Frame>,
    center_x: f64,
    center_y: f64,
    config: &ActionabilityConfig,
) -> Result<()> {
    cdp_with_timeout(
        actionability_timeout(config),
        "checking whether iframe chain is obscured",
        page.check_frame_coverage(center_x, center_y),
    )
    .await?;
    let frame_selectors = match get_frame_selectors(frame) {
        Some(s) if !s.is_empty() => s,
        _ => return Ok(()),
    };

    let script = frame_coverage_script(&frame_selectors, center_x, center_y);
    let val = evaluate_value(
        page,
        script,
        actionability_timeout(config),
        "checking whether iframe chain is obscured",
    )
    .await?;

    parse_frame_coverage_result(&frame_selectors, &val)
}

fn parse_frame_coverage_result(
    frame_selectors: &[String],
    value: &serde_json::Value,
) -> Result<()> {
    if value.get("ok").and_then(|v| v.as_bool()).unwrap_or(false) {
        return Ok(());
    }

    let frame_selector = value
        .get("frame")
        .and_then(|value| value.as_str())
        .unwrap_or(&frame_selectors[0]);
    match value
        .get("error")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
    {
        "obscured" => {
            let by = value
                .get("by")
                .and_then(|field| field.as_str())
                .unwrap_or("unknown");
            Err(anyhow!(
                "Element inside frame '{}' is obscured in its parent document by '{}'",
                frame_selector,
                by
            ))
        }
        "no_element_at_point" => Err(anyhow!(
            "Frame '{}' is outside its parent viewport at the target point",
            frame_selector
        )),
        "frame_not_visible" => Err(anyhow!("Frame '{}' is not visible", frame_selector)),
        "frame_inert" => Err(anyhow!("Frame '{}' is inert", frame_selector)),
        "not_a_frame" => Err(anyhow!(
            "Frame selector '{}' matches a <{}>, not an <iframe>",
            frame_selector,
            value
                .get("tag")
                .and_then(|value| value.as_str())
                .unwrap_or("unknown")
                .to_lowercase()
        )),
        "cross_origin" => Err(anyhow!(
            "Cannot access cross-origin frame '{}' for coverage checking",
            frame_selector
        )),
        error => Err(anyhow!(
            "Frame coverage check failed for '{}': {}",
            frame_selector,
            error
        )),
    }
}

fn frame_coverage_script(frame_selectors: &[String], center_x: f64, center_y: f64) -> String {
    format!(
        r#"(() => {{
            const selectors = [{}];
            const frames = [];
            let currentDocument = document;
            for (const selector of selectors) {{
                const frame = currentDocument.querySelector(selector);
                if (!frame) return {{ error: 'frame_not_found', frame: selector }};
                if (frame.tagName !== 'IFRAME' && frame.tagName !== 'FRAME')
                    return {{ error: 'not_a_frame', frame: selector, tag: frame.tagName }};
                const rect = frame.getBoundingClientRect();
                const view = currentDocument.defaultView;
                if (!view) return {{ error: 'no_window', frame: selector }};
                if (rect.width === 0 || rect.height === 0)
                    return {{ error: 'frame_not_visible', frame: selector }};
                for (let current = frame; current; current = current.parentElement) {{
                    const style = view.getComputedStyle(current);
                    if (current.inert === true || current.hasAttribute?.('inert'))
                        return {{ error: 'frame_inert', frame: selector }};
                    if (style.display === 'none'
                        || style.visibility === 'hidden' || style.visibility === 'collapse'
                        || style.contentVisibility === 'hidden'
                        || parseFloat(style.opacity) === 0) {{
                        return {{ error: 'frame_not_visible', frame: selector }};
                    }}
                }}
                if (!frame.contentDocument)
                    return {{ error: 'cross_origin', frame: selector }};
                frames.push({{ selector, frame, document: currentDocument, rect }});
                currentDocument = frame.contentDocument;
            }}

            let x = {};
            let y = {};
            for (let index = frames.length - 1; index >= 0; index--) {{
                const entry = frames[index];
                x += entry.rect.left + entry.frame.clientLeft;
                y += entry.rect.top + entry.frame.clientTop;
                const topElement = entry.document.elementFromPoint(x, y);
                if (!topElement)
                    return {{ error: 'no_element_at_point', frame: entry.selector }};
                if (topElement === entry.frame || entry.frame.contains(topElement)) continue;
                const tag = topElement.tagName.toLowerCase();
                const id = topElement.id ? '#' + topElement.id : '';
                const cls = topElement.className && typeof topElement.className === 'string'
                    ? '.' + topElement.className.trim().split(/\s+/).join('.')
                    : '';
                return {{ error: 'obscured', frame: entry.selector, by: tag + id + cls }};
            }}
            return {{ ok: true }};
        }})()"#,
        frame_selectors_js(frame_selectors),
        center_x,
        center_y,
    )
}

/// Full actionability check: wait for element to exist, be visible, stable,
/// scroll into view, verify not obscured, and check enabled.
/// Returns element center coordinates (relative to its document context).
pub async fn ensure_actionable(
    page: &Page,
    selector: &str,
    frame: &Option<Frame>,
    config: &ActionabilityConfig,
) -> Result<ElementInfo> {
    ensure_actionable_at(page, selector, frame, None, config).await
}

pub(super) async fn ensure_actionable_at(
    page: &Page,
    selector: &str,
    frame: &Option<Frame>,
    position: Option<crate::types::ActionPoint>,
    config: &ActionabilityConfig,
) -> Result<ElementInfo> {
    use crate::diagnostics::record::SafeValue;
    // 1. Wait for element to exist
    diagnostic_condition(
        SafeValue::Exists,
        selector,
        wait_for_attached_selector(page, selector, frame, config),
    )
    .await?;

    // 2. Wait for element to be visible
    diagnostic_condition(
        SafeValue::Visible,
        selector,
        wait_for_visible(page, selector, frame, config),
    )
    .await?;

    // 3. Wait for element to be stable
    diagnostic_condition(
        SafeValue::Stable,
        selector,
        wait_for_stable(page, selector, frame, config),
    )
    .await?;

    // 4. Scroll element into viewport
    diagnostic_condition(
        SafeValue::Scroll,
        selector,
        scroll_into_view(page, selector, frame, config),
    )
    .await?;

    // 5. Check element is not obscured (also returns position info)
    let started = std::time::Instant::now();
    let mut condition = SafeValue::Unobscured;
    let mut attempts = 0;
    loop {
        attempts += 1;
        let result = async {
            condition = SafeValue::Unobscured;
            let info = check_not_obscured_at(page, selector, frame, position, config).await?;
            check_frame_not_obscured(page, frame, info.center_x, info.center_y, config).await?;
            condition = SafeValue::Enabled;
            check_enabled(page, selector, frame, config).await?;
            Ok(info)
        }
        .await;
        match result {
            Ok(info) => return Ok(info),
            Err(error) if started.elapsed() >= actionability_timeout(config) => {
                use crate::diagnostics::{record::*, runtime as diagnostics};
                diagnostics::event(
                    Stage::ActionsFrames,
                    DiagnosticEvent::Waiting,
                    Severity::Error,
                    [
                        (Field::Condition, FieldValue::Kind(condition)),
                        (Field::RetryAttempt, FieldValue::Number(attempts)),
                    ],
                );
                return Err(error);
            }
            Err(_) => tokio::time::sleep(Duration::from_millis(config.poll_interval_ms)).await,
        }
    }
}

pub(crate) async fn diagnostic_condition<Output>(
    condition: crate::diagnostics::record::SafeValue,
    selector: &str,
    work: impl Future<Output = Result<Output>>,
) -> Result<Output> {
    diagnostic_condition_in(
        crate::diagnostics::record::Stage::ActionsFrames,
        condition,
        selector,
        work,
    )
    .await
}

/// Keep the source selector beside the unmet condition, without action input values.
pub(crate) async fn diagnostic_condition_in<Output>(
    stage: crate::diagnostics::record::Stage,
    condition: crate::diagnostics::record::SafeValue,
    selector: &str,
    work: impl Future<Output = Result<Output>>,
) -> Result<Output> {
    use crate::diagnostics::{record::*, runtime as diagnostics};
    diagnostics::stage(
        stage,
        [
            (Field::Condition, FieldValue::Kind(condition)),
            (
                Field::Target,
                FieldValue::Identity(SafeIdentity::source(selector)),
            ),
        ],
        work,
    )
    .await
}

/// Full actionability check for focus, typing, key presses, and selection.
/// These interactions must meet the same visibility, stability, viewport,
/// coverage, and enabled-state requirements as a pointer interaction.
pub async fn ensure_focusable(
    page: &Page,
    selector: &str,
    frame: &Option<Frame>,
    config: &ActionabilityConfig,
) -> Result<()> {
    ensure_actionable(page, selector, frame, config)
        .await
        .map(|_| ())
}

/// Focus an actionable element and verify the target document reports it as
/// active. A plain `element.focus()` can silently fail for non-focusable nodes.
pub async fn focus_element(
    page: &Page,
    selector: &str,
    frame: &Option<Frame>,
    config: &ActionabilityConfig,
) -> Result<()> {
    let script = focus_element_script(selector, frame);
    let operation = format!("focusing element '{}'", selector);
    let value = evaluate_value(page, script, actionability_timeout(config), &operation).await?;
    parse_focus_result(selector, &value)
}

fn focus_element_script(selector: &str, frame: &Option<Frame>) -> String {
    let frame_selectors = get_frame_selectors(frame);
    let element_js = build_element_js(selector, &frame_selectors);
    let document_js = frame_selectors
        .as_ref()
        .map(|selectors| build_frame_document_js(selectors))
        .unwrap_or_else(|| "document".to_string());
    format!(
        r#"(() => {{
            const targetDocument = {document_js};
            const element = {element_js};
            if (!element) return {{ error: 'not_found' }};
            element.focus({{ preventScroll: true }});
            for (let current = targetDocument.activeElement; current; current = current.parentElement || current.getRootNode?.()?.host || null) {{
                if (current === element) return {{ ok: true }};
            }}
            const active = targetDocument.activeElement;
            const tag = active ? active.tagName.toLowerCase() : 'none';
            const id = active && active.id ? '#' + active.id : '';
            return {{ error: 'not_focusable', active: tag + id }};
        }})()"#
    )
}

fn parse_focus_result(selector: &str, value: &serde_json::Value) -> Result<()> {
    if value.get("ok").and_then(|field| field.as_bool()) == Some(true) {
        return Ok(());
    }
    match value
        .get("error")
        .and_then(|field| field.as_str())
        .unwrap_or("unknown")
    {
        "not_found" => Err(anyhow!("Element '{}' disappeared before focus", selector)),
        "not_focusable" => Err(anyhow!(
            "Element '{}' could not receive focus; active element is '{}'",
            selector,
            value
                .get("active")
                .and_then(|field| field.as_str())
                .unwrap_or("unknown")
        )),
        error => Err(anyhow!("Failed to focus element '{}': {}", selector, error)),
    }
}

/// Wait for a match using the action selector engine, without requiring visibility.
pub async fn ensure_exists(
    page: &Page,
    selector: &str,
    frame: &Option<Frame>,
    config: &ActionabilityConfig,
) -> Result<()> {
    diagnostic_condition(
        crate::diagnostics::record::SafeValue::Exists,
        selector,
        wait_for_attached_selector(page, selector, frame, config),
    )
    .await
}

/// A document stylesheet cannot reach shadow roots or use the action selector engine.
/// `page` already targets the selected frame; `window.document` bypasses its action proxy.
pub(super) async fn ensure_stylesheet_target(
    page: &Page,
    selector: &str,
    config: &ActionabilityConfig,
) -> Result<()> {
    let lookup = format!(
        "window.document.querySelector({})",
        serde_json::to_string(selector)?
    );
    diagnostic_condition(
        crate::diagnostics::record::SafeValue::Exists,
        selector,
        wait_for_attachment(
            page,
            selector,
            &None,
            config,
            lookup,
            "the selected document (standard CSS only; shadow roots are not searched)",
        ),
    )
    .await
}

// ─── CDP Mouse Helpers ───────────────────────────────────────────────────────

/// Perform a CDP click at the given viewport coordinates.
pub async fn cdp_click(page: &Page, x: f64, y: f64, config: &ActionabilityConfig) -> Result<()> {
    use chromiumoxide::cdp::browser_protocol::input::{
        DispatchMouseEventParams, DispatchMouseEventType, MouseButton,
    };

    // Move to position
    let move_cmd = DispatchMouseEventParams::builder()
        .r#type(DispatchMouseEventType::MouseMoved)
        .x(x)
        .y(y)
        .build()
        .map_err(|e| anyhow!("Failed to build mouse move: {:?}", e))?;
    cdp_with_timeout(
        actionability_timeout(config),
        "dispatching mouse move for click",
        page.execute(move_cmd),
    )
    .await?;

    // Press
    let press_cmd = DispatchMouseEventParams::builder()
        .r#type(DispatchMouseEventType::MousePressed)
        .x(x)
        .y(y)
        .button(MouseButton::Left)
        .click_count(1)
        .build()
        .map_err(|e| anyhow!("Failed to build mouse press: {:?}", e))?;
    cdp_with_timeout(
        actionability_timeout(config),
        "dispatching mouse press",
        page.execute(press_cmd),
    )
    .await?;

    // Release
    let release_cmd = DispatchMouseEventParams::builder()
        .r#type(DispatchMouseEventType::MouseReleased)
        .x(x)
        .y(y)
        .button(MouseButton::Left)
        .click_count(1)
        .build()
        .map_err(|e| anyhow!("Failed to build mouse release: {:?}", e))?;
    cdp_with_timeout(
        actionability_timeout(config),
        "dispatching mouse release",
        page.execute(release_cmd),
    )
    .await?;

    Ok(())
}

/// Perform a CDP hover (mouse move) at the given viewport coordinates.
pub async fn cdp_hover(page: &Page, x: f64, y: f64, config: &ActionabilityConfig) -> Result<()> {
    use chromiumoxide::cdp::browser_protocol::input::{
        DispatchMouseEventParams, DispatchMouseEventType,
    };

    let move_cmd = DispatchMouseEventParams::builder()
        .r#type(DispatchMouseEventType::MouseMoved)
        .x(x)
        .y(y)
        .build()
        .map_err(|e| anyhow!("Failed to build mouse move: {:?}", e))?;
    cdp_with_timeout(
        actionability_timeout(config),
        "dispatching mouse move for hover",
        page.execute(move_cmd),
    )
    .await?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{fs, process::Command};

    use super::*;

    #[tokio::test]
    async fn cdp_timeout_bounds_stalled_operations() {
        let result = cdp_with_timeout(
            Duration::from_millis(1),
            "evaluating actionability",
            std::future::pending::<std::result::Result<(), std::io::Error>>(),
        )
        .await;

        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("Timed out after 1ms while evaluating actionability")
        );
    }

    #[test]
    fn frame_access_distinguishes_retryable_missing_frame() {
        let value = serde_json::json!({
            "error": "frame_not_found",
            "selector": "#application-frame"
        });

        assert_eq!(
            parse_frame_access(&value).unwrap(),
            FrameAccess::WaitingForFrame("#application-frame".to_string())
        );
    }

    #[test]
    fn frame_access_rejects_non_frame_and_cross_origin_targets() {
        let non_frame = serde_json::json!({
            "error": "not_a_frame",
            "selector": ".panel",
            "tag": "DIV"
        });
        let cross_origin = serde_json::json!({
            "error": "cross_origin",
            "selector": "#external"
        });

        assert!(
            parse_frame_access(&non_frame)
                .unwrap_err()
                .to_string()
                .contains("matches a <div>")
        );
        assert!(
            parse_frame_access(&cross_origin)
                .unwrap_err()
                .to_string()
                .contains("cross-origin")
        );
    }

    #[test]
    fn frame_selector_literals_are_escaped() {
        assert_eq!(
            frame_selectors_js(&["iframe[name='checkout']".to_string()]),
            "'iframe[name=\\'checkout\\']'"
        );
    }

    #[test]
    fn focus_result_reports_a_non_focusable_target() {
        let error = parse_focus_result(
            "#status",
            &serde_json::json!({ "error": "not_focusable", "active": "body" }),
        )
        .unwrap_err();

        assert!(error.to_string().contains("#status"));
        assert!(error.to_string().contains("active element is 'body'"));
    }

    #[test]
    fn focus_script_is_valid_javascript_when_node_is_available() {
        if Command::new("node").arg("--version").output().is_err() {
            return;
        }

        let temp = tempfile::tempdir().unwrap();
        let script_path = temp.path().join("focus.js");
        fs::write(&script_path, focus_element_script("#email", &None)).unwrap();
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
    fn focus_script_activates_the_requested_top_level_element() {
        if Command::new("node").arg("--version").output().is_err() {
            return;
        }

        let temp = tempfile::tempdir().unwrap();
        let script_path = temp.path().join("focus-runtime.js");
        let script = focus_element_script("#email", &None);
        fs::write(
            &script_path,
            format!(
                "const element = {{ focus() {{ document.activeElement = this; }} }};\n\
                 global.document = {{ activeElement: null, querySelector: () => element }};\n\
                 const result = {script};\n\
                 if (!result.ok || document.activeElement !== element) throw new Error(JSON.stringify(result));\n"
            ),
        )
        .unwrap();
        let output = Command::new("node").arg(&script_path).output().unwrap();

        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn frame_offset_script_rejects_non_frame_targets() {
        if Command::new("node").arg("--version").output().is_err() {
            return;
        }

        let temp = tempfile::tempdir().unwrap();
        let script_path = temp.path().join("frame-offset-non-frame.js");
        let script = frame_offset_script(&["#target".to_string()]);
        fs::write(
            &script_path,
            format!(
                r#"const target = {{ tagName: 'DIV' }};
global.document = {{ querySelector: () => target }};
const result = {script};
if (result.error !== 'not_a_frame' || result.tag !== 'DIV')
    throw new Error(JSON.stringify(result));
"#
            ),
        )
        .unwrap();
        let output = Command::new("node").arg(&script_path).output().unwrap();

        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn frame_offset_script_rejects_content_hidden_frames() {
        if Command::new("node").arg("--version").output().is_err() {
            return;
        }

        let temp = tempfile::tempdir().unwrap();
        let script_path = temp.path().join("frame-offset-content-visibility.js");
        let script = frame_offset_script(&["#target".to_string()]);
        fs::write(
            &script_path,
            format!(
                r#"const target = {{
    tagName: 'IFRAME',
    getBoundingClientRect: () => ({{ x: 0, y: 0, width: 10, height: 10 }}),
}};
global.document = {{
    querySelector: () => target,
    defaultView: {{
        getComputedStyle: () => ({{
            display: 'block', visibility: 'visible', contentVisibility: 'hidden', opacity: '1',
        }}),
    }},
}};
const result = {script};
if (result.error !== 'frame_not_visible') throw new Error(JSON.stringify(result));
"#
            ),
        )
        .unwrap();
        let output = Command::new("node").arg(&script_path).output().unwrap();

        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn visibility_script_treats_content_hidden_elements_as_non_visible() {
        if Command::new("node").arg("--version").output().is_err() {
            return;
        }

        let temp = tempfile::tempdir().unwrap();
        let script_path = temp.path().join("content-visibility-runtime.js");
        let script = visibility_script("#target", &None);
        let runtime = format!(
            r#"const element = {{
    parentElement: null,
    ownerDocument: {{
        defaultView: {{
            getComputedStyle: () => ({{
                display: 'block',
                visibility: 'visible',
                contentVisibility: 'hidden',
                opacity: '1',
            }}),
        }},
    }},
    getBoundingClientRect: () => ({{ width: 10, height: 10 }}),
}};
global.document = {{ querySelector: () => element }};
const result = {script};
if (result.visible !== false || result.reason !== 'content_visibility_hidden')
    throw new Error(JSON.stringify(result));
"#
        );
        fs::write(&script_path, runtime).unwrap();
        let output = Command::new("node").arg(&script_path).output().unwrap();

        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn enabled_script_rejects_inert_ancestors() {
        if Command::new("node").arg("--version").output().is_err() {
            return;
        }

        let temp = tempfile::tempdir().unwrap();
        let script_path = temp.path().join("inert-runtime.js");
        let script = enabled_script("#target", &None);
        let runtime = format!(
            r#"const inertAncestor = {{ parentElement: null, inert: true, hasAttribute: () => false }};
const target = {{
    parentElement: inertAncestor,
    inert: false,
    hasAttribute: () => false,
    matches: () => false,
    getAttribute: () => null,
}};
global.document = {{ querySelector: () => target }};
const result = {script};
if (result.disabled !== true || result.reason !== 'inert')
    throw new Error(JSON.stringify(result));
"#
        );
        fs::write(&script_path, runtime).unwrap();
        let output = Command::new("node").arg(&script_path).output().unwrap();

        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn frame_coverage_script_detects_an_intermediate_frame_overlay() {
        if Command::new("node").arg("--version").output().is_err() {
            return;
        }

        let temp = tempfile::tempdir().unwrap();
        let script_path = temp.path().join("frame-coverage-runtime.js");
        let coverage_script =
            frame_coverage_script(&["#outer".to_string(), "#inner".to_string()], 5.0, 6.0);
        let setup = r#"
const overlay = { tagName: 'DIV', id: 'overlay', className: 'modal' };
const innerDocument = {};
const inner = {
    tagName: 'IFRAME', contentDocument: innerDocument, clientLeft: 0, clientTop: 0,
    getBoundingClientRect: () => ({ left: 10, top: 20, width: 100, height: 100 }),
    contains: () => false,
};
const outerDocument = {
    querySelector: (selector) => selector === '#inner' ? inner : null,
    elementFromPoint: () => overlay,
    defaultView: { getComputedStyle: () => ({ display: 'block', visibility: 'visible', contentVisibility: 'visible', opacity: '1' }) },
};
const outer = {
    tagName: 'IFRAME', contentDocument: outerDocument, clientLeft: 0, clientTop: 0,
    getBoundingClientRect: () => ({ left: 100, top: 200, width: 300, height: 200 }),
    contains: () => false,
};
global.document = {
    querySelector: (selector) => selector === '#outer' ? outer : null,
    elementFromPoint: () => outer,
    defaultView: { getComputedStyle: () => ({ display: 'block', visibility: 'visible', contentVisibility: 'visible', opacity: '1' }) },
};
const result =
"#;
        let assertion = r#";
if (result.error !== 'obscured' || result.frame !== '#inner' || result.by !== 'div#overlay.modal')
    throw new Error(JSON.stringify(result));
"#;
        fs::write(&script_path, format!("{setup}{coverage_script}{assertion}")).unwrap();
        let output = Command::new("node").arg(&script_path).output().unwrap();

        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn frame_coverage_script_rejects_hidden_frame_ancestors() {
        if Command::new("node").arg("--version").output().is_err() {
            return;
        }

        let temp = tempfile::tempdir().unwrap();
        let script_path = temp.path().join("frame-coverage-hidden-runtime.js");
        let coverage_script = frame_coverage_script(&["#embedded".to_string()], 5.0, 6.0);
        let runtime = format!(
            r#"const hiddenContainer = {{ parentElement: null }};
const frame = {{
    tagName: 'IFRAME', contentDocument: {{}}, clientLeft: 0, clientTop: 0,
    parentElement: hiddenContainer,
    getBoundingClientRect: () => ({{ left: 10, top: 20, width: 100, height: 100 }}),
}};
global.document = {{
    querySelector: () => frame,
    defaultView: {{
        getComputedStyle: (element) => ({{
            display: 'block',
            visibility: 'visible',
            contentVisibility: 'visible',
            opacity: element === hiddenContainer ? '0' : '1',
        }}),
    }},
}};
const result = {coverage_script};
if (result.error !== 'frame_not_visible' || result.frame !== '#embedded')
    throw new Error(JSON.stringify(result));
"#
        );
        fs::write(&script_path, runtime).unwrap();
        let output = Command::new("node").arg(&script_path).output().unwrap();

        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn frame_coverage_script_rejects_inert_frame_ancestors() {
        if Command::new("node").arg("--version").output().is_err() {
            return;
        }

        let temp = tempfile::tempdir().unwrap();
        let script_path = temp.path().join("frame-coverage-inert-runtime.js");
        let coverage_script = frame_coverage_script(&["#embedded".to_string()], 5.0, 6.0);
        let runtime = format!(
            r#"const inertContainer = {{ parentElement: null, inert: true, hasAttribute: () => false }};
const frame = {{
    tagName: 'IFRAME', contentDocument: {{}}, clientLeft: 0, clientTop: 0,
    parentElement: inertContainer, inert: false, hasAttribute: () => false,
    getBoundingClientRect: () => ({{ left: 10, top: 20, width: 100, height: 100 }}),
}};
global.document = {{
    querySelector: () => frame,
    defaultView: {{
        getComputedStyle: () => ({{
            display: 'block', visibility: 'visible', contentVisibility: 'visible', opacity: '1',
        }}),
    }},
}};
const result = {coverage_script};
if (result.error !== 'frame_inert' || result.frame !== '#embedded')
    throw new Error(JSON.stringify(result));
"#
        );
        fs::write(&script_path, runtime).unwrap();
        let output = Command::new("node").arg(&script_path).output().unwrap();

        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn frame_coverage_result_reports_invalid_frame_boundaries() {
        let frames = vec!["#embed".to_string()];
        let non_frame = parse_frame_coverage_result(
            &frames,
            &serde_json::json!({
                "error": "not_a_frame",
                "frame": "#embed",
                "tag": "DIV"
            }),
        )
        .unwrap_err();
        assert!(non_frame.to_string().contains("matches a <div>"));

        let cross_origin = parse_frame_coverage_result(
            &frames,
            &serde_json::json!({ "error": "cross_origin", "frame": "#embed" }),
        )
        .unwrap_err();
        assert!(
            cross_origin
                .to_string()
                .contains("cross-origin frame '#embed'")
        );

        let hidden = parse_frame_coverage_result(
            &frames,
            &serde_json::json!({ "error": "frame_not_visible", "frame": "#embed" }),
        )
        .unwrap_err();
        assert!(hidden.to_string().contains("Frame '#embed' is not visible"));

        let inert = parse_frame_coverage_result(
            &frames,
            &serde_json::json!({ "error": "frame_inert", "frame": "#embed" }),
        )
        .unwrap_err();
        assert!(inert.to_string().contains("Frame '#embed' is inert"));
    }

    #[test]
    fn actionability_coordinates_must_be_present_and_finite() {
        let value = serde_json::json!({ "x": 12.5, "y": 7.0 });
        assert_eq!(
            required_finite_coordinate(&value, "x", "test operation").unwrap(),
            12.5
        );
        assert!(
            required_finite_coordinate(&value, "center_x", "test operation")
                .unwrap_err()
                .to_string()
                .contains("no finite 'center_x' coordinate")
        );
    }

    #[test]
    fn shadow_dom_hit_testing_focus_visibility_and_enabled_checks_cross_shadow_roots() {
        if Command::new("node").arg("--version").output().is_err() {
            return;
        }

        let temp = tempfile::tempdir().unwrap();
        let script_path = temp.path().join("shadow-dom-actionability.js");
        let obscured = obscured_script("#host", &None);
        let focus = focus_element_script("#host", &None);
        let visible = visibility_script("#shadow-child", &None);
        let enabled = enabled_script("#shadow-child", &None);
        let runtime = format!(
            r#"let hostOpacity = '1';
let hostInert = false;
const host = {{
    tagName: 'MY-BUTTON',
    parentElement: null,
    get inert() {{ return hostInert; }},
    hasAttribute: () => false,
    contains: () => false,
    getBoundingClientRect: () => ({{ x: 10, y: 20, width: 100, height: 40 }}),
    focus() {{ document.activeElement = shadowChild; }},
    ownerDocument: {{
        defaultView: {{
            getComputedStyle: (el) => ({{
                display: 'block',
                visibility: 'visible',
                contentVisibility: 'visible',
                opacity: el === host ? hostOpacity : '1',
            }}),
        }},
    }},
}};
const shadowChild = {{
    tagName: 'SPAN',
    id: 'shadow-child',
    parentElement: null,
    inert: false,
    hasAttribute: () => false,
    matches: () => false,
    getAttribute: () => null,
    getRootNode: () => ({{ host }}),
    getBoundingClientRect: () => ({{ x: 10, y: 20, width: 100, height: 40 }}),
    ownerDocument: host.ownerDocument,
}};
global.document = {{
    activeElement: null,
    querySelector: (sel) => sel === '#host' ? host : shadowChild,
    elementFromPoint: () => shadowChild,
}};

const obscuredResult = {obscured};
if (!obscuredResult.ok || obscuredResult.center_x !== 60 || obscuredResult.center_y !== 40)
    throw new Error('obscured check failed for shadow child: ' + JSON.stringify(obscuredResult));

const focusResult = {focus};
if (!focusResult.ok)
    throw new Error('focus check failed for delegated shadow focus: ' + JSON.stringify(focusResult));

hostOpacity = '0';
const visibleResult = {visible};
if (visibleResult.visible !== false || visibleResult.reason !== 'zero_opacity')
    throw new Error('visibility check missed shadow host opacity: ' + JSON.stringify(visibleResult));

hostOpacity = '1';
hostInert = true;
const enabledResult = {enabled};
if (enabledResult.disabled !== true || enabledResult.reason !== 'inert')
    throw new Error('enabled check missed inert shadow host: ' + JSON.stringify(enabledResult));
"#
        );
        fs::write(&script_path, runtime).unwrap();
        let output = Command::new("node").arg(&script_path).output().unwrap();

        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn escape_css_rule_preserves_valid_selectors_and_neutralizes_rule_breakouts() {
        for valid in [
            "div > span.highlight",
            r#"[data-name="it's a card"]"#,
            r#"[data-name='hello "world"']"#,
            r#"[data-expr="a{b}c;d"]"#,
            r".icon\2603",
            ".café",
        ] {
            assert_eq!(escape_css_rule(valid), valid);
        }

        assert_eq!(
            escape_css_rule(".banner {} body { display: none }"),
            r".banner \{\} body \{ display: none \}"
        );
        assert_eq!(
            escape_css_rule("@import url('x'); .target"),
            r"\@import url('x')\; .target"
        );
        assert_eq!(
            escape_css_rule(".target /* unclosed comment"),
            r".target /\* unclosed comment"
        );
        assert_eq!(
            escape_css_rule(r#".target[title="unclosed"#),
            r#".target[title="unclosed""#
        );
        assert_eq!(escape_css_rule(r".target\"), r".target\\");
        assert_eq!(escape_css_rule("div\r\n\t>\0.child"), "div   >.child");
    }
}

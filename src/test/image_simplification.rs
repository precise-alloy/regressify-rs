//! Opt-in in-page image simplification for screenshots.
//!
//! With `imageSimplification: pixelate`, qualifying images in the page's main
//! document are flagged right before each screenshot, so a constructed
//! stylesheet applies an SVG block-averaging filter to them; the flags are
//! removed after the screenshot. With `none` no page work happens at all. The
//! browser logic lives in `image_simplification.js`; this module serializes
//! typed options, runs operations with bounded timeouts, and records count-only
//! diagnostics (never selectors, URLs, or paths).
//!
//! Each capture also records its effective setting in its sidecar link (see
//! `capture::CaptureLogLink`), which `ref` and `approve` promote with the image.
//! Comparisons warn, without changing results, when the reference and test
//! settings differ.

use std::{sync::LazyLock, time::Duration};

use anyhow::{Context, Result, anyhow};
use chromiumoxide::Page;
use serde::{Deserialize, Serialize, de::DeserializeOwned};

use crate::{
    config::{self, ResolvedConfig},
    diagnostics::{
        record::{DiagnosticEvent, Field, FieldValue, SafeValue, Severity, Stage},
        runtime as diagnostics,
    },
    types::ImageSimplification,
};

const LIBRARY: &str = include_str!("image_simplification.js");
const MARK_ATTRIBUTE: &str = "data-regressify-simplify";
const PIXELATE: &str = "pixelate";
const SHARP: &str = "sharp";
const SETUP_TIMEOUT: Duration = Duration::from_secs(5);
// Marking walks the composed tree once; very large documents need more than
// the 5-second budget of the other capture-preparation scripts.
const GUARD_TIMEOUT: Duration = Duration::from_secs(30);

/// Process-unique suffix so the filter host and filter IDs cannot collide with
/// page content. Reference and test runs differ only in IDs, never in pixels.
static RUN_TOKEN: LazyLock<String> = LazyLock::new(|| uuid::Uuid::new_v4().simple().to_string());

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Operation {
    Ensure,
    Mark,
    Refresh,
    Clear,
}

impl Operation {
    fn name(self) -> &'static str {
        match self {
            Self::Ensure => "ensure",
            Self::Mark => "mark",
            Self::Refresh => "refresh",
            Self::Clear => "clear",
        }
    }

    fn timeout(self) -> Duration {
        match self {
            Self::Ensure | Self::Clear => SETUP_TIMEOUT,
            Self::Mark | Self::Refresh => GUARD_TIMEOUT,
        }
    }
}

/// Image counts from one marking pass. Each rendered image is counted once, in
/// the first reason that keeps it sharp; hidden images are not counted.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct MarkCounts {
    pub simplified: u64,
    pub small: u64,
    pub excluded: u64,
    pub filtered: u64,
    pub transformed: u64,
    pub overlapped: u64,
    pub invalid_selectors: u64,
}

impl MarkCounts {
    fn fields(&self) -> Vec<(Field, FieldValue)> {
        vec![
            (Field::Condition, FieldValue::Kind(SafeValue::Images)),
            (Field::SimplifiedImages, FieldValue::Number(self.simplified)),
            (Field::SmallImages, FieldValue::Number(self.small)),
            (Field::ExcludedImages, FieldValue::Number(self.excluded)),
            (Field::FilteredImages, FieldValue::Number(self.filtered)),
            (
                Field::TransformedImages,
                FieldValue::Number(self.transformed),
            ),
            (Field::OverlappedImages, FieldValue::Number(self.overlapped)),
            (
                Field::InvalidSelectors,
                FieldValue::Number(self.invalid_selectors),
            ),
        ]
    }
}

/// Paint-order guard result after a selector capture scrolled the page.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct RefreshCounts {
    pub simplified: u64,
    pub overlapped: u64,
    pub filtered: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Ensured {
    #[allow(dead_code)]
    ensured: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Cleared {
    #[allow(dead_code)]
    cleared: u64,
}

/// Resolved `pixelate` settings compiled into page-script options.
#[derive(Clone, Debug)]
pub(crate) struct ImageSimplifier {
    options: String,
}

impl ImageSimplifier {
    /// `None` when simplification is off, so callers skip every page operation.
    pub(crate) fn from_config(config: &ResolvedConfig) -> Result<Option<Self>> {
        if config.image_simplification == ImageSimplification::None {
            return Ok(None);
        }
        Self::new(
            config.image_simplification_block_size,
            config.image_simplification_min_size,
            &config.image_simplification_exclude_selectors,
        )
        .map(Some)
    }

    fn new(block_size: u32, min_size: u32, exclude_selectors: &[String]) -> Result<Self> {
        // Preflight rejects these already; re-check because an invalid block
        // size would build a filter that Chromium rejects or misaligns.
        config::validate_image_simplification(
            "Resolved",
            Some(block_size),
            Some(min_size),
            Some(exclude_selectors),
        )?;
        let token = RUN_TOKEN.as_str();
        let options = serde_json::json!({
            "attribute": MARK_ATTRIBUTE,
            "pixelate": PIXELATE,
            "sharp": SHARP,
            "hostId": format!("regressify-simplify-host-{token}"),
            "filterId": format!("regressify-simplify-filter-{token}"),
            "blockSize": block_size,
            "minSize": min_size,
            "excludeSelectors": exclude_selectors,
        });
        Ok(Self {
            options: json_literal(&options)?,
        })
    }

    fn script(&self, operation: Operation) -> String {
        format!(
            "(() => {{ 'use strict'; const options = {}; const operation = {};\n{LIBRARY}\n}})()",
            self.options,
            serde_json::Value::from(operation.name())
        )
    }

    async fn run<T: DeserializeOwned>(&self, page: &Page, operation: Operation) -> Result<T> {
        let timeout = operation.timeout();
        let result = tokio::time::timeout(
            timeout,
            super::runtime::evaluate_main_document(page, self.script(operation)),
        )
        .await
        .map_err(|_| {
            anyhow!(
                "Timed out after {}ms during image simplification ({})",
                timeout.as_millis(),
                operation.name()
            )
        })?
        .with_context(|| format!("Image simplification ({}) failed", operation.name()))?;
        result.into_value().with_context(|| {
            format!(
                "Image simplification ({}) returned invalid data",
                operation.name()
            )
        })
    }

    /// Install the filter host and stylesheet in the current document.
    pub(crate) async fn ensure(&self, page: &Page) -> Result<()> {
        let _: Ensured = self.run(page, Operation::Ensure).await?;
        Ok(())
    }

    /// Flag qualifying images immediately before a screenshot.
    pub(crate) async fn prepare(&self, page: &Page) -> Result<MarkCounts> {
        diagnostics::stage(
            Stage::ScreenshotSettling,
            [(Field::Condition, FieldValue::Kind(SafeValue::Images))],
            async {
                let counts: MarkCounts = self.run(page, Operation::Mark).await?;
                diagnostics::event(
                    Stage::ScreenshotSettling,
                    DiagnosticEvent::Decision,
                    if counts.invalid_selectors > 0 {
                        Severity::Warn
                    } else {
                        Severity::Debug
                    },
                    counts.fields(),
                );
                Ok(counts)
            },
        )
        .await
    }

    /// Re-run the paint-order guard for flagged images at the current scroll
    /// position. Selector captures scroll after preparation, which can move
    /// fixed or sticky content over different images.
    pub(crate) async fn refresh(&self, page: &Page) -> Result<RefreshCounts> {
        let counts: RefreshCounts = self.run(page, Operation::Refresh).await?;
        diagnostics::event(
            Stage::CaptureArtifacts,
            DiagnosticEvent::Decision,
            Severity::Debug,
            [
                (Field::Condition, FieldValue::Kind(SafeValue::Images)),
                (
                    Field::SimplifiedImages,
                    FieldValue::Number(counts.simplified),
                ),
                (
                    Field::OverlappedImages,
                    FieldValue::Number(counts.overlapped),
                ),
                (Field::FilteredImages, FieldValue::Number(counts.filtered)),
            ],
        );
        Ok(counts)
    }

    /// Remove every flag so later actions, navigation, and settling see the
    /// unmodified page. The host and stylesheet stay; they match nothing.
    pub(crate) async fn restore(&self, page: &Page) -> Result<()> {
        diagnostics::stage(
            Stage::CaptureArtifacts,
            [(Field::Condition, FieldValue::Kind(SafeValue::Images))],
            async {
                let _: Cleared = self.run(page, Operation::Clear).await?;
                Ok(())
            },
        )
        .await
    }
}

/// Effective image simplification of one capture, as recorded in its sidecar
/// link. Only values that change pixels are recorded: `none` records the mode
/// alone, and exclusions are recorded as a digest, never as selector text.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "lowercase")]
pub enum ImageSimplificationSetting {
    None,
    #[serde(rename_all = "camelCase")]
    Pixelate {
        block_size: u32,
        min_size: u32,
        /// See [`exclude_selectors_digest`].
        exclude_selectors_digest: String,
    },
}

impl ImageSimplificationSetting {
    pub(crate) fn from_config(config: &ResolvedConfig) -> Self {
        match config.image_simplification {
            ImageSimplification::None => Self::None,
            ImageSimplification::Pixelate => Self::Pixelate {
                block_size: config.image_simplification_block_size,
                min_size: config.image_simplification_min_size,
                exclude_selectors_digest: exclude_selectors_digest(
                    &config.image_simplification_exclude_selectors,
                ),
            },
        }
    }

    fn describe(&self) -> String {
        match self {
            Self::None => "none".into(),
            Self::Pixelate {
                block_size,
                min_size,
                exclude_selectors_digest,
            } => {
                let exclusions = if *exclude_selectors_digest == *EMPTY_EXCLUSIONS_DIGEST {
                    "no exclusions".to_string()
                } else {
                    // Links can come from a shared repository: print only a plain
                    // hex prefix so a hand-edited digest cannot inject terminal text.
                    match exclude_selectors_digest
                        .get(..12)
                        .filter(|prefix| prefix.bytes().all(|byte| byte.is_ascii_hexdigit()))
                    {
                        Some(prefix) => format!("exclusion list {prefix}"),
                        None => "unreadable exclusion list".to_string(),
                    }
                };
                format!("pixelate, {block_size} px blocks, {min_size} px minimum, {exclusions}")
            }
        }
    }
}

static EMPTY_EXCLUSIONS_DIGEST: LazyLock<String> = LazyLock::new(|| exclude_selectors_digest(&[]));

/// Stable digest of an exclusion list: lowercase hex SHA-256 of the sorted,
/// de-duplicated selectors as a compact JSON array. Exclusion is a set test, so
/// order and duplicates cannot change pixels and must not change the digest.
/// Recorded digests are compared across releases; keep this canonical form.
pub(crate) fn exclude_selectors_digest(selectors: &[String]) -> String {
    use sha2::{Digest, Sha256};
    let mut canonical: Vec<&str> = selectors.iter().map(String::as_str).collect();
    canonical.sort_unstable();
    canonical.dedup();
    let json = serde_json::Value::from(canonical).to_string();
    format!("{:x}", Sha256::digest(json.as_bytes()))
}

/// A setting as stored in a capture link. Values this release cannot parse,
/// such as a mode added later, stay verbatim so promotion and lossless
/// optimization preserve them, and they never invalidate the link's log data.
/// Known modes ignore unknown fields, and promotion rewrites them without
/// those fields, so a field added to a known mode later must mean today's
/// pixels when it is absent.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum RecordedImageSimplification {
    Known(ImageSimplificationSetting),
    Unrecognized(serde_json::Value),
}

impl RecordedImageSimplification {
    /// Reports carry only a marker for unrecognized values, so arbitrary
    /// sidecar content is never copied into report.json. A known mode whose
    /// digest is not the canonical 64 lowercase hex characters was hand-edited
    /// (links can come from a shared repository) and is treated the same way.
    fn for_report(&self) -> Self {
        match self {
            Self::Known(ImageSimplificationSetting::Pixelate {
                exclude_selectors_digest,
                ..
            }) if !(exclude_selectors_digest.len() == 64
                && exclude_selectors_digest
                    .bytes()
                    .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))) =>
            {
                Self::unrecognized_marker()
            }
            Self::Known(_) => self.clone(),
            Self::Unrecognized(_) => Self::unrecognized_marker(),
        }
    }

    fn unrecognized_marker() -> Self {
        Self::Unrecognized(serde_json::json!({ "mode": "unrecognized" }))
    }
}

/// Report warning for a compared pair whose reference and test captures
/// recorded different settings. Informational only: it never changes the
/// pair's status or the command's exit code.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageSimplificationWarning {
    #[serde(default)]
    pub message: String,
    /// `null` when the reference has no recorded setting.
    #[serde(default)]
    pub reference: Option<RecordedImageSimplification>,
    /// `null` when the test capture has no recorded setting.
    #[serde(default)]
    pub test: Option<RecordedImageSimplification>,
}

/// Compare the settings recorded with a reference and a test capture. A capture
/// without a recorded setting predates recording, or its link no longer matches
/// its image; releases before recording could not pixelate, so a missing record
/// counts as `none` and warns only against a pixelated or unrecognized setting.
pub(crate) fn setting_warning(
    reference: Option<&RecordedImageSimplification>,
    test: Option<&RecordedImageSimplification>,
) -> Option<ImageSimplificationWarning> {
    let none = RecordedImageSimplification::Known(ImageSimplificationSetting::None);
    if reference.unwrap_or(&none) == test.unwrap_or(&none) {
        return None;
    }
    let describe = |setting: Option<&RecordedImageSimplification>| match setting {
        None => "no recorded setting".to_string(),
        Some(RecordedImageSimplification::Known(setting)) => setting.describe(),
        Some(RecordedImageSimplification::Unrecognized(_)) => {
            "a setting this Regressify version does not recognize".to_string()
        }
    };
    Some(ImageSimplificationWarning {
        message: format!(
            "Reference and test captures used different image simplification settings \
             (reference: {}; test: {}). Run ref again, or approve these test captures, so \
             both sides use the same settings.",
            describe(reference),
            describe(test)
        ),
        reference: reference.map(RecordedImageSimplification::for_report),
        test: test.map(RecordedImageSimplification::for_report),
    })
}

/// Serialize a value as a JavaScript literal. JSON is valid JavaScript; line
/// separators are escaped so the literal also survives older tooling.
fn json_literal(value: &serde_json::Value) -> Result<String> {
    Ok(serde_json::to_string(value)?
        .replace('\u{2028}', "\\u2028")
        .replace('\u{2029}', "\\u2029"))
}

/// Remove image flags after a screenshot step, even when the step failed. The
/// step's own error wins; a restore failure fails an otherwise successful step
/// because later steps would run against a modified page.
pub(crate) async fn restore_after<T>(
    simplifier: Option<&ImageSimplifier>,
    page: &Page,
    step: Result<T>,
) -> Result<T> {
    let Some(simplifier) = simplifier else {
        return step;
    };
    let restored = simplifier.restore(page).await;
    match (step, restored) {
        (Ok(value), Ok(())) => Ok(value),
        (Ok(_), Err(error)) | (Err(error), _) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use std::process::Command;

    use super::*;

    fn pixelate(block_size: u32, min_size: u32, selectors: &[&str]) -> ResolvedConfig {
        ResolvedConfig {
            image_simplification: ImageSimplification::Pixelate,
            image_simplification_block_size: block_size,
            image_simplification_min_size: min_size,
            image_simplification_exclude_selectors: selectors
                .iter()
                .map(|selector| selector.to_string())
                .collect(),
            ..Default::default()
        }
    }

    fn options_of(script: &str) -> serde_json::Value {
        let start = script.find("const options = ").unwrap() + "const options = ".len();
        let end = script.find("; const operation = ").unwrap();
        serde_json::from_str(&script[start..end]).unwrap()
    }

    const OPERATIONS: [Operation; 4] = [
        Operation::Ensure,
        Operation::Mark,
        Operation::Refresh,
        Operation::Clear,
    ];

    #[test]
    fn image_simplification_none_builds_no_page_work() {
        assert!(
            ImageSimplifier::from_config(&ResolvedConfig::default())
                .unwrap()
                .is_none()
        );
        // Other keys are inert while the mode is none.
        let configured = ResolvedConfig {
            image_simplification_block_size: 16,
            image_simplification_min_size: 1,
            image_simplification_exclude_selectors: vec!["#hero".into()],
            ..Default::default()
        };
        assert!(ImageSimplifier::from_config(&configured).unwrap().is_none());
    }

    #[test]
    fn image_simplification_options_are_typed_literals_and_run_unique_ids() {
        let simplifier = ImageSimplifier::from_config(&pixelate(8, 96, &[".keep"]))
            .unwrap()
            .unwrap();
        for operation in OPERATIONS {
            let script = simplifier.script(operation);
            assert!(script.starts_with("(() => { 'use strict'; const options = {"));
            assert!(script.ends_with("\n})()"));
            assert!(script.contains(&format!("const operation = \"{}\";", operation.name())));
            let options = options_of(&script);
            assert_eq!(options["blockSize"], 8);
            assert_eq!(options["minSize"], 96);
            assert_eq!(options["excludeSelectors"], serde_json::json!([".keep"]));
            assert_eq!(options["attribute"], MARK_ATTRIBUTE);
            assert_eq!(options["pixelate"], PIXELATE);
            assert_eq!(options["sharp"], SHARP);
            // IDs reach CSS url() and getElementById; keep them to a safe alphabet.
            for key in ["hostId", "filterId"] {
                let id = options[key].as_str().unwrap();
                assert!(id.ends_with(RUN_TOKEN.as_str()), "{id}");
                assert!(
                    id.chars().all(|character| character.is_ascii_lowercase()
                        || character.is_ascii_digit()
                        || character == '-'),
                    "{id}"
                );
            }
            assert_ne!(options["hostId"], options["filterId"]);
        }
    }

    #[test]
    fn image_simplification_selectors_are_json_escaped_without_injection() {
        let hostile = [
            r#"img[alt="a\"b"]"#,
            r"img[alt='\\']",
            "'); document.body.remove(); ('",
            "\"]; window.injected = true; [\"",
            "</script><script>window.injected = true</script>",
            "img[data-x=\"\u{2028}\u{2029}\"]",
            "img[data-tab=\"\t\"]\n.newline",
            "}) ; throw new Error('escaped'); ({",
            "img[data-unicode=\"\u{1F600}\"]",
            "img[data-nul=\"\u{0}\"]",
        ];
        let simplifier = ImageSimplifier::from_config(&pixelate(4, 64, &hostile))
            .unwrap()
            .unwrap();
        let script = simplifier.script(Operation::Mark);
        assert_eq!(
            options_of(&script)["excludeSelectors"],
            serde_json::json!(hostile)
        );
        assert!(!script.contains('\u{2028}') && !script.contains('\u{2029}'));
        // Everything before the library is the fixed wrapper plus one JSON
        // literal, so no selector can contribute code.
        let options = json_literal(&options_of(&script)).unwrap();
        assert_eq!(
            &script[..script.find(LIBRARY).unwrap()],
            format!(
                "(() => {{ 'use strict'; const options = {options}; const operation = \"mark\";\n"
            )
        );
    }

    #[test]
    fn image_simplification_rejects_invalid_resolved_settings() {
        for (block_size, min_size, selectors) in [
            (5, 64, vec![]),
            (18, 64, vec![]),
            (0, 64, vec![]),
            (4, 0, vec![]),
            (4, 64, vec![" "]),
        ] {
            assert!(
                ImageSimplifier::from_config(&pixelate(block_size, min_size, &selectors)).is_err(),
                "{block_size} {min_size} {selectors:?}"
            );
        }
        for block_size in [2, 4, 6, 8, 10, 12, 14, 16] {
            assert!(
                ImageSimplifier::from_config(&pixelate(block_size, 1, &[]))
                    .unwrap()
                    .is_some()
            );
        }
    }

    #[test]
    fn image_simplification_counts_map_to_count_only_fields() {
        let counts = MarkCounts {
            simplified: 1,
            small: 2,
            excluded: 3,
            filtered: 4,
            transformed: 5,
            overlapped: 6,
            invalid_selectors: 7,
        };
        let fields = counts.fields();
        assert_eq!(fields.len(), 8);
        assert!(fields.iter().all(|(_, value)| matches!(
            value,
            FieldValue::Number(_) | FieldValue::Kind(SafeValue::Images)
        )));
        let mut record = crate::diagnostics::record::DiagnosticRecordV1::new(
            crate::diagnostics::record::OperationId::new(),
            crate::diagnostics::record::OperationKind::Capture,
            Stage::ScreenshotSettling,
            DiagnosticEvent::Decision,
        );
        record.fields.extend(fields);
        let json = serde_json::to_value(&record).unwrap();
        for (key, value) in [
            ("simplifiedImages", 1),
            ("smallImages", 2),
            ("excludedImages", 3),
            ("filteredImages", 4),
            ("transformedImages", 5),
            ("overlappedImages", 6),
            ("invalidSelectors", 7),
        ] {
            assert_eq!(json["fields"][key]["type"], "number", "{key}");
            assert_eq!(json["fields"][key]["value"], value, "{key}");
        }
        assert_eq!(json["fields"]["condition"]["value"], "images");
        assert_eq!(
            serde_json::from_value::<MarkCounts>(serde_json::json!({
                "simplified": 1, "small": 2, "excluded": 3, "filtered": 4,
                "transformed": 5, "overlapped": 6, "invalidSelectors": 7
            }))
            .unwrap(),
            counts
        );
        // Page results must not smuggle extra data into diagnostics.
        assert!(
            serde_json::from_value::<MarkCounts>(serde_json::json!({"selector": "#hero"})).is_err()
        );
    }

    fn selectors(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| value.to_string()).collect()
    }

    #[test]
    fn image_simplification_setting_records_only_pixel_affecting_values() {
        use serde_json::json;
        // Other keys cannot change pixels while the mode is none.
        let off = ResolvedConfig {
            image_simplification_block_size: 16,
            image_simplification_min_size: 1,
            image_simplification_exclude_selectors: selectors(&["#hero"]),
            ..Default::default()
        };
        for config in [ResolvedConfig::default(), off] {
            let setting = ImageSimplificationSetting::from_config(&config);
            assert_eq!(setting, ImageSimplificationSetting::None);
            assert_eq!(
                serde_json::to_value(&setting).unwrap(),
                json!({"mode": "none"})
            );
        }
        let setting = ImageSimplificationSetting::from_config(&pixelate(
            8,
            96,
            &[".keep", "img[data-secret=\"marker\"]"],
        ));
        let value = serde_json::to_value(&setting).unwrap();
        assert_eq!(
            value,
            json!({
                "mode": "pixelate",
                "blockSize": 8,
                "minSize": 96,
                "excludeSelectorsDigest": exclude_selectors_digest(&selectors(&[
                    ".keep",
                    "img[data-secret=\"marker\"]"
                ])),
            })
        );
        // Selector text never reaches the sidecar or reports.
        assert!(!value.to_string().contains("keep") && !value.to_string().contains("marker"));
        assert_eq!(
            serde_json::from_value::<ImageSimplificationSetting>(value).unwrap(),
            setting
        );
    }

    #[test]
    fn image_simplification_exclusion_digest_is_canonical_and_stable() {
        let digest = |values: &[&str]| exclude_selectors_digest(&selectors(values));
        // Pinned: digests recorded by earlier captures must keep matching.
        assert_eq!(
            digest(&[]),
            "4f53cda18c2baa0c0354bb5f9a3ecbe5ed12ab4d8e11ba873c2f11161202b945"
        );
        assert_eq!(
            digest(&[".keep", "#hero img"]),
            "a0e40105a1d97d079aa0b78e78974e8eb9b4d83722999006d15d66153b579e39"
        );
        // Exclusion is a set test: order and duplicates cannot change pixels.
        assert_eq!(
            digest(&["#hero img", ".keep", ".keep", "#hero img"]),
            digest(&[".keep", "#hero img"])
        );
        // Any other textual difference counts, and entries cannot run together.
        for (first, second) in [
            (vec![".keep"], vec![".keep "]),
            (vec![".keep"], vec![".Keep"]),
            (vec!["a", "b"], vec!["a,b"]),
            (vec!["a", "b"], vec!["a\",\"b"]),
            (vec![".keep"], vec![]),
        ] {
            assert_ne!(digest(&first), digest(&second), "{first:?} {second:?}");
        }
        for values in [
            vec!["'); throw 1; ('", "img[alt=\"\u{2028}\"]"],
            vec!["\u{0}", "\u{1F600}"],
        ] {
            let value = digest(&values);
            assert_eq!(value.len(), 64);
            assert!(
                value
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            );
        }
    }

    #[test]
    fn image_simplification_recorded_settings_parse_leniently() {
        use RecordedImageSimplification::{Known, Unrecognized};
        use serde_json::json;
        let digest = exclude_selectors_digest(&[]);
        let pixelated = Known(ImageSimplificationSetting::Pixelate {
            block_size: 4,
            min_size: 64,
            exclude_selectors_digest: digest.clone(),
        });
        for (value, expected) in [
            (json!(null), None),
            (
                json!({"mode": "none"}),
                Some(Known(ImageSimplificationSetting::None)),
            ),
            (
                json!({"mode": "none", "blockSize": 4}),
                Some(Known(ImageSimplificationSetting::None)),
            ),
            (
                json!({"mode": "pixelate", "blockSize": 4, "minSize": 64,
                       "excludeSelectorsDigest": digest, "addedLater": true}),
                Some(pixelated.clone()),
            ),
        ] {
            assert_eq!(
                serde_json::from_value::<Option<RecordedImageSimplification>>(value.clone())
                    .unwrap(),
                expected,
                "{value}"
            );
        }
        // A newer mode or a malformed value is kept verbatim, never rejected.
        for value in [
            json!({"mode": "blur", "radius": 3}),
            json!({"mode": "pixelate", "blockSize": 4}),
            json!({"mode": "pixelate", "blockSize": "4", "minSize": 64,
                   "excludeSelectorsDigest": digest}),
            json!({"mode": "Pixelate", "blockSize": 4, "minSize": 64,
                   "excludeSelectorsDigest": digest}),
            json!("pixelate"),
            json!(4),
        ] {
            let parsed: RecordedImageSimplification =
                serde_json::from_value(value.clone()).unwrap();
            assert_eq!(parsed, Unrecognized(value.clone()));
            assert_eq!(serde_json::to_value(&parsed).unwrap(), value);
        }
        assert_eq!(
            serde_json::to_value(&pixelated).unwrap(),
            json!({"mode": "pixelate", "blockSize": 4, "minSize": 64,
                   "excludeSelectorsDigest": digest})
        );
    }

    #[test]
    fn image_simplification_setting_warnings_cover_every_recorded_state() {
        use RecordedImageSimplification::{Known, Unrecognized};
        use serde_json::json;
        let none = Known(ImageSimplificationSetting::None);
        let setting = |block_size, min_size, values: &[&str]| {
            Known(ImageSimplificationSetting::from_config(&pixelate(
                block_size, min_size, values,
            )))
        };
        let base = setting(4, 64, &[]);
        let larger_blocks = setting(8, 64, &[]);
        let smaller_minimum = setting(4, 32, &[]);
        let excluded = setting(4, 64, &["img[data-secret=\"marker\"]"]);
        let future = Unrecognized(json!({"mode": "blur", "radius": 3}));
        let other_future = Unrecognized(json!({"mode": "blur", "radius": 4}));
        for (reference, test, warns) in [
            (None, None, false),
            (None, Some(&none), false),
            (Some(&none), Some(&none), false),
            (Some(&base), Some(&base), false),
            (Some(&future), Some(&future), false),
            // A reference without a recorded setting cannot have been pixelated
            // by an earlier release, but a pixelated test capture still differs.
            (None, Some(&base), true),
            (Some(&none), Some(&base), true),
            (Some(&base), Some(&larger_blocks), true),
            (Some(&base), Some(&smaller_minimum), true),
            (Some(&base), Some(&excluded), true),
            (None, Some(&future), true),
            (Some(&none), Some(&future), true),
            (Some(&future), Some(&other_future), true),
        ] {
            assert_eq!(
                setting_warning(reference, test).is_some(),
                warns,
                "{reference:?} {test:?}"
            );
            assert_eq!(
                setting_warning(test, reference).is_some(),
                warns,
                "{test:?} {reference:?}"
            );
        }

        let warning = setting_warning(None, Some(&base)).unwrap();
        assert_eq!(
            warning.message,
            "Reference and test captures used different image simplification settings \
             (reference: no recorded setting; test: pixelate, 4 px blocks, 64 px minimum, no \
             exclusions). Run ref again, or approve these test captures, so both sides use the \
             same settings."
        );
        let value = serde_json::to_value(&warning).unwrap();
        assert_eq!(
            value,
            json!({
                "message": warning.message,
                "reference": null,
                "test": {"mode": "pixelate", "blockSize": 4, "minSize": 64,
                         "excludeSelectorsDigest": exclude_selectors_digest(&[])},
            })
        );
        // Approval reads saved reports back; older or partial shapes stay readable.
        assert_eq!(
            serde_json::from_value::<ImageSimplificationWarning>(value).unwrap(),
            warning
        );
        assert_eq!(
            serde_json::from_value::<ImageSimplificationWarning>(json!({})).unwrap(),
            ImageSimplificationWarning {
                message: String::new(),
                reference: None,
                test: None,
            }
        );

        // Exclusions appear as a short digest prefix, never as selector text.
        let warning = setting_warning(Some(&larger_blocks), Some(&excluded)).unwrap();
        let digest = exclude_selectors_digest(&selectors(&["img[data-secret=\"marker\"]"]));
        assert!(
            warning
                .message
                .contains("reference: pixelate, 8 px blocks, 64 px minimum, no exclusions;"),
            "{}",
            warning.message
        );
        assert!(
            warning
                .message
                .contains(&format!("exclusion list {}", &digest[..12])),
            "{}",
            warning.message
        );
        assert!(!warning.message.contains("marker"));

        // A well-formed digest is copied into the report on either side.
        let valid = exclude_selectors_digest(&selectors(&["img[data-secret=\"marker\"]"]));
        for (reference, test) in [(&base, &excluded), (&excluded, &base)] {
            let warning = setting_warning(Some(reference), Some(test)).unwrap();
            assert_eq!(warning.reference.as_ref(), Some(reference));
            assert_eq!(warning.test.as_ref(), Some(test));
        }
        assert!(
            serde_json::to_string(&setting_warning(Some(&base), Some(&excluded)))
                .unwrap()
                .contains(&valid)
        );

        // Hand-edited or future sidecar values are neither printed nor copied.
        let marker = json!({"mode": "unrecognized"});
        // `true` when the digest prefix itself is unprintable.
        for (hostile, unreadable) in [
            ("\u{1b}[2J0123456789abcdef".to_string(), true),
            ("éééééééééééééé".to_string(), true),
            ("abc".to_string(), true),
            // Exactly 64 characters, but not lowercase hex.
            (valid.to_uppercase(), false),
            (format!("{}\u{1b}", &valid[..63]), false),
            // Lowercase hex, one character short or long.
            (valid[..63].to_string(), false),
            (format!("{valid}0"), false),
        ] {
            let hostile = hostile.as_str();
            let edited = Known(ImageSimplificationSetting::Pixelate {
                block_size: 4,
                min_size: 64,
                exclude_selectors_digest: hostile.into(),
            });
            let warning = setting_warning(Some(&future), Some(&edited)).unwrap();
            assert!(
                warning
                    .message
                    .contains("reference: a setting this Regressify version does not recognize;"),
                "{}",
                warning.message
            );
            assert_eq!(
                warning.message.contains("unreadable exclusion list"),
                unreadable,
                "{}",
                warning.message
            );
            assert!(!warning.message.contains(hostile));
            assert_eq!(serde_json::to_value(&warning.reference).unwrap(), marker);
            assert_eq!(
                serde_json::to_value(&warning.test).unwrap(),
                marker,
                "{hostile:?} was copied into the report"
            );
            // The same value on the reference side is replaced as well.
            let warning = setting_warning(Some(&edited), Some(&base)).unwrap();
            assert_eq!(serde_json::to_value(&warning.reference).unwrap(), marker);
            assert_eq!(warning.test.as_ref(), Some(&base));
        }
    }

    #[test]
    fn image_simplification_scripts_are_valid_javascript_when_node_is_available() {
        if Command::new("node").arg("--version").output().is_err() {
            return;
        }
        let temp = tempfile::tempdir().unwrap();
        let simplifier = ImageSimplifier::from_config(&pixelate(
            16,
            1,
            &["'); throw 1; ('", "img[alt=\"\u{2028}\"]"],
        ))
        .unwrap()
        .unwrap();
        for operation in OPERATIONS {
            let path = temp.path().join(format!("{}.js", operation.name()));
            std::fs::write(&path, simplifier.script(operation)).unwrap();
            let output = Command::new("node")
                .arg("--check")
                .arg(&path)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}: {}",
                operation.name(),
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }

    #[test]
    fn image_simplification_clear_removes_only_its_flag_when_node_is_available() {
        if Command::new("node").arg("--version").output().is_err() {
            return;
        }
        let temp = tempfile::tempdir().unwrap();
        let simplifier = ImageSimplifier::from_config(&pixelate(4, 64, &[]))
            .unwrap()
            .unwrap();
        let harness = format!(
            r#"const removed = [];
global.document = {{
    querySelectorAll(selector) {{
        if (selector !== '[{MARK_ATTRIBUTE}]') throw new Error(selector);
        return [1, 2, 3].map(id => ({{ removeAttribute(name) {{ removed.push([id, name]); }} }}));
    }},
}};
const result = {};
if (JSON.stringify(result) !== JSON.stringify({{ cleared: 3 }}) || removed.length !== 3 ||
    removed.some(([, name]) => name !== '{MARK_ATTRIBUTE}')) {{
    throw new Error(JSON.stringify({{ result, removed }}));
}}
"#,
            simplifier.script(Operation::Clear)
        );
        let path = temp.path().join("clear.js");
        std::fs::write(&path, harness).unwrap();
        let output = Command::new("node").arg(&path).output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

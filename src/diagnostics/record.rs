use std::{collections::BTreeMap, fmt};

use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const SCHEMA_VERSION: u16 = 1;
const MAX_JSON_INTEGER: u64 = (1 << 53) - 1;

macro_rules! uuid_identity {
    ($name:ident) => {
        #[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(try_from = "String", into = "String")]
        pub struct $name(String);

        impl $name {
            pub fn new() -> Self {
                Self(uuid::Uuid::new_v4().to_string())
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl TryFrom<String> for $name {
            type Error = &'static str;

            fn try_from(value: String) -> Result<Self, Self::Error> {
                let parsed = uuid::Uuid::parse_str(&value).map_err(|_| "Invalid diagnostic ID")?;
                if parsed.is_nil() || parsed.to_string() != value {
                    return Err("Diagnostic IDs must be non-nil canonical UUIDs");
                }
                Ok(Self(value))
            }
        }

        impl From<$name> for String {
            fn from(value: $name) -> Self {
                value.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(&self.0)
            }
        }
    };
}

uuid_identity!(OperationId);
uuid_identity!(ExecutionId);
uuid_identity!(SpanId);

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct CaptureRunId(String);

impl CaptureRunId {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for CaptureRunId {
    type Error = String;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        crate::cli::args::run_id_parser(&value).map(Self)
    }
}

impl From<CaptureRunId> for String {
    fn from(value: CaptureRunId) -> Self {
        value.0
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct SafeIdentity(String);

impl SafeIdentity {
    pub fn source(value: impl AsRef<str>) -> Self {
        Self(super::redaction::sanitize_source(value.as_ref()))
    }

    pub fn fingerprint(value: impl AsRef<[u8]>) -> Self {
        Self(format!("sha256:{:x}", Sha256::digest(value.as_ref())))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for SafeIdentity {
    type Error = &'static str;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        if value != super::redaction::sanitize_source(&value) {
            return Err("Expected bounded, sanitized diagnostic source metadata");
        }
        Ok(Self(value))
    }
}

impl From<SafeIdentity> for String {
    fn from(value: SafeIdentity) -> Self {
        value.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum OperationKind {
    Capture,
    Deployment,
    Command,
    Application,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CaptureRole {
    Reference,
    Test,
}

#[derive(
    Clone,
    Copy,
    Debug,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Serialize,
    Deserialize,
    clap::ValueEnum,
    schemars::JsonSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
}

impl std::str::FromStr for Severity {
    type Err = &'static str;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "trace" => Ok(Self::Trace),
            "debug" => Ok(Self::Debug),
            "info" => Ok(Self::Info),
            "warn" => Ok(Self::Warn),
            "error" => Ok(Self::Error),
            _ => Err("Log level must be trace, debug, info, warn, or error"),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Stage {
    QueueSetup,
    Navigation,
    StateHooks,
    ActionsFrames,
    ScreenshotSettling,
    CaptureArtifacts,
    Finish,
    Preflight,
    SnapshotStaging,
    PackagingOptimization,
    ApiUploads,
    RetryPolling,
    CompletionCleanup,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Outcome {
    Success,
    Failure,
    Cancelled,
    CancelledRemoteUnknown,
    Mismatch,
    Unknown,
}

impl Outcome {
    pub fn severity(self) -> Severity {
        match self {
            Self::Success => Severity::Info,
            Self::Failure => Severity::Error,
            Self::Cancelled | Self::CancelledRemoteUnknown | Self::Mismatch | Self::Unknown => {
                Severity::Warn
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DiagnosticEvent {
    Queued,
    Started,
    StageStarted,
    StageFinished,
    Decision,
    Waiting,
    Retry,
    Heartbeat,
    Captured,
    Compared,
    Published,
    Warning,
    Cancelled,
    Finished,
}

impl DiagnosticEvent {
    pub fn message(self) -> &'static str {
        match self {
            Self::Queued => "Execution queued",
            Self::Started => "Execution started",
            Self::StageStarted => "Stage started",
            Self::StageFinished => "Stage finished",
            Self::Decision => "Execution decision",
            Self::Waiting => "Waiting for readiness",
            Self::Retry => "Retry scheduled",
            Self::Heartbeat => "Operation is still working",
            Self::Captured => "Capture completed",
            Self::Compared => "Comparison completed",
            Self::Published => "Publication confirmed",
            Self::Warning => "Diagnostics are incomplete",
            Self::Cancelled => "Execution cancelled",
            Self::Finished => "Execution finished",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Field {
    PublicationUrl,
    Command,
    ActionIndex,
    ActionType,
    Target,
    Destination,
    Artifact,
    Capture,
    TimeoutMs,
    ElementTimeoutMs,
    ActionabilityTimeoutMs,
    PollIntervalMs,
    ScenarioTimeoutMs,
    RetryLimit,
    RetryAttempt,
    BackoffMs,
    QueueMs,
    ElapsedMs,
    CompletedAnimations,
    FrozenAnimations,
    Width,
    Height,
    X,
    Y,
    Bytes,
    BytesBefore,
    BytesAfter,
    BudgetBytes,
    FileCount,
    SkippedFiles,
    BatchCount,
    SelectedSuites,
    StatusCode,
    OsErrorCode,
    RequestId,
    Provider,
    Endpoint,
    Condition,
    Reason,
    State,
    CredentialPresent,
    Compression,
    DeadlineMs,
    RedirectCount,
    FrameDepth,
    TabCount,
    Infrastructure,
    CleanupFailed,
    // Image simplification counts; selectors and image sources are never recorded.
    SimplifiedImages,
    SmallImages,
    ExcludedImages,
    FilteredImages,
    TransformedImages,
    OverlappedImages,
    InvalidSelectors,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SafeValue {
    Click,
    Hover,
    Check,
    Uncheck,
    Focus,
    Input,
    Press,
    Select,
    Scroll,
    Navigate,
    Save,
    Hide,
    Remove,
    Wait,
    Persist,
    Restore,
    SwitchTab,
    Drag,
    DropFiles,
    Wheel,
    Dialog,
    Key,
    Tap,
    Swipe,
    LongPress,
    Blur,
    Pinch,
    SelectText,
    Paste,
    CloseTab,
    Hook,
    Before,
    Ready,
    Exists,
    Visible,
    Stable,
    Enabled,
    Hidden,
    Detached,
    Disabled,
    Content,
    Unobscured,
    Fonts,
    Images,
    LazyLoading,
    Animation,
    Caret,
    Cached,
    Download,
    Browser,
    Page,
    Handler,
    Azure,
    Netlify,
    Cloudflare,
    Manifest,
    Package,
    Upload,
    Poll,
    Publication,
    Preflight,
    Transient,
    RateLimited,
    Timeout,
    Pending,
    Complete,
    Failed,
    Cancelled,
    Init,
    Install,
    Mcp,
    Version,
    Test,
    Reference,
    Compare,
    Approve,
    Snapshot,
    Optimize,
    Clean,
    Empty,
    BelowMinimum,
    Unchanged,
    Cli,
    Desktop,
    Worker,
    Shell,
    Window,
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "value", rename_all = "camelCase")]
pub enum FieldValue {
    PublicUrl(PublicUrl),
    Number(u64),
    Coordinate(i32),
    Boolean(bool),
    Identity(SafeIdentity),
    Kind(SafeValue),
}

impl Field {
    fn accepts(self, value: &FieldValue) -> bool {
        match self {
            Self::PublicationUrl => matches!(value, FieldValue::PublicUrl(_)),
            Self::Target | Self::Destination | Self::Artifact | Self::Capture | Self::RequestId => {
                matches!(value, FieldValue::Identity(_))
            }
            Self::Command
            | Self::ActionType
            | Self::Provider
            | Self::Endpoint
            | Self::Condition
            | Self::Reason
            | Self::State => {
                matches!(value, FieldValue::Kind(_))
            }
            Self::CredentialPresent
            | Self::Compression
            | Self::Infrastructure
            | Self::CleanupFailed => {
                matches!(value, FieldValue::Boolean(_))
            }
            Self::X | Self::Y => matches!(value, FieldValue::Coordinate(_)),
            _ => matches!(value, FieldValue::Number(number) if *number <= MAX_JSON_INTEGER),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct PublicUrl(String);

impl PublicUrl {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for PublicUrl {
    type Error = &'static str;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        let url = reqwest::Url::parse(&value).map_err(|_| "Invalid public URL")?;
        if value.len() > 2048
            || url.scheme() != "https"
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
            || !matches!(url.path(), "" | "/")
            || value.chars().any(char::is_control)
        {
            return Err("Unsafe public URL");
        }
        Ok(Self(url.to_string()))
    }
}

impl From<PublicUrl> for String {
    fn from(value: PublicUrl) -> Self {
        value.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ErrorCategory {
    Timeout,
    NotFound,
    Permission,
    Navigation,
    Actionability,
    Cancelled,
    InvalidData,
    Storage,
    Provider,
    Unknown,
}

impl ErrorCategory {
    fn classify(message: &str) -> Self {
        let message = message.to_ascii_lowercase();
        // Failure summaries end with their category in parentheses. Comparison
        // records classify report messages again, so keep that category.
        if let Some(category) = [
            Self::Timeout,
            Self::NotFound,
            Self::Permission,
            Self::Navigation,
            Self::Actionability,
            Self::Cancelled,
            Self::InvalidData,
            Self::Storage,
        ]
        .into_iter()
        .find(|category| message.contains(&format!("({category:?})").to_ascii_lowercase()))
        {
            return category;
        }
        // Match `timeout` as a word so expected-field lists in parse errors, such
        // as `readyTimeout`, cannot turn a schema mistake into a timeout.
        let has_word = |word: &str| {
            message.match_indices(word).any(|(start, _)| {
                !message[..start]
                    .chars()
                    .next_back()
                    .is_some_and(|character| character.is_ascii_alphanumeric())
            })
        };
        if [
            "unknown field",
            "unknown variant",
            "missing field",
            "duplicate field",
            "invalid type",
            "did not match any variant",
            "unknown keyboard key",
        ]
        .iter()
        .any(|phrase| message.contains(phrase))
        {
            Self::InvalidData
        } else if message.contains("timed out")
            || has_word("timeout")
            || message.contains("navigation exceeded")
        {
            Self::Timeout
        } else if message.contains("cancel") {
            Self::Cancelled
        } else if message.contains("not found") || message.contains("no such file") {
            Self::NotFound
        } else if message.contains("permission") || message.contains("access is denied") {
            Self::Permission
        } else if message.contains("navigation") {
            Self::Navigation
        } else if [
            "actionability",
            "obscured",
            "not visible",
            "disabled",
            "not stable",
        ]
        .iter()
        .any(|word| message.contains(word))
        {
            Self::Actionability
        } else if ["invalid", "parse", "must be", "must have", "must not"]
            .iter()
            .any(|word| message.contains(word))
        {
            Self::InvalidData
        } else if message.contains("write") || message.contains("disk") || message.contains("flush")
        {
            Self::Storage
        } else {
            Self::Unknown
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SanitizedError {
    pub category: ErrorCategory,
    pub causes: Vec<ErrorCategory>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    local_detail: Option<String>,
}

impl SanitizedError {
    pub fn summary(&self) -> String {
        self.local_detail
            .clone()
            .unwrap_or_else(|| format!("{:?} failure (causes: {:?})", self.category, self.causes))
    }

    pub fn capture(error: &anyhow::Error) -> Self {
        let causes: Vec<_> = error
            .chain()
            .take(8)
            .map(|cause| ErrorCategory::classify(&cause.to_string()))
            .collect();
        Self {
            category: causes.first().copied().unwrap_or(ErrorCategory::Unknown),
            causes,
            local_detail: None,
        }
    }

    pub fn deployment(message: &str, authorization: &reqwest::header::HeaderValue) -> Self {
        Self {
            category: ErrorCategory::Provider,
            causes: vec![ErrorCategory::Provider],
            local_detail: Some(super::redaction::sanitize(message, authorization)),
        }
    }
}

/// Fields that locate a failed capture step. They are reviewed typed values that
/// public capture journals already contain, so summaries built only from them
/// are as safe to publish as those journals.
const FAILURE_LOCATION_FIELDS: [Field; 7] = [
    Field::ActionIndex,
    Field::ActionType,
    Field::Condition,
    Field::State,
    Field::Artifact,
    Field::Endpoint,
    Field::ScenarioTimeoutMs,
];

/// The failed step of a capture, assembled from typed diagnostic fields.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FailureSite {
    stage: Stage,
    fields: BTreeMap<Field, FieldValue>,
    duration_ms: Option<u64>,
}

impl FailureSite {
    pub fn new(stage: Stage) -> Self {
        Self {
            stage,
            fields: BTreeMap::new(),
            duration_ms: None,
        }
    }

    /// Add a failed record. Errors propagate outward, so the innermost failure
    /// arrives first and wins each field; enclosing failures only fill gaps, such
    /// as the action that owned a failed condition.
    pub fn merge(&mut self, fields: &BTreeMap<Field, FieldValue>, duration_ms: Option<u64>) {
        for field in FAILURE_LOCATION_FIELDS {
            if let Some(value) = fields.get(&field) {
                self.fields.entry(field).or_insert_with(|| value.clone());
            }
        }
        self.duration_ms = self.duration_ms.or(duration_ms);
    }

    fn kind(&self, field: Field) -> Option<SafeValue> {
        match self.fields.get(&field) {
            Some(FieldValue::Kind(value)) => Some(*value),
            _ => None,
        }
    }

    fn number(&self, field: Field) -> Option<u64> {
        match self.fields.get(&field) {
            Some(FieldValue::Number(value)) => Some(*value),
            _ => None,
        }
    }

    /// The failing step in authoring terms, such as `Action 2 (click)`.
    pub fn location(&self) -> String {
        if let (Some(index), Some(action)) = (
            self.number(Field::ActionIndex),
            self.kind(Field::ActionType),
        ) {
            let name = match action {
                SafeValue::Navigate => "goto".to_owned(),
                action => serde_json::to_value(action)
                    .ok()
                    .and_then(|value| value.as_str().map(str::to_owned))
                    .unwrap_or_else(|| format!("{action:?}")),
            };
            return format!("Action {index} ({name})");
        }
        let location = match (
            self.kind(Field::ActionType),
            self.kind(Field::Condition),
            self.kind(Field::State),
        ) {
            (Some(SafeValue::Hook), Some(SafeValue::Before), _) => "onBeforeScript",
            (Some(SafeValue::Hook), Some(SafeValue::Ready), _) => "onReadyScript",
            (Some(SafeValue::Hook), _, Some(SafeValue::Ready)) => {
                "CSS override or browser ready script"
            }
            (Some(SafeValue::Hook), ..) => "Hook file loading",
            (Some(SafeValue::Restore), ..) if self.fields.contains_key(&Field::Artifact) => {
                "Cookie file"
            }
            (Some(SafeValue::Restore), ..) => "Saved state",
            (None, _, Some(SafeValue::Ready)) => "readySelector",
            _ => match self.stage {
                Stage::QueueSetup if self.kind(Field::Endpoint) == Some(SafeValue::Browser) => {
                    "Browser launch"
                }
                Stage::Navigation => "Navigation",
                Stage::ScreenshotSettling
                    if self.kind(Field::Condition) == Some(SafeValue::LazyLoading) =>
                {
                    "Lazy-load scrolling"
                }
                Stage::ScreenshotSettling => "Capture preparation",
                Stage::CaptureArtifacts => "Screenshot capture",
                Stage::StateHooks => "State or hooks",
                Stage::ActionsFrames => "Actions",
                _ => "Capture",
            },
        };
        location.to_owned()
    }

    /// One publish-safe line naming the step, the unmet condition, and the error
    /// category, for example `Action 1 (click): no element matched after 30012 ms
    /// (Timeout)`. It never contains selectors, URLs, page text, or error text.
    pub fn summary(&self, category: ErrorCategory) -> String {
        let location = self.location();
        if let Some(timeout) = self.number(Field::ScenarioTimeoutMs) {
            return format!(
                "scenarioTimeout of {timeout} ms expired during {location} ({category:?})"
            );
        }
        let problem = match self.kind(Field::Condition) {
            Some(SafeValue::Exists) => ": no element matched",
            Some(SafeValue::Visible) => ": element was not visible",
            Some(SafeValue::Stable) => ": element kept moving",
            Some(SafeValue::Scroll) => ": element could not be scrolled into view",
            Some(SafeValue::Unobscured) => ": element was covered or outside the viewport",
            Some(SafeValue::Enabled) => ": element was disabled",
            Some(SafeValue::Navigate) => ": URL did not match",
            _ => " failed",
        };
        let duration = self
            .duration_ms
            .map(|duration| format!(" after {duration} ms"))
            .unwrap_or_default();
        format!("{location}{problem}{duration} ({category:?})")
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ViewportIdentity {
    pub name: SafeIdentity,
    pub width: u32,
    pub height: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecutionIdentity {
    pub execution_id: ExecutionId,
    pub suite_id: SafeIdentity,
    pub scenario_id: SafeIdentity,
    pub viewport: ViewportIdentity,
    pub capture_role: CaptureRole,
    pub attempt: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticRecordV1 {
    #[serde(deserialize_with = "version_one")]
    pub schema_version: u16,
    pub operation_id: OperationId,
    pub operation_kind: OperationKind,
    pub capture_run_id: Option<CaptureRunId>,
    pub execution: Option<ExecutionIdentity>,
    pub sequence: u64,
    pub timestamp_utc_ms: i64,
    pub elapsed_ms: u64,
    pub severity: Severity,
    pub stage: Stage,
    pub event_name: DiagnosticEvent,
    pub message: String,
    pub fields: BTreeMap<Field, FieldValue>,
    pub span_id: SpanId,
    pub parent_span: Option<SpanId>,
    pub duration_ms: Option<u64>,
    pub outcome: Option<Outcome>,
    pub error_cause: Option<SanitizedError>,
}

fn version_one<'de, Deserializer: serde::Deserializer<'de>>(
    deserializer: Deserializer,
) -> Result<u16, Deserializer::Error> {
    let version = u16::deserialize(deserializer)?;
    if version != SCHEMA_VERSION {
        return Err(serde::de::Error::custom(
            "Unsupported diagnostic schema version",
        ));
    }
    Ok(version)
}

impl DiagnosticRecordV1 {
    pub fn new(
        operation_id: OperationId,
        operation_kind: OperationKind,
        stage: Stage,
        event: DiagnosticEvent,
    ) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            operation_id,
            operation_kind,
            capture_run_id: None,
            execution: None,
            sequence: 0,
            timestamp_utc_ms: chrono::Utc::now().timestamp_millis(),
            elapsed_ms: 0,
            severity: Severity::Info,
            stage,
            event_name: event,
            message: event.message().into(),
            fields: BTreeMap::new(),
            span_id: SpanId::new(),
            parent_span: None,
            duration_ms: None,
            outcome: None,
            error_cause: None,
        }
    }

    pub fn validate(&self) -> Result<()> {
        ensure!(
            self.schema_version == SCHEMA_VERSION,
            "Unsupported diagnostic schema version"
        );
        ensure!(
            self.message == self.event_name.message(),
            "Diagnostic messages must use safe templates"
        );
        ensure!(
            self.sequence <= MAX_JSON_INTEGER
                && self.elapsed_ms <= MAX_JSON_INTEGER
                && self
                    .duration_ms
                    .is_none_or(|duration| duration <= MAX_JSON_INTEGER),
            "Diagnostic integer exceeds JSON precision"
        );
        ensure!(
            (-8_640_000_000_000_000..=8_640_000_000_000_000).contains(&self.timestamp_utc_ms),
            "Diagnostic timestamp exceeds the supported date range"
        );
        for (field, value) in &self.fields {
            ensure!(
                *field != Field::PublicationUrl || self.operation_kind == OperationKind::Deployment,
                "Publication URLs are deployment-only"
            );
            ensure!(field.accepts(value), "Invalid diagnostic field type");
        }
        if let Some(execution) = &self.execution {
            ensure!(
                self.operation_kind == OperationKind::Capture,
                "Only captures can own execution identities"
            );
            ensure!(
                execution.attempt > 0
                    && execution.viewport.width > 0
                    && execution.viewport.height > 0,
                "Invalid execution attempt or viewport"
            );
        }
        if let Some(error) = &self.error_cause {
            ensure!(
                error.local_detail.as_ref().is_none_or(
                    |detail| detail.len() <= 8195 && !detail.chars().any(char::is_control)
                ),
                "Invalid provider diagnostic detail"
            );
            ensure!(error.causes.len() <= 8, "Too many diagnostic error causes");
            ensure!(
                self.operation_kind == OperationKind::Deployment || error.local_detail.is_none(),
                "Provider details cannot enter public capture records"
            );
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn publication_urls_reject_credentials_signed_values_and_capture_export() {
        for url in [
            "http://reports.example.test/",
            "https://user:secret@reports.example.test/",
            "https://reports.example.test/?sig=private",
            "https://reports.example.test/#secret",
            "https://reports.example.test/private-token",
        ] {
            assert!(PublicUrl::try_from(url.to_owned()).is_err());
        }
        let url = PublicUrl::try_from("https://reports.example.test/".to_owned()).unwrap();
        let mut record = DiagnosticRecordV1::new(
            OperationId::new(),
            OperationKind::Capture,
            Stage::Finish,
            DiagnosticEvent::Finished,
        );
        record
            .fields
            .insert(Field::PublicationUrl, FieldValue::PublicUrl(url));
        assert!(record.validate().is_err());
        record.operation_kind = OperationKind::Deployment;
        record.validate().unwrap();
    }

    use super::*;

    #[test]
    fn source_identities_round_trip_without_obfuscating_locations() {
        for source in [
            "search-functions",
            "keyword-search-button",
            "desktop",
            "#hero-search",
            "common/_search-ready.js",
            "https://example.test/search-results/",
        ] {
            let identity = SafeIdentity::try_from(source.to_owned()).unwrap();
            assert_eq!(identity.as_str(), source);
            let encoded = serde_json::to_string(&identity).unwrap();
            assert_eq!(
                serde_json::from_str::<SafeIdentity>(&encoded).unwrap(),
                identity
            );
        }
        let legacy = SafeIdentity::fingerprint("search-functions");
        assert_eq!(
            SafeIdentity::try_from(legacy.as_str().to_owned()).unwrap(),
            legacy
        );
    }

    #[test]
    fn diagnostic_record_v1_round_trips_through_json() {
        let mut record = DiagnosticRecordV1::new(
            OperationId::new(),
            OperationKind::Capture,
            Stage::ActionsFrames,
            DiagnosticEvent::StageFinished,
        );
        record.capture_run_id = Some(CaptureRunId::try_from("20261001_120000".to_owned()).unwrap());
        record.execution = Some(ExecutionIdentity {
            execution_id: ExecutionId::new(),
            suite_id: SafeIdentity::fingerprint("suite"),
            scenario_id: SafeIdentity::fingerprint("scenario"),
            viewport: ViewportIdentity {
                name: SafeIdentity::fingerprint("desktop"),
                width: 1200,
                height: 800,
            },
            capture_role: CaptureRole::Test,
            attempt: 2,
        });
        record
            .fields
            .insert(Field::ActionIndex, FieldValue::Number(3));
        record
            .fields
            .insert(Field::ActionType, FieldValue::Kind(SafeValue::Click));
        record.fields.insert(
            Field::Target,
            FieldValue::Identity(SafeIdentity::fingerprint("[data-secret=private]")),
        );
        record.error_cause = Some(SanitizedError::capture(&anyhow::anyhow!(
            "Timed out on private selector"
        )));
        record.outcome = Some(Outcome::Failure);
        record.validate().unwrap();
        let encoded = serde_json::to_string(&record).unwrap();
        assert!(!encoded.contains("private"));
        let decoded: DiagnosticRecordV1 = serde_json::from_str(&encoded).unwrap();
        decoded.validate().unwrap();
        assert_eq!(record, decoded);
    }

    #[test]
    fn diagnostic_ids_reject_paths_reserved_names_and_noncanonical_values() {
        for value in ["../escape", "CON", "a/b", "", "nul", "C:\\secrets"] {
            assert!(CaptureRunId::try_from(value.to_owned()).is_err());
            assert!(OperationId::try_from(value.to_owned()).is_err());
        }
        assert!(OperationId::try_from(uuid::Uuid::nil().to_string()).is_err());
        assert!(SafeIdentity::try_from("https://user:secret@example.test".to_owned()).is_err());
        assert!(serde_json::from_str::<ExecutionId>("\"../escape\"").is_err());
    }

    #[test]
    fn schema_rejects_unknown_versions_fields_and_unreviewed_payloads() {
        let mut record = DiagnosticRecordV1::new(
            OperationId::new(),
            OperationKind::Capture,
            Stage::QueueSetup,
            DiagnosticEvent::Started,
        );
        for timestamp in [
            i64::MIN,
            -8_640_000_000_000_001,
            8_640_000_000_000_001,
            i64::MAX,
        ] {
            record.timestamp_utc_ms = timestamp;
            assert!(record.validate().is_err());
        }
        for timestamp in [-8_640_000_000_000_000, 8_640_000_000_000_000, 0] {
            record.timestamp_utc_ms = timestamp;
            record.validate().unwrap();
        }
        let mut value = serde_json::to_value(&record).unwrap();
        value["schemaVersion"] = 2.into();
        assert!(serde_json::from_value::<DiagnosticRecordV1>(value.clone()).is_err());
        value["schemaVersion"] = 1.into();
        value["fields"] = serde_json::json!({"password": {"type": "kind", "value": "input"}});
        assert!(serde_json::from_value::<DiagnosticRecordV1>(value).is_err());
        record
            .fields
            .insert(Field::Destination, FieldValue::Number(1));
        assert!(record.validate().is_err());
        record.fields.clear();
        record.message = "private payload".into();
        assert!(record.validate().is_err());
    }

    #[test]
    fn capture_errors_never_export_arbitrary_payloads_or_provider_details() {
        let error =
            anyhow::anyhow!("typed-input synthetic-secret <script>alert(1)</script>\u{1b}[31m")
                .context("Navigation timeout https://user:password@example.test/?sig=private");
        let safe = SanitizedError::capture(&error);
        assert_eq!(safe.category, ErrorCategory::Timeout);
        let encoded = serde_json::to_string(&safe).unwrap();
        for secret in ["synthetic-secret", "script", "password", "private", "31m"] {
            assert!(!encoded.contains(secret));
        }
        let mut record = DiagnosticRecordV1::new(
            OperationId::new(),
            OperationKind::Capture,
            Stage::Finish,
            DiagnosticEvent::Finished,
        );
        record.error_cause = Some(SanitizedError::deployment(
            "HTTP 403; token=synthetic-secret",
            &reqwest::header::HeaderValue::from_static("Bearer synthetic-secret"),
        ));
        assert!(record.validate().is_err());
        record.operation_kind = OperationKind::Deployment;
        record.validate().unwrap();
        assert!(
            !serde_json::to_string(&record)
                .unwrap()
                .contains("synthetic-secret")
        );
    }

    #[test]
    fn error_categories_prefer_parse_failures_and_whole_word_timeouts() {
        let category =
            |message: &str| SanitizedError::capture(&anyhow::anyhow!("{message}")).category;
        for message in [
            "unknown field `readySelectr`, expected one of `readySelector`, `readyTimeout`",
            "data did not match any variant of untagged enum TestAction at line 4 column 7",
            "invalid type: integer `5`, expected a string",
            "missing field `url`",
            "workspace elementTimeout must be greater than zero",
            "`input` action must have either `value` or `file`",
            "Unknown keyboard key 'enter'",
        ] {
            assert_eq!(category(message), ErrorCategory::InvalidData, "{message}");
        }
        for message in [
            "Scenario 'checkout' exceeded its 600000ms timeout",
            "Timeout waiting for URL '**/done'",
            "Timed out after 30000ms waiting for element '#a' to exist in the DOM",
            "Navigation exceeded the configured limit",
        ] {
            assert_eq!(category(message), ErrorCategory::Timeout, "{message}");
        }
        assert_eq!(
            category("Cannot capture selector '#hero': element not found"),
            ErrorCategory::NotFound
        );
        assert_eq!(
            category("Element '#buy' is not visible after 5000ms (reason: display_none)"),
            ErrorCategory::Actionability
        );
        for (summary, expected) in [
            (
                "Test scenario failed to capture: Action 1 (save): no element matched after 3 ms (NotFound)",
                ErrorCategory::NotFound,
            ),
            (
                "Reference scenario failed to capture: Screenshot capture failed (Storage)",
                ErrorCategory::Storage,
            ),
        ] {
            assert_eq!(category(summary), expected, "{summary}");
        }
    }

    #[test]
    fn failure_summaries_name_the_step_and_condition_without_payloads() {
        type FailedRecord<'a> = (&'a [(Field, FieldValue)], Option<u64>);
        let site = |stage, records: &[FailedRecord<'_>]| {
            let mut site = FailureSite::new(stage);
            for (fields, duration) in records {
                site.merge(&fields.iter().cloned().collect(), *duration);
            }
            site
        };
        let kind = FieldValue::Kind;
        let target = (
            Field::Target,
            FieldValue::Identity(SafeIdentity::fingerprint("#private-selector")),
        );
        let action = [
            (Field::ActionIndex, FieldValue::Number(2)),
            (Field::ActionType, kind(SafeValue::Click)),
            target.clone(),
        ];
        let condition = [
            action[0].clone(),
            action[1].clone(),
            (Field::Condition, kind(SafeValue::Exists)),
        ];
        let click = site(
            Stage::ActionsFrames,
            &[(&condition, Some(30_012)), (&action, Some(30_020))],
        );
        assert_eq!(
            click.summary(ErrorCategory::Timeout),
            "Action 2 (click): no element matched after 30012 ms (Timeout)"
        );

        let covered = site(
            Stage::ActionsFrames,
            &[
                (&[(Field::Condition, kind(SafeValue::Unobscured))], None),
                (&action, Some(5_100)),
            ],
        );
        assert_eq!(
            covered.summary(ErrorCategory::Actionability),
            "Action 2 (click): element was covered or outside the viewport after 5100 ms (Actionability)"
        );

        let goto = [
            (Field::ActionIndex, FieldValue::Number(1)),
            (Field::ActionType, kind(SafeValue::Navigate)),
        ];
        let navigation = site(
            Stage::Navigation,
            &[
                (std::slice::from_ref(&target), Some(60_001)),
                (&goto, Some(60_005)),
            ],
        );
        assert_eq!(
            navigation.summary(ErrorCategory::Timeout),
            "Action 1 (goto) failed after 60001 ms (Timeout)"
        );

        let screenshot = site(
            Stage::CaptureArtifacts,
            &[
                (
                    &[(Field::Condition, kind(SafeValue::Visible)), target.clone()],
                    None,
                ),
                (
                    &[(
                        Field::Capture,
                        FieldValue::Identity(SafeIdentity::fingerprint("home__FINAL")),
                    )],
                    Some(105),
                ),
            ],
        );
        assert_eq!(
            screenshot.summary(ErrorCategory::Actionability),
            "Screenshot capture: element was not visible after 105 ms (Actionability)"
        );

        let ready = site(
            Stage::Navigation,
            &[
                (&[(Field::Condition, kind(SafeValue::Exists))], Some(1_000)),
                (&[(Field::State, kind(SafeValue::Ready))], Some(1_002)),
            ],
        );
        assert_eq!(
            ready.summary(ErrorCategory::Timeout),
            "readySelector: no element matched after 1000 ms (Timeout)"
        );

        for (phase, expected) in [
            (
                Some(SafeValue::Before),
                "onBeforeScript failed after 7 ms (Unknown)",
            ),
            (
                Some(SafeValue::Ready),
                "onReadyScript failed after 7 ms (Unknown)",
            ),
            (None, "Hook file loading failed after 7 ms (Unknown)"),
        ] {
            let mut fields = vec![(Field::ActionType, kind(SafeValue::Hook))];
            fields.extend(phase.map(|phase| (Field::Condition, kind(phase))));
            assert_eq!(
                site(Stage::StateHooks, &[(&fields, Some(7))]).summary(ErrorCategory::Unknown),
                expected
            );
        }
        let document_hooks = site(
            Stage::StateHooks,
            &[(
                &[
                    (Field::ActionType, kind(SafeValue::Hook)),
                    (Field::State, kind(SafeValue::Ready)),
                ],
                Some(3),
            )],
        );
        assert_eq!(
            document_hooks.location(),
            "CSS override or browser ready script"
        );

        let timeout = site(
            Stage::ActionsFrames,
            &[(
                &[
                    (Field::ScenarioTimeoutMs, FieldValue::Number(600_000)),
                    action[0].clone(),
                    action[1].clone(),
                ],
                None,
            )],
        );
        assert_eq!(
            timeout.summary(ErrorCategory::Timeout),
            "scenarioTimeout of 600000 ms expired during Action 2 (click) (Timeout)"
        );
        for site in [click, covered, navigation, screenshot, ready, timeout] {
            assert!(!site.summary(ErrorCategory::Unknown).contains("private"));
            assert!(!site.summary(ErrorCategory::Unknown).contains("sha256"));
        }
    }
}

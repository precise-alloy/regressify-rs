mod outbox;

use std::{
    ffi::OsStr,
    sync::{OnceLock, mpsc},
    time::Duration,
};

use chrono::Utc;
use outbox::{Outbox, Pending, Selection};
use posthog_rs::{Client, ClientOptionsBuilder, Error, Event};
use serde_json::Value;
use uuid::Uuid;

const HOST: &str = "https://us.i.posthog.com";
const PROJECT_TOKEN: &str = "phc_rFcdnV8w5LESQjpYoi2Psy7npbXMicUtNt32CnUpg4yk";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const POLL_INTERVAL: Duration = Duration::from_secs(30);

static WORKER: OnceLock<Option<mpsc::SyncSender<()>>> = OnceLock::new();

fn local_outbox() -> Option<Outbox> {
    // Test-launched binaries are not cfg(test). They must neither replay nor
    // purge the user's queue, even when the child inherits DO_NOT_TRACK.
    if std::env::var_os("REGRESSIFY_TEST_NO_ANALYTICS").as_deref() == Some(OsStr::new("1")) {
        return None;
    }
    Some(Outbox::new(
        directories::BaseDirs::new()?
            .data_local_dir()
            .join("regressify")
            .join("analytics"),
    ))
}

/// Resume pending delivery without collecting a launch event. Opt-out purges
/// synchronously so even a short-lived CLI invocation honors it before exit.
pub fn start() {
    if cfg!(test) {
        return;
    }
    if let Some(outbox) = local_outbox() {
        start_using(
            outbox,
            opted_out(std::env::var_os("DO_NOT_TRACK").as_deref()),
        );
    }
}

fn start_using(outbox: Outbox, disabled: bool) {
    if disabled {
        let _ = outbox.purge();
    } else {
        wake(outbox);
    }
}

fn opted_out(value: Option<&OsStr>) -> bool {
    value == Some(OsStr::new("1"))
}

/// Queue an anonymous `regressify_[object]_[verb]` event. Callers must exclude
/// credentials and personal data. Only the local write is awaited, never HTTP.
pub async fn capture(object: &str, verb: &str, properties: Value) {
    if cfg!(test) || opted_out(std::env::var_os("DO_NOT_TRACK").as_deref()) {
        return;
    }
    let (Some(outbox), Some(event)) = (local_outbox(), anonymous_event(object, verb, properties))
    else {
        return;
    };
    enqueue(outbox, event, wake).await;
}

async fn enqueue(outbox: Outbox, event: Pending, wake: impl FnOnce(Outbox) + Send + 'static) {
    if let Ok(runtime) = tokio::runtime::Handle::try_current() {
        let _ = runtime
            .spawn_blocking(move || {
                if outbox.push(event).is_ok() {
                    wake(outbox);
                }
            })
            .await;
    }
}

fn wake(outbox: Outbox) {
    if let Some(sender) = WORKER.get_or_init(|| spawn_worker(outbox, HOST, PROJECT_TOKEN).ok()) {
        let _ = sender.try_send(());
    }
}

fn valid_part(part: &str) -> bool {
    !part.is_empty() && part.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

fn anonymous_event(object: &str, verb: &str, properties: Value) -> Option<Pending> {
    if !valid_part(object) || !valid_part(verb) {
        return None;
    }
    let Value::Object(mut properties) = properties else {
        return None;
    };
    properties.insert("app_version".into(), env!("CARGO_PKG_VERSION").into());
    Some(Pending {
        id: Uuid::new_v4(),
        created_ms: Utc::now().timestamp_millis(),
        object: object.to_owned(),
        verb: verb.to_owned(),
        properties,
        attempts: 0,
        due_ms: 0,
    })
}

impl Pending {
    fn wire_event(&self) -> Option<Event> {
        if !valid_part(&self.object) || !valid_part(&self.verb) {
            return None;
        }
        // Both identifiers are event-scoped. Replay never creates a new identity.
        let mut event = Event::new(
            format!("regressify_{}_{}", self.object, self.verb),
            self.id.to_string(),
        );
        event.set_uuid(self.id);
        event
            .set_timestamp(chrono::DateTime::from_timestamp_millis(self.created_ms)?)
            .ok()?;
        for (key, value) in &self.properties {
            event.insert_prop(key, value).ok()?;
        }
        event.insert_prop("$process_person_profile", false).ok()?;
        event.insert_prop("$geoip_disable", true).ok()?;
        Some(event)
    }
}

fn spawn_worker(outbox: Outbox, host: &str, token: &str) -> std::io::Result<mpsc::SyncSender<()>> {
    let (sender, receiver) = mpsc::sync_channel(1);
    let (host, token) = (host.to_owned(), token.to_owned());
    std::thread::Builder::new()
        .name("analytics".into())
        .spawn(move || {
            // Own the runtime and silence its entire lifetime, including HTTP cleanup.
            tracing::subscriber::with_default(tracing::subscriber::NoSubscriber::default(), || {
                let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                else {
                    return;
                };
                let mut client = None;
                loop {
                    let wait = runtime
                        .block_on(deliver_one(
                            &outbox,
                            &mut client,
                            &host,
                            &token,
                            REQUEST_TIMEOUT,
                        ))
                        .unwrap_or(POLL_INTERVAL);
                    if matches!(
                        receiver.recv_timeout(wait.min(POLL_INTERVAL)),
                        Err(mpsc::RecvTimeoutError::Disconnected)
                    ) {
                        break;
                    }
                }
            });
        })?;
    Ok(sender)
}

async fn deliver_one(
    outbox: &Outbox,
    client: &mut Option<Client>,
    host: &str,
    token: &str,
    timeout: Duration,
) -> anyhow::Result<Duration> {
    // Only senders take this lock. Neither capture nor opt-out waits on the network.
    let Some(_sender) = outbox.try_sender()? else {
        return Ok(POLL_INTERVAL);
    };
    let item = match outbox.select(Utc::now().timestamp_millis())? {
        Selection::Wait(delay) => return Ok(delay),
        Selection::Ready(item) => item,
    };
    let Some(event) = item.wire_event() else {
        outbox.finish(item.id, false, Utc::now().timestamp_millis())?;
        return Ok(Duration::ZERO);
    };
    let client = match client {
        Some(client) => client,
        None => {
            let http = reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .retry(reqwest::retry::never())
                .build()?;
            let options = ClientOptionsBuilder::default()
                .host(host)
                .api_key(token.to_owned())
                .request_timeout_seconds(REQUEST_TIMEOUT.as_secs())
                .max_capture_attempts(1)
                .http_client(http)
                .disable_geoip(true)
                .is_server(false)
                .on_error(|_| {})
                .build()?;
            client.insert(posthog_rs::client(options).await)
        }
    };
    let retry = match tokio::time::timeout(timeout, client.capture_immediate(event)).await {
        Ok(Ok(summary)) => summary.submitted() != 1 || !summary.all_persisted(),
        Ok(Err(error)) => retryable(&error),
        Err(_) => true,
    };
    outbox.finish(item.id, retry, Utc::now().timestamp_millis())?;
    Ok(Duration::ZERO)
}

fn retryable(error: &Error) -> bool {
    match error {
        Error::RateLimit | Error::ServerError { .. } => true,
        Error::Connection(message) => {
            // posthog-rs v0 exposes unmapped HTTP statuses (including 403 and
            // 408) as Connection, with this prefix, rather than a typed status.
            let status = message
                .strip_prefix("Unexpected HTTP status ")
                .and_then(|rest| rest.split_once(':'))
                .and_then(|(code, _)| code.parse::<u16>().ok());
            matches!(status, None | Some(408 | 429 | 500..=599))
        }
        Error::BadRequest(_)
        | Error::Unauthorized
        | Error::BillingLimitExceeded(_)
        | Error::InvalidTimestamp(_)
        | Error::Serialization(_) => false,
        _ => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn anonymous_events_preserve_properties_without_person_profiles() {
        let event = anonymous_event(
            "site",
            "deployed",
            json!({
                "url": "https://reports.example.com/",
                "provider": "netlify",
                "$process_person_profile": true,
                "$geoip_disable": false,
            }),
        )
        .unwrap()
        .wire_event()
        .unwrap();
        assert_eq!(event.event_name(), "regressify_site_deployed");
        assert!(uuid::Uuid::parse_str(event.distinct_id()).is_ok());
        assert_eq!(event.properties()["url"], "https://reports.example.com/");
        assert_eq!(event.properties()["provider"], "netlify");
        assert_eq!(event.properties()["app_version"], env!("CARGO_PKG_VERSION"));
        assert_eq!(event.properties()["$process_person_profile"], false);
        assert_eq!(event.properties()["$geoip_disable"], true);
    }

    #[test]
    fn events_have_independent_anonymous_identities() {
        let first = anonymous_event("application", "started", json!({})).unwrap();
        let second = anonymous_event("application", "started", json!({})).unwrap();
        assert_ne!(first.id, second.id);
    }

    #[test]
    fn non_object_properties_are_not_captured() {
        for properties in [Value::Null, json!([]), json!("invalid"), json!(1)] {
            assert!(anonymous_event("application", "started", properties).is_none());
        }
    }

    #[test]
    fn event_names_always_use_the_regressify_object_verb_format() {
        for (object, verb) in [("site", "deployed"), ("test_suite", "run_started")] {
            let event = anonymous_event(object, verb, json!({}))
                .unwrap()
                .wire_event()
                .unwrap();
            assert_eq!(event.event_name(), format!("regressify_{object}_{verb}"));
            assert!(!event.event_name().contains(' '));
        }
        for (object, verb) in [
            ("", "started"),
            ("site", ""),
            ("regressify site", "deployed"),
            ("site", "deployed url"),
            ("site\n", "deployed"),
            ("site", "deployed\0"),
        ] {
            assert!(anonymous_event(object, verb, json!({})).is_none());
        }
    }

    #[test]
    fn missing_runtime_skips_tracking() {
        let root = tempfile::tempdir().unwrap();
        futures::executor::block_on(enqueue(
            Outbox::new(root.path().join("analytics")),
            anonymous_event("application", "started", json!({})).unwrap(),
            |_| panic!("Must not start a sender"),
        ));
        assert!(!root.path().join("analytics").exists());
    }

    #[test]
    fn do_not_track_is_respected() {
        assert!(opted_out(Some(OsStr::new("1"))));
        for value in [None, Some(OsStr::new("")), Some(OsStr::new("0"))] {
            assert!(!opted_out(value));
        }
    }

    #[test]
    fn fixture_process_cannot_resolve_the_real_outbox() {
        const FLAG: &str = "REGRESSIFY_TEST_NO_ANALYTICS";
        if std::env::var(FLAG).as_deref() == Ok("1") {
            // Resolving a path does no I/O, even if this regression fails.
            assert!(local_outbox().is_none());
            return;
        }
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "analytics::tests::fixture_process_cannot_resolve_the_real_outbox",
            ])
            .env(FLAG, "1")
            .env("DO_NOT_TRACK", "1")
            .status()
            .unwrap();
        assert!(status.success());
    }

    #[tokio::test]
    async fn unit_tests_never_initialize_the_production_client() {
        capture("site", "deployed", json!({})).await;
        start();
        assert!(WORKER.get().is_none());
    }
}

#[cfg(test)]
mod delivery_tests;

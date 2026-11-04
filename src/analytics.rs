use std::{ffi::OsStr, time::Duration};

use posthog_rs::{Client, ClientOptions, ClientOptionsBuilder, ClientOptionsBuilderError, Event};
use serde_json::Value;
use tokio::sync::OnceCell;
use tracing::{instrument::WithSubscriber, subscriber::NoSubscriber};

const HOST: &str = "https://us.i.posthog.com";
const PROJECT_TOKEN: &str = "phc_rFcdnV8w5LESQjpYoi2Psy7npbXMicUtNt32CnUpg4yk";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);

static CLIENT: OnceCell<Option<Client>> = OnceCell::const_new();

/// Captures `regressify_[object]_[verb]` with JSON object properties, anonymously.
/// Object and verb must be nonempty ASCII letters, digits, or underscores.
/// Callers must exclude credentials and personal data from the properties.
/// Delivery is silent, bounded, and skipped without a Tokio runtime or with DO_NOT_TRACK=1.
pub async fn capture(object: &str, verb: &str, properties: Value) {
    if cfg!(test) || opted_out(std::env::var_os("DO_NOT_TRACK").as_deref()) {
        return;
    }
    capture_using(&CLIENT, object, verb, properties, || {
        client_options(HOST, PROJECT_TOKEN)
    })
    .await;
}

fn opted_out(value: Option<&OsStr>) -> bool {
    value == Some(OsStr::new("1"))
}

async fn capture_using(
    client: &OnceCell<Option<Client>>,
    object: &str,
    verb: &str,
    properties: Value,
    options: impl FnOnce() -> Result<ClientOptions, ClientOptionsBuilderError>,
) {
    if tokio::runtime::Handle::try_current().is_err() {
        return;
    }
    let work = async {
        let Some(event) = anonymous_event(object, verb, properties) else {
            return;
        };
        let client = client
            .get_or_init(|| async { Some(posthog_rs::client(options().ok()?).await) })
            .await;
        if let Some(client) = client {
            // Workers exit after completion; an in-memory queue would lose their events.
            let _ = client.capture_immediate(event).await;
        }
    };
    // Keep timeout cleanup inside the silent scope too; HTTP futures log on drop.
    async {
        let _ = tokio::time::timeout(REQUEST_TIMEOUT, work).await;
    }
    .with_subscriber(NoSubscriber::default())
    .await;
}

fn client_options(host: &str, token: &str) -> Result<ClientOptions, ClientOptionsBuilderError> {
    ClientOptionsBuilder::default()
        .host(host)
        .api_key(token.to_owned())
        .request_timeout_seconds(REQUEST_TIMEOUT.as_secs())
        .max_capture_attempts(1)
        .shutdown_timeout_ms(REQUEST_TIMEOUT.as_millis() as u64)
        .disable_geoip(true)
        .is_server(false)
        .on_error(|_| {})
        .build()
}

fn anonymous_event(object: &str, verb: &str, properties: Value) -> Option<Event> {
    if [object, verb].into_iter().any(|part| {
        part.is_empty()
            || !part
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
    }) {
        return None;
    }
    let mut event = Event::new_anon(format!("regressify_{object}_{verb}"));
    for (key, value) in properties.as_object()? {
        event.insert_prop(key, value).ok()?;
    }
    event
        .insert_prop("app_version", env!("CARGO_PKG_VERSION"))
        .ok()?;
    event.insert_prop("$process_person_profile", false).ok()?;
    event.insert_prop("$geoip_disable", true).ok()?;
    Some(event)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    };
    use tracing_subscriber::{Layer, layer::Context, prelude::*};

    #[derive(Clone, Default)]
    struct RecordedEvents(Arc<Mutex<Vec<String>>>);

    impl<Subscriber: tracing::Subscriber> Layer<Subscriber> for RecordedEvents {
        fn on_event(&self, event: &tracing::Event<'_>, _: Context<'_, Subscriber>) {
            let metadata = event.metadata();
            self.0
                .lock()
                .unwrap()
                .push(format!("{}: {}", metadata.target(), metadata.name()));
        }
    }

    fn mock_server(
        status: u16,
        count: usize,
    ) -> (
        Arc<tiny_http::Server>,
        String,
        std::thread::JoinHandle<Vec<Value>>,
    ) {
        let server = Arc::new(tiny_http::Server::http("127.0.0.1:0").unwrap());
        let host = format!("http://{}", server.server_addr());
        let receiver = server.clone();
        let task = std::thread::spawn(move || {
            (0..count)
                .map(|_| {
                    let mut request = receiver
                        .recv_timeout(Duration::from_secs(5))
                        .unwrap()
                        .expect("Expected a local capture request");
                    assert_eq!(request.method().as_str(), "POST");
                    assert_eq!(request.url(), "/batch/");
                    let payload = serde_json::from_reader(request.as_reader()).unwrap();
                    request
                        .respond(
                            tiny_http::Response::from_string("synthetic response")
                                .with_status_code(status),
                        )
                        .unwrap();
                    payload
                })
                .collect()
        });
        (server, host, task)
    }

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
        assert_ne!(first.distinct_id(), second.distinct_id());
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
            let event = anonymous_event(object, verb, json!({})).unwrap();
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
    fn us_cloud_options_are_valid() {
        assert!(client_options(HOST, "phc_test_project").is_ok());
    }

    #[tokio::test]
    async fn captures_events_and_reuses_the_client() {
        let (server, host, task) = mock_server(200, 2);
        let client = OnceCell::new();
        let initializations = AtomicUsize::new(0);
        for (object, verb) in [("site", "deployed"), ("application", "started")] {
            capture_using(
                &client,
                object,
                verb,
                json!({ "url": "https://reports.example.com/" }),
                || {
                    initializations.fetch_add(1, Ordering::Relaxed);
                    client_options(&host, "phc_test_project")
                },
            )
            .await;
        }
        let payloads = task.join().unwrap();
        assert_eq!(initializations.load(Ordering::Relaxed), 1);
        for (payload, name) in payloads
            .iter()
            .zip(["regressify_site_deployed", "regressify_application_started"])
        {
            assert_eq!(payload["api_key"], "phc_test_project");
            assert_eq!(payload["batch"].as_array().unwrap().len(), 1);
            let event = &payload["batch"][0];
            assert_eq!(event["event"], name);
            assert_eq!(event["properties"]["url"], "https://reports.example.com/");
            assert_eq!(event["properties"]["$process_person_profile"], false);
            assert_eq!(event["properties"]["$geoip_disable"], true);
        }
        assert!(server.try_recv().unwrap().is_none());
    }

    #[tokio::test]
    async fn rejected_requests_are_silent_and_do_not_retry_or_fail() {
        for status in [400, 401, 403, 429, 500, 503] {
            let (server, host, task) = mock_server(status, 1);
            let client = OnceCell::new();
            let recorded = RecordedEvents::default();
            capture_using(&client, "site", "deployed", json!({}), || {
                client_options(&host, "phc_test_project")
            })
            .with_subscriber(tracing_subscriber::registry().with(recorded.clone()))
            .await;
            assert_eq!(task.join().unwrap().len(), 1);
            let unexpected = server.try_recv().unwrap();
            assert!(
                unexpected.is_none(),
                "Unexpected additional request after HTTP {status}: {unexpected:?}"
            );
            assert_eq!(*recorded.0.lock().unwrap(), Vec::<String>::new());
        }
    }

    #[test]
    fn shared_client_handles_concurrent_calls_and_runtime_restarts() {
        let (server, host, task) = mock_server(200, 4);
        let client = OnceCell::new();
        let initializations = AtomicUsize::new(0);
        let options = || {
            initializations.fetch_add(1, Ordering::Relaxed);
            client_options(&host, "phc_test_project")
        };
        for _ in 0..2 {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap()
                .block_on(async {
                    tokio::join!(
                        capture_using(&client, "site", "deployed", json!({}), &options),
                        capture_using(&client, "application", "started", json!({}), &options),
                    );
                });
        }
        assert_eq!(initializations.load(Ordering::Relaxed), 1);
        assert_eq!(task.join().unwrap().len(), 4);
        assert!(server.try_recv().unwrap().is_none());
    }

    #[tokio::test]
    async fn connection_failures_are_silent() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let host = format!("http://{}", listener.local_addr().unwrap());
        drop(listener);
        let client = OnceCell::new();
        let recorded = RecordedEvents::default();
        capture_using(&client, "application", "started", json!({}), || {
            client_options(&host, "phc_test_project")
        })
        .with_subscriber(tracing_subscriber::registry().with(recorded.clone()))
        .await;
        assert_eq!(*recorded.0.lock().unwrap(), Vec::<String>::new());
    }

    #[tokio::test]
    async fn stalled_requests_time_out_without_failing() {
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let host = format!("http://{}", server.server_addr());
        let (release, wait) = std::sync::mpsc::channel();
        let task = std::thread::spawn(move || {
            let request = server
                .recv_timeout(Duration::from_secs(10))
                .unwrap()
                .expect("Expected a local capture request");
            wait.recv_timeout(Duration::from_secs(10)).unwrap();
            drop(request);
        });
        let started = std::time::Instant::now();
        capture_using(
            &OnceCell::new(),
            "application",
            "started",
            json!({}),
            || client_options(&host, "phc_test_project"),
        )
        .await;
        let elapsed = started.elapsed();
        release.send(()).unwrap();
        task.join().unwrap();
        assert!(elapsed >= Duration::from_secs(1));
        assert!(elapsed < REQUEST_TIMEOUT + Duration::from_secs(2));
    }

    #[tokio::test]
    async fn configuration_failures_disable_tracking_without_failing() {
        let client = OnceCell::new();
        capture_using(&client, "application", "started", json!({}), || {
            Err(ClientOptionsBuilderError::ValidationError(
                "invalid options".into(),
            ))
        })
        .await;
        assert!(matches!(client.get(), Some(None)));
    }

    #[test]
    fn missing_runtime_skips_tracking_without_initializing_the_client() {
        let client = OnceCell::new();
        futures::executor::block_on(capture_using(
            &client,
            "application",
            "started",
            json!({}),
            || panic!("Tracking without a runtime must not initialize a client"),
        ));
        assert!(client.get().is_none());
    }

    #[test]
    fn do_not_track_is_respected() {
        assert!(opted_out(Some(OsStr::new("1"))));
        for value in [None, Some(OsStr::new("")), Some(OsStr::new("0"))] {
            assert!(!opted_out(value));
        }
    }

    #[tokio::test]
    async fn unit_tests_never_initialize_the_production_client() {
        capture("site", "deployed", json!({})).await;
        assert!(CLIENT.get().is_none());
    }
}

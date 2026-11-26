use super::*;
use serde_json::json;
use std::{path::PathBuf, process::Command, sync::Arc, time::Instant};

const TOKEN: &str = "phc_test_project";
const DEADLINE: Duration = Duration::from_secs(10);

fn request(server: &tiny_http::Server) -> (tiny_http::Request, Value) {
    let mut request = server
        .recv_timeout(DEADLINE)
        .unwrap()
        .expect("loopback request");
    assert_eq!(request.method().as_str(), "POST");
    assert_eq!(request.url(), "/batch/");
    let payload: Value = serde_json::from_reader(request.as_reader()).unwrap();
    assert_eq!(payload["api_key"], TOKEN);
    assert_eq!(payload["batch"].as_array().unwrap().len(), 1);
    (request, payload["batch"][0].clone())
}

fn put(outbox: &Outbox, object: &str) -> Uuid {
    let event = anonymous_event(object, "finished", json!({"n": 17})).unwrap();
    let id = event.id;
    outbox.push(event).unwrap();
    id
}

#[tokio::test]
async fn statuses_retry_only_transient_errors_and_ack_only_the_sent_item() {
    let root = tempfile::tempdir().unwrap();
    let outbox = Outbox::new(root.path().into());
    let server = Arc::new(tiny_http::Server::http("127.0.0.1:0").unwrap());
    let host = format!("http://{}", server.server_addr());
    let mut client = None;
    for (status, retry) in [
        (200, false),
        (400, false),
        (401, false),
        (403, false),
        (307, false),
        (408, true),
        (429, true),
        (500, true),
        (503, true),
    ] {
        let id = put(&outbox, "suite");
        let receiver = server.clone();
        let response = std::thread::spawn(move || {
            let (req, event) = request(&receiver);
            assert_eq!(event["uuid"], id.to_string());
            assert_eq!(event["event"], "regressify_suite_finished");
            assert_eq!(event["properties"]["$process_person_profile"], false);
            assert_eq!(event["properties"]["$geoip_disable"], true);
            req.respond(tiny_http::Response::empty(status)).unwrap();
        });
        deliver_one(&outbox, &mut client, &host, TOKEN, REQUEST_TIMEOUT)
            .await
            .unwrap();
        response.join().unwrap();
        let next = outbox
            .select(Utc::now().timestamp_millis() + 60_000)
            .unwrap();
        assert_eq!(
            matches!(next, Selection::Ready(p) if p.id == id),
            retry,
            "status {status}"
        );
        outbox.purge().unwrap();
    }
}

#[tokio::test]
async fn a_clock_rollback_after_selection_does_not_cost_an_attempt_or_the_event() {
    let root = tempfile::tempdir().unwrap();
    let outbox = Outbox::new(root.path().into());
    let mut event = anonymous_event("site", "deployed", json!({})).unwrap();
    let later = Utc::now().timestamp_millis() + 60_000;
    event.created_ms = later;
    let id = event.id;
    outbox.push(event).unwrap();
    let Selection::Ready(item) = outbox.select(later).unwrap() else {
        panic!("not selected")
    };
    let mut client = None;
    deliver_selected(&outbox, &mut client, "", TOKEN, REQUEST_TIMEOUT, item)
        .await
        .unwrap();
    assert!(client.is_none());
    assert!(matches!(
        outbox.select(Utc::now().timestamp_millis()).unwrap(),
        Selection::Wait(_)
    ));
    assert!(matches!(outbox.select(later).unwrap(), Selection::Ready(p)
        if p.id == id && p.created_ms == later && p.attempts == 0 && p.due_ms == 0));
}

#[tokio::test]
async fn a_purge_between_selection_and_dispatch_prevents_the_request() {
    let root = tempfile::tempdir().unwrap();
    let outbox = Outbox::new(root.path().into());
    let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
    let host = format!("http://{}", server.server_addr());
    put(&outbox, "suite");
    let Selection::Ready(item) = outbox.select(Utc::now().timestamp_millis()).unwrap() else {
        panic!("not selected")
    };
    outbox.purge().unwrap();
    deliver_selected(
        &outbox,
        &mut None,
        &host,
        TOKEN,
        Duration::from_millis(200),
        item,
    )
    .await
    .unwrap();
    assert!(server.try_recv().unwrap().is_none());
    assert!(outbox.is_empty().unwrap());
}

#[tokio::test]
async fn connection_failures_and_stalled_requests_remain_queued() {
    let socket = tokio::net::TcpSocket::new_v4().unwrap();
    socket.bind("127.0.0.1:0".parse().unwrap()).unwrap();
    let stalled = tiny_http::Server::http("127.0.0.1:0").unwrap();
    for address in [
        socket.local_addr().unwrap().to_string(),
        stalled.server_addr().to_string(),
    ] {
        let root = tempfile::tempdir().unwrap();
        let outbox = Outbox::new(root.path().into());
        let id = put(&outbox, "test");
        let started = Instant::now();
        deliver_one(
            &outbox,
            &mut None,
            &format!("http://{address}"),
            TOKEN,
            Duration::from_millis(200),
        )
        .await
        .unwrap();
        assert!(started.elapsed() < DEADLINE);
        assert!(
            matches!(outbox.select(Utc::now().timestamp_millis() + 60_000).unwrap(),
            Selection::Ready(p) if p.id == id && p.attempts == 1)
        );
    }
    assert!(stalled.try_recv().unwrap().is_some());
}

#[test]
fn interrupted_sender_replays_the_original_event_without_blocking_producers() {
    const CHILD: &str = "REGRESSIFY_TEST_ANALYTICS_CHILD";
    if let Some(root) = std::env::var_os(CHILD) {
        let root = PathBuf::from(root);
        let outbox = Outbox::new(root.clone());
        let host = std::env::var("REGRESSIFY_TEST_ANALYTICS_HOST").unwrap();
        let sender = spawn_worker(outbox.clone(), &host, TOKEN).unwrap();
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(enqueue(
                outbox,
                anonymous_event("suite", "finished", json!({"n":17})).unwrap(),
                move |_| {
                    let _ = sender.try_send(());
                },
            ));
        std::fs::write(root.join("producer-returned"), []).unwrap();
        // Keep the process alive until the parent interrupts the in-flight request.
        std::thread::park_timeout(DEADLINE * 3);
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let outbox = Outbox::new(root.path().into());
    let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
    let host = format!("http://{}", server.server_addr());
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "analytics::delivery_tests::interrupted_sender_replays_the_original_event_without_blocking_producers"])
        .env(CHILD, root.path()).env("REGRESSIFY_TEST_ANALYTICS_HOST", &host)
        .spawn().unwrap();
    let (in_flight, original) = request(&server);
    let until = Instant::now() + DEADLINE;
    while !root.path().join("producer-returned").exists() {
        assert!(Instant::now() < until, "Producer waited for HTTP");
        std::thread::sleep(Duration::from_millis(10));
    }
    // A second process can enqueue while the request is stalled, but cannot send it twice.
    assert!(outbox.try_sender().unwrap().is_none());
    let second_id = put(&outbox, "report");
    child.kill().unwrap();
    child.wait().unwrap();
    drop(in_flight);
    let sender = spawn_worker(outbox.clone(), &host, TOKEN).unwrap();
    for previous in [Some(&original), None] {
        let (req, event) = request(&server);
        if let Some(original) = previous {
            assert_eq!(&event, original);
        } else {
            assert_eq!(event["uuid"], second_id.to_string());
            assert_eq!(event["event"], "regressify_report_finished");
        }
        req.respond(tiny_http::Response::empty(204)).unwrap();
    }
    let until = Instant::now() + DEADLINE;
    while !outbox.is_empty().unwrap() {
        assert!(Instant::now() < until, "Acknowledged item was not removed");
        std::thread::sleep(Duration::from_millis(10));
    }
    drop(sender);
}

#[test]
#[ignore = "exercises the real 30-second HTTP deadline and retry backoff"]
fn real_worker_retries_three_times_then_defers_the_timed_out_event() {
    let root = tempfile::tempdir().unwrap();
    let outbox = Outbox::new(root.path().into());
    let id = put(&outbox, "report");
    let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
    let sender = spawn_worker(
        outbox.clone(),
        &format!("http://{}", server.server_addr()),
        TOKEN,
    )
    .unwrap();
    let (mut req, mut event) = request(&server);
    for (status, min_backoff) in [(503, Duration::from_secs(1)), (429, Duration::from_secs(2))] {
        assert_eq!(event["uuid"], id.to_string());
        let responded = Instant::now();
        req.respond(tiny_http::Response::empty(status)).unwrap();
        (req, event) = request(&server);
        assert!(
            responded.elapsed() + Duration::from_millis(30) >= min_backoff,
            "Retried {status} too early"
        );
    }
    assert_eq!(event["uuid"], id.to_string());
    let started = Instant::now();
    loop {
        if let Selection::Wait(delay) = outbox.select(Utc::now().timestamp_millis()).unwrap() {
            assert!(delay > Duration::from_secs(290));
            break;
        }
        assert!(
            started.elapsed() < Duration::from_secs(35),
            "Request did not time out"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(
        started.elapsed() >= Duration::from_secs(29),
        "Request timed out early"
    );
    assert!(
        matches!(outbox.select(Utc::now().timestamp_millis() + 300_000).unwrap(),
        Selection::Ready(p) if p.id == id && p.attempts == 0)
    );
    drop(req);
    drop(sender);
}

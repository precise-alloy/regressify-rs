use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use anyhow::{Context, Result, anyhow};
use tiny_http::{Header, Request, Response, Server, StatusCode};

use crate::types::BasicAuth;

use super::{
    authentication::Authentication,
    browsers::{LaunchOptions, prepare_browser},
};

pub(crate) async fn launch_browser(
    log_prefix: &str,
    options: LaunchOptions,
) -> Result<(chromiumoxide::Browser, tokio::task::JoinHandle<()>, PathBuf)> {
    let executable = prepare_browser(Some(false)).await?;
    super::browsers::launch_browser(&Some(executable), log_prefix, options)
        .await
        .map_err(|error| anyhow!("{error}"))
}

pub(crate) struct Site {
    pub origin: String,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Site {
    pub fn new(handler: impl Fn(Request) + Send + 'static) -> Result<Self> {
        let server = Server::http("127.0.0.1:0").map_err(|error| anyhow!("{error}"))?;
        let origin = format!("http://{}", server.server_addr());
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        let thread = std::thread::spawn(move || {
            while !stopped.load(Ordering::Relaxed) {
                match server.recv_timeout(Duration::from_millis(100)) {
                    Ok(Some(request)) => handler(request),
                    Ok(None) => {}
                    Err(_) => break,
                }
            }
        });
        Ok(Self {
            origin,
            stop,
            thread: Some(thread),
        })
    }
}

impl Drop for Site {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn expanded_action_page(request: Request) {
    let _ = request.respond(Response::from_string(r#"<!doctype html><html><head>
<style>
body { margin:8px; font:14px sans-serif; }
button,input { width:95px; height:32px; margin:3px; }
.row { display:flex; gap:15px; margin:12px 0; }
#pointer,#native,#drop,#files { width:100px; height:65px; background:lightblue; }
#scroller { width:160px; height:60px; overflow:auto; }
#scroller > div { width:600px; height:600px; }
#touch { width:200px; height:90px; background:lightgreen; touch-action:none; }
#covered-wrap { position:relative; width:140px; height:40px; }
#covered { width:130px; margin:0; }
#cover { position:absolute; width:60px; height:40px; left:40px; top:0; background:gray; }
</style></head><body>
<div class="row"><button id="click">Click</button><button id="context">Context</button><a id="aux" href='#aux-link'>Aux link</a><button id="odd'id">Quoted</button>
<div id="covered-wrap"><button id="covered">Partial cover</button><div id="cover"></div></div></div>
<div><input id="first"><input id="second"><input id="typed"><input id="range" type="range" min="0" max="100" value="10"><input id="color" type="color" value='#000000'><input id="otp"><span id="copy">Alpha Beta Gamma</span></div>
<div class="row"><div id="pointer">Pointer source</div><div id="native" draggable="true">Native source</div><div id="drop">Drop target</div><div id="files">Files</div></div>
<div class="row"><div id="scroller"><div>Scrollable</div></div><div id="touch">Touch</div></div>
<button id="change">Change</button><span id="status">Loading</span><span id="busy">Busy</span><span id="gone">Remove</span>
<button id="delayed" disabled>Delayed</button><button id="disabled" disabled>Disabled</button><input id="value">
<div><button id="confirm">Confirm</button><button id="prompt">Prompt</button><button id="alert">Alert</button><button id="extra">Two dialogs</button><button id="noop">No dialog</button><button id="unload">Arm unload</button></div>
<script>
const body = document.body;
body.dataset.initialTouch = String(navigator.maxTouchPoints);
body.dataset.loads = String(Number(sessionStorage.loads || 0) + 1);
sessionStorage.loads = body.dataset.loads;
const get = id => document.getElementById(id);
get('click').addEventListener('dblclick', e => {
  body.dataset.double = String(e.isTrusted && e.detail === 2 && e.ctrlKey && e.shiftKey);
});
get('click').addEventListener('click', e => { body.dataset.modifiers = String(e.ctrlKey || e.shiftKey || e.altKey || e.metaKey); });
get('click').addEventListener('mousemove', e => {
  if (e.altKey && e.offsetX >= 5 && e.offsetX <= 20) body.dataset.hoverMod = String(e.isTrusted);
});
get('second').addEventListener('blur', () => { body.dataset.blurred = 'true'; });
get('range').addEventListener('change', () => { body.dataset.rangeChanged = get('range').value; });
get('color').addEventListener('change', () => { body.dataset.colorChanged = get('color').value; });
get('otp').addEventListener('paste', e => {
  e.preventDefault();
  body.dataset.customPaste = e.clipboardData.getData('text/plain');
});
document.addEventListener('selectionchange', () => {
  const selected = String(window.getSelection() || '');
  if (selected === 'Beta') body.dataset.selectedDom = 'Beta';
});
get('context').addEventListener('contextmenu', e => { e.preventDefault(); body.dataset.context = String(e.isTrusted && e.button === 2); });
get('aux').addEventListener('auxclick', e => { e.preventDefault(); body.dataset.aux = String(e.isTrusted && e.button === 1); });
get('covered').onclick = e => body.dataset.position = String(e.isTrusted && e.offsetX < 30);
get("odd'id").onclick = e => body.dataset.quoted = String(e.isTrusted);
const typing = [];
get('typed').onkeydown = e => { typing.push({time:e.timeStamp, trusted:e.isTrusted}); body.dataset.typing = JSON.stringify(typing); };
let pointer = false;
get('pointer').onpointerdown = e => { pointer = true; body.dataset.pointerStart = String(e.isTrusted); };
document.addEventListener('pointermove', e => { if (pointer && e.buttons === 1) body.dataset.pointerMoved = String(e.isTrusted); });
document.addEventListener('pointerup', e => { if (pointer) { body.dataset.pointerDrop = String(e.target === get('drop') && e.isTrusted); pointer = false; } });
get('native').ondragstart = e => { e.dataTransfer.setData('text/plain', 'native-payload'); body.dataset.dragStart = String(e.isTrusted); };
get('drop').ondragover = e => e.preventDefault();
get('drop').ondrop = e => { e.preventDefault(); body.dataset.nativeDrop = String(e.isTrusted && e.dataTransfer.getData('text/plain') === 'native-payload'); };
get('files').ondragover = e => e.preventDefault();
get('files').ondrop = async e => {
  e.preventDefault();
  const files = [...e.dataTransfer.files];
  body.dataset.fileDrop = String(e.isTrusted && files.length === 2 && await files[0].text() === 'first-file' && await files[1].text() === 'second-file');
};
get('scroller').onscroll = () => { get('scroller').dataset.moved = String(get('scroller').scrollTop > 0 && get('scroller').scrollLeft > 0); };
get('scroller').addEventListener('wheel', e => { body.dataset.wheelTrusted = String(e.isTrusted); });
let touchStart;
let pinchStartSpan = 0;
get('touch').addEventListener('touchstart', e => {
  touchStart = {time:e.timeStamp, x:e.touches[0].clientX};
  if (e.touches.length === 2) {
    pinchStartSpan = Math.abs(e.touches[1].clientX - e.touches[0].clientX);
  }
  body.dataset.touchStarted = 'true';
});
get('touch').addEventListener('touchmove', e => {
  if (e.touches.length === 2 && pinchStartSpan > 0) {
    const span = Math.abs(e.touches[1].clientX - e.touches[0].clientX);
    if (e.isTrusted && span > pinchStartSpan * 1.5) body.dataset.pinched = 'true';
  } else {
    body.dataset.swipe = String(e.isTrusted && Math.abs(e.touches[0].clientX - touchStart.x) >= 50);
  }
});
get('touch').addEventListener('touchend', e => {
  body.dataset.touchTrusted = String(e.isTrusted);
  body.dataset.touchDuration = String(e.timeStamp - touchStart.time);
});
get('touch').addEventListener('touchcancel', () => body.dataset.touchCancelled = 'true');
get('change').onclick = () => setTimeout(() => {
  get('status').textContent = '  Ready   now ';
  get('busy').style.display = 'none'; get('gone').remove();
  get('delayed').disabled = false; get('value').value = 'ready';
}, 150);
get('confirm').onclick = () => body.dataset.confirm = String(confirm('Continue?'));
get('prompt').onclick = () => body.dataset.prompt = String(prompt('Name?'));
get('alert').onclick = () => { alert('Notice'); body.dataset.alert = 'handled'; };
get('extra').onclick = () => { confirm('First'); confirm('Second'); };
get('unload').onclick = () => window.onbeforeunload = () => '';
</script></body></html>"#)
        .with_header(Header::from_bytes("Content-Type", "text/html; charset=utf-8").unwrap()));
}

#[tokio::test]
#[ignore = "requires managed Chromium; isolated child owns fixture paths"]
async fn expanded_actions_dispatch_trusted_events_and_preserve_frames() -> Result<()> {
    use super::{
        runtime::Runtime,
        storage_state::StorageState,
        test_action::{ActionExecutionContext, exec_test_action},
    };
    use crate::{
        config::ResolvedConfig,
        types::{Frame, TestAction, TestScenario, TestSuite},
    };
    use serde_json::{Value, json};
    let root = match std::env::var_os("REGRESSIFY_EXPANDED_ACTION_FIXTURE") {
        Some(root) => PathBuf::from(root),
        None => {
            let workspace = tempfile::tempdir()?;
            let output = std::process::Command::new(std::env::current_exe()?)
                .args(["--exact", "test::browser_tests::expanded_actions_dispatch_trusted_events_and_preserve_frames", "--ignored", "--nocapture"])
                .env("REGRESSIFY_EXPANDED_ACTION_FIXTURE", workspace.path()).output()?;
            anyhow::ensure!(
                output.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return Ok(());
        }
    };
    crate::common::ROOT_DIR
        .set(root.clone())
        .map_err(|_| anyhow!("Fixture root already initialized"))?;
    std::fs::create_dir_all(root.join("visual_tests").join("uploads"))?;
    std::fs::write(
        root.join("visual_tests").join("uploads").join("first.txt"),
        "first-file",
    )?;
    std::fs::write(
        root.join("visual_tests").join("uploads").join("second.txt"),
        "second-file",
    )?;
    let files = ["first.txt", "second.txt"].map(|name| {
        PathBuf::from("uploads")
            .join(name)
            .to_string_lossy()
            .into_owned()
    });
    let outer = Site::new(expanded_action_page)?;
    let inner = Site::new(expanded_action_page)?;
    let (mut browser, handler, directory) = launch_browser(
        "expanded-actions",
        LaunchOptions {
            has_touch: true,
            viewport: Some((1100, 900)),
            ..Default::default()
        },
    )
    .await?;
    let result: Result<()> = async {
        let page = browser.new_page("about:blank").await?;
        let runtime = Runtime::new(&browser, &page).await?;
        let storage = Arc::new(StorageState::install_with_client(runtime.client(), &page, None, &outer.origin).await?);
        let touch_config = ResolvedConfig { element_timeout_ms: 4000, ..Default::default() };
        let touch_tabs = super::tabs::Tabs::install(&page, storage.clone(), &touch_config, None, true).await?;
        page.goto(outer.origin.as_str()).await?;
        let child = inner.origin.replace("127.0.0.1", "localhost");
        super::runtime::evaluate_main_document(&page, format!("(() => {{ const frame = document.createElement('iframe'); frame.id = 'frame'; frame.width = 1000; frame.height = 600; frame.src = {}; document.body.append(frame); }})()", serde_json::to_string(&child)?)).await?;
        let child_target = runtime.target(&Some(Frame::Single("#frame".into())), Duration::from_secs(4)).await?;
        super::actionability::wait_for_selector(&child_target, "body[data-initial-touch]", &None, &super::actionability::ActionabilityConfig { element_timeout_ms: 4000, ..Default::default() }, crate::diagnostics::record::Stage::Navigation).await?;
        child_target.evaluate(format!("(() => {{ const frame = document.createElement('iframe'); frame.id = 'nested'; frame.width = 950; frame.height = 580; frame.src = {}; document.body.append(frame); }})()", serde_json::to_string(&outer.origin)?)).await?;
        let suite = TestSuite::default();
        let scenario = TestScenario { url: outer.origin.clone(), ..Default::default() };
        let config = ResolvedConfig { element_timeout_ms: 4000, actionability_timeout_ms: 2000, ..Default::default() };
        let context = || ActionExecutionContext { page: &page, runtime: &runtime, storage: &storage, test_suite: &suite, scenario: &scenario, config: &config, url_replacements: &[], viewport_label: None, log_prefix: "expanded" };
        for frame in [None, Some(Frame::Single("#frame".into())), Some(Frame::Multiple(vec!["#frame".into(), "#nested".into()]))] {
            touch_tabs.check().await?;
            for mut value in [
                json!({"click":"#click","clickCount":2,"modifiers":["Control","Shift"]}),
                json!({"click":"#context","button":"right"}),
                json!({"click":"#aux","button":"middle"}),
                json!({"click":"#covered","position":{"x":10,"y":10}}),
                json!({"click":"[id=\"odd'id\"]"}),
                json!({"input":"#typed","value":"abc","typingDelay":35}),
                json!({"focus":"#first"}),
                json!({"key":"Tab"}),
                json!({"key":"x"}),
                json!({"drag":"#pointer","to":"#drop","mode":"pointer","duration":120}),
                json!({"drag":"#native","to":"#drop","duration":180}),
                json!({"dropFiles":"#files","file":files}),
                json!({"wait":"body[data-file-drop=true]"}),
                json!({"wheel":"#scroller","deltaY":100,"deltaX":50}),
                json!({"wait":"#scroller[data-moved=true]"}),
                json!({"scroll":"#scroller","position":{"x":20,"y":40}}),
                json!({"scroll":"#scroller","offset":{"x":5,"y":10}}),
                json!({"hover":"#click","position":{"x":10,"y":10},"modifiers":["Alt"]}),
                json!({"input":"#range","value":"75"}),
                json!({"input":"#color","value":"#ff00aa"}),
                json!({"paste":"#otp","value":"654321"}),
                json!({"paste":"#first","value":"hello"}),
                json!({"paste":"#first","value":" world","append":true}),
                json!({"selectText":"#first","start":0,"end":5}),
                json!({"selectText":"#copy","text":"Beta"}),
                json!({"focus":"#second"}),
                json!({"blur":"#second"}),
                json!({"focus":"#first"}),
                json!({"blur":"active"}),
                json!({"tap":"#touch"}),
                json!({"longPress":"#touch","duration":600}),
            ] {
                value["frame"] = json!(frame);
                let action: TestAction = serde_json::from_value(value.clone())?;
                exec_test_action(context(), &action).await.with_context(|| format!("expanded action {}", value.as_object().unwrap().keys().filter(|key| *key != "frame").cloned().collect::<Vec<_>>().join(",")))?;
            }
            let target = runtime.target(&frame, Duration::from_secs(4)).await?;
            exec_test_action(context(), &serde_json::from_value(json!({"dialog":"accept","kind":"confirm"}))?).await?;
            exec_test_action(context(), &serde_json::from_value(json!({"click":"#confirm","frame":frame}))?).await?;
            let values: Value = target.evaluate("({...document.body.dataset, first:document.querySelector('#first').value, second:document.querySelector('#second').value, typed:document.querySelector('#typed').value, scrollLeft:document.querySelector('#scroller').scrollLeft, scrollTop:document.querySelector('#scroller').scrollTop})").await?.into_value()?;
            for name in ["double", "context", "aux", "position", "quoted", "pointerStart", "pointerMoved", "pointerDrop", "dragStart", "nativeDrop", "fileDrop", "wheelTrusted", "touchTrusted", "confirm", "hoverMod", "blurred"] {
                anyhow::ensure!(values[name] == "true", "Missing trusted interaction result ({}): {name} = {}", json!(frame), values[name]);
            }
            anyhow::ensure!(values["first"] == "hello world" && values["second"] == "x" && values["typed"] == "abc", "Focused keys, paste, or typing changed the wrong field");
            anyhow::ensure!(values["rangeChanged"] == "75" && values["colorChanged"] == "#ff00aa", "Native range/color input values were not updated");
            anyhow::ensure!(values["customPaste"] == "654321" && values["selectedDom"] == "Beta", "Clipboard paste or DOM text selection did not run");
            anyhow::ensure!(values["scrollLeft"] == 25 && values["scrollTop"] == 50, "Scroll container position/offset was not applied: {:?}", values);
            anyhow::ensure!(values["initialTouch"].as_str().unwrap().parse::<u64>()? > 0, "Touch was not enabled before document scripts");
            anyhow::ensure!(values["touchDuration"].as_str().unwrap().parse::<f64>()? >= 550.0, "Long press released before its duration");
            let typing: Vec<Value> = serde_json::from_str(values["typing"].as_str().unwrap())?;
            let characters = &typing[typing.len() - 3..];
            anyhow::ensure!(characters.iter().all(|key| key["trusted"] == true), "Typing used synthetic key events");
            anyhow::ensure!(characters.windows(2).all(|pair| pair[1]["time"].as_f64().unwrap() - pair[0]["time"].as_f64().unwrap() >= 30.0), "typingDelay did not separate character events");
            for value in [
                json!({"swipe":"#touch","position":{"x":30,"y":30},"offset":{"x":70,"y":0},"duration":160,"frame":frame}),
                json!({"pinch":"#touch","scale":2.0,"duration":160,"frame":frame}),
                json!({"click":"#change","frame":frame}),
                json!({"wait":"#status","text":"Ready now","frame":frame}),
                json!({"wait":"#status","containsText":"Ready","frame":frame}),
                json!({"wait":"#busy","state":"hidden","frame":frame}),
                json!({"wait":"#gone","state":"detached","frame":frame}),
                json!({"wait":"#delayed","state":"enabled","frame":frame}),
                json!({"wait":"#disabled","state":"disabled","frame":frame}),
                json!({"wait":"#value","value":"ready","frame":frame}),
                json!({"wait":"#busy","state":"attached","frame":frame}),
            ] {
                exec_test_action(context(), &serde_json::from_value(value)?).await?;
            }
            let swiped: String = target.evaluate("document.body.dataset.swipe").await?.into_value()?;
            anyhow::ensure!(swiped == "true", "Swipe did not move a trusted touch point");
            let pinched: String = target.evaluate("document.body.dataset.pinched").await?.into_value()?;
            anyhow::ensure!(pinched == "true", "Pinch did not move two trusted touch points");
            let cancelling: TestAction = serde_json::from_value(json!({"longPress":"#touch","duration":5000,"frame":frame}))?;
            anyhow::ensure!(tokio::time::timeout(Duration::from_millis(800), exec_test_action(context(), &cancelling)).await.is_err(), "Cancellation fixture did not interrupt the hold");
            exec_test_action(context(), &serde_json::from_value(json!({"wait":"body[data-touch-cancelled=true]","frame":frame}))?).await?;
            exec_test_action(context(), &serde_json::from_value(json!({"click":"#click","frame":frame}))?).await?;
            let released: String = target.evaluate("document.body.dataset.modifiers").await?.into_value()?;
            anyhow::ensure!(released == "false", "Gesture modifiers remained held");
        }
        let short = ResolvedConfig { element_timeout_ms: 500, actionability_timeout_ms: 300, ..Default::default() };
        for value in [
            json!({"click":"#covered","position":{"x":500,"y":10}}),
            json!({"tap":"#disabled"}),
            json!({"wheel":"#missing","deltaY":10}),
            json!({"swipe":"#touch","offset":{"x":10000,"y":0}}),
            json!({"wait":"#status","text":"never"}),
            json!({"wait":"#busy","state":"visible"}),
            json!({"blur":"#second"}),
            json!({"selectText":"#copy","text":"Nonexistent"}),
            json!({"input":"#range","value":"250"}),
        ] {
            let context = ActionExecutionContext { config: &short, ..context() };
            anyhow::ensure!(exec_test_action(context, &serde_json::from_value(value)?).await.is_err(), "Invalid/unsatisfied interaction succeeded");
        }
        Ok(())
    }.await;
    browser.close().await?;
    tokio::time::timeout(Duration::from_secs(10), browser.wait()).await??;
    handler.await?;
    drop(browser);
    let cleanup = std::fs::remove_dir_all(directory);
    result?;
    cleanup?;
    Ok(())
}

#[tokio::test]
#[ignore = "requires managed Chromium; loopback history and real JavaScript dialogs"]
async fn expanded_actions_dialogs_and_history_are_strict() -> Result<()> {
    use super::{
        runtime::Runtime,
        storage_state::StorageState,
        test_action::{ActionExecutionContext, exec_test_action},
    };
    use crate::{
        config::ResolvedConfig,
        types::{TestAction, TestScenario, TestSuite},
    };
    use serde_json::{Value, json};
    let site = Site::new(expanded_action_page)?;
    let (mut browser, handler, directory) =
        launch_browser("expanded-dialogs", Default::default()).await?;
    let result: Result<()> = async {
        let page = browser.new_page("about:blank").await?;
        let runtime = Runtime::new(&browser, &page).await?;
        let storage = StorageState::install(&browser, &page, None, &site.origin).await?;
        let suite = TestSuite::default();
        let scenario = TestScenario {
            url: site.origin.clone(),
            ..Default::default()
        };
        let config = ResolvedConfig {
            element_timeout_ms: 2500,
            actionability_timeout_ms: 1500,
            ..Default::default()
        };
        let context = || ActionExecutionContext {
            page: &page,
            runtime: &runtime,
            storage: &storage,
            test_suite: &suite,
            scenario: &scenario,
            config: &config,
            url_replacements: &[],
            viewport_label: None,
            log_prefix: "dialogs",
        };
        let back: TestAction = serde_json::from_value(json!({"navigate":"back"}))?;
        anyhow::ensure!(
            exec_test_action(context(), &back).await.is_err(),
            "Missing history must fail"
        );
        page.goto(format!("{}/one", site.origin)).await?;
        for value in [
            json!({"dialog":"accept","kind":"confirm","message":"Continue?"}),
            json!({"click":"#confirm"}),
            json!({"dialog":"accept","kind":"prompt","promptText":"fixture response"}),
            json!({"click":"#prompt"}),
            json!({"dialog":"dismiss","kind":"alert","message":"Notice"}),
            json!({"click":"#alert"}),
        ] {
            exec_test_action(context(), &serde_json::from_value(value)?).await?;
        }
        let target = runtime.target(&None, Duration::from_secs(3)).await?;
        let values: Value = target
            .evaluate("({...document.body.dataset})")
            .await?
            .into_value()?;
        anyhow::ensure!(
            values["confirm"] == "true"
                && values["prompt"] == "fixture response"
                && values["alert"] == "handled",
            "Dialog outcomes were incorrect"
        );
        for (expectation, trigger) in [
            (
                json!({"dialog":"accept","kind":"prompt"}),
                json!({"click":"#confirm"}),
            ),
            (
                json!({"dialog":"accept","kind":"confirm","message":"Wrong message"}),
                json!({"click":"#confirm"}),
            ),
            (
                json!({"dialog":"accept","kind":"confirm"}),
                json!({"click":"#extra"}),
            ),
            (
                json!({"dialog":"accept","kind":"confirm"}),
                json!({"click":"#noop"}),
            ),
        ] {
            exec_test_action(context(), &serde_json::from_value(expectation)?).await?;
            anyhow::ensure!(
                tokio::time::timeout(
                    Duration::from_secs(6),
                    exec_test_action(context(), &serde_json::from_value(trigger)?)
                )
                .await?
                .is_err(),
                "Unmet/extra dialog expectation succeeded"
            );
        }
        for value in [
            json!({"goto":format!("{}/two", site.origin)}),
            json!({"navigate":"back"}),
            json!({"wait":0,"url":format!("{}/one", site.origin)}),
            json!({"navigate":"forward"}),
            json!({"wait":0,"url":format!("{}/two", site.origin)}),
            json!({"click":"#unload"}),
            json!({"dialog":"dismiss","kind":"beforeunload"}),
            json!({"goto":format!("{}/cancelled", site.origin)}),
            json!({"wait":0,"url":format!("{}/two", site.origin)}),
            json!({"dialog":"accept","kind":"beforeunload"}),
            json!({"navigate":"reload","ignoreCache":true}),
            json!({"wait":"#click","url":format!("{}/two", site.origin)}),
        ] {
            exec_test_action(context(), &serde_json::from_value(value)?).await?;
        }
        runtime.ensure_no_pending_dialog()?;
        Ok(())
    }
    .await;
    browser.close().await?;
    tokio::time::timeout(Duration::from_secs(10), browser.wait()).await??;
    handler.await?;
    drop(browser);
    let cleanup = std::fs::remove_dir_all(directory);
    result?;
    cleanup?;
    Ok(())
}

#[tokio::test]
#[ignore = "requires managed Chromium; run regressify install"]
async fn reference_and_test_inject_ready_scripts_without_node() -> Result<()> {
    use crate::{test::capture, types::ScreenshotType};
    use clap::Parser;
    use serde_json::{Value, json};

    if let Some(root) = std::env::var_os("REGRESSIFY_BROWSER_HOOK_FIXTURE") {
        let mode = std::env::var("REGRESSIFY_BROWSER_HOOK_MODE")?;
        let command = if mode == "test" { "test" } else { "ref" };
        let arguments: Vec<std::ffi::OsString> = vec![
            "regressify".into(),
            "--root".into(),
            root,
            "--run-id".into(),
            mode.into(),
            "--async-capture-limit".into(),
            "2".into(),
            "--async-capture-ramp-ms".into(),
            "0".into(),
            command.into(),
            "--fetch-browser".into(),
            "false".into(),
        ];
        return crate::application::execute(crate::cli::Cli::try_parse_from(arguments)?).await;
    }
    let site = Site::new(|request| {
        let _ = request.respond(Response::from_string(r#"<!doctype html>
<html><head><script>window.beforeObserved = window.beforeCalls === 1;</script>
<style>body { margin:0; min-height:100vh; background:rgb(200,30,30); }
#cookie-banner { position:fixed; inset:0; z-index:1000; background:white; }
button { margin:30px; } .volatile { position:fixed; inset:0; background:red; }</style></head>
<body><div class="volatile">Changing data</div><div id="cookie-banner">Cookie banner</div>
<button id="target" onclick="
const context = window.regressify;
const id = context.scenario.id;
const expectedShared = id === 'scenario-ready' ? 0 : 1;
const expectedReady = id === 'js-path' ? 0 : 1;
if (window.beforeObserved && (window.sharedCalls || 0) === expectedShared &&
    (window.readyCalls || 0) === expectedReady && !document.querySelector('#cookie-banner') &&
    getComputedStyle(document.querySelector('.volatile')).visibility === 'hidden' &&
    context.viewport.label === 'desktop' && typeof context.isReference === 'boolean' &&
    !('actions' in context.scenario) && !('basicAuth' in context.scenario) &&
    !('basicAuth' in context.config) && typeof process === 'undefined' && typeof require === 'undefined') {
    document.body.style.background = 'rgb(14,160,80)';
}">Continue</button></body></html>"#)
            .with_header(Header::from_bytes("Content-Type", "text/html").unwrap()));
    })?;
    let workspace = tempfile::tempdir()?;
    let root = workspace.path();
    std::fs::create_dir(root.join("visual_tests"))?;
    std::fs::create_dir(root.join("common"))?;
    std::fs::write(
        root.join("common/_viewports.yaml"),
        "- label: desktop\n  width: 640\n  height: 240\n",
    )?;
    std::fs::write(
        root.join("common/_override.css"),
        ".volatile { visibility: hidden !important; }",
    )?;
    std::fs::write(
        root.join("common/before.js"),
        "window.beforeCalls = (window.beforeCalls || 0) + 1;",
    )?;
    for (path, counter) in [("shared.js", "sharedCalls"), ("ready.js", "readyCalls")] {
        std::fs::write(
            root.join("common").join(path),
            format!(
                "(async () => {{ await new Promise(resolve => setTimeout(resolve, 25)); window.{counter} = (window.{counter} || 0) + 1; document.querySelector('#cookie-banner')?.remove(); }})()"
            ),
        )?;
    }
    std::fs::write(
        root.join("regressify.yaml"),
        "elementTimeout: 1500\npostInteractionWait: 0\n",
    )?;
    let mut scenarios = Vec::new();
    for id in ["js-path", "scenario-ready", "both"] {
        let mut scenario = json!({
            "id": id, "url": site.origin, "readySelector": "#target",
            "onBeforeScript": "common/before.js", "delay": 0, "noScrollTop": true,
            "selectors": ["viewport"], "actions": [{"click": "#target"}]
        });
        if id != "scenario-ready" {
            scenario["jsOnReadyPath"] = json!("common/shared.js");
        }
        if id != "js-path" {
            scenario["onReadyScript"] = json!("common/ready.js");
        }
        scenarios.push(scenario);
    }
    std::fs::write(
        root.join("visual_tests/hooks.tests.json"),
        serde_json::to_vec_pretty(&json!({"id": "hooks", "scenarios": scenarios}))?,
    )?;
    let run = |mode: &str| -> Result<()> {
        let output = std::process::Command::new(std::env::current_exe()?)
            .args([
                "--ignored",
                "--exact",
                "test::browser_tests::reference_and_test_inject_ready_scripts_without_node",
                "--nocapture",
            ])
            .env("REGRESSIFY_BROWSER_HOOK_FIXTURE", root)
            .env("REGRESSIFY_BROWSER_HOOK_MODE", mode)
            .env("PATH", "")
            .env_remove("NODE_PATH")
            .env_remove("REPLACEMENT_PROFILE")
            .output()?;
        anyhow::ensure!(
            output.status.success(),
            "{mode}: {}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        Ok(())
    };
    run("reference")?;
    run("test")?;
    let suite_dir = root.join(".regressify/hooks");
    let mut baselines = Vec::new();
    for id in ["js-path", "scenario-ready", "both"] {
        let key =
            capture::final_screenshot_order_key(id, Some("desktop"), &ScreenshotType::Viewport);
        let baseline = suite_dir
            .join("bitmaps_reference")
            .join(format!("{key}.png"));
        baselines.push((baseline.clone(), std::fs::read(&baseline)?));
        for mode in ["reference", "test"] {
            let path = suite_dir
                .join("bitmaps_test")
                .join(mode)
                .join(format!("{key}.png"));
            anyhow::ensure!(
                !capture::error_marker_path(&path).exists(),
                "{mode}/{id} failed"
            );
            let image = lodepng::decode32(std::fs::read(path)?)?;
            let pixel = image.buffer.first().unwrap();
            anyhow::ensure!(
                (pixel.r, pixel.g, pixel.b) == (14, 160, 80),
                "{mode}/{id}: banner dismissal or hook order failed"
            );
        }
    }
    std::fs::write(
        root.join("common/ready.js"),
        "throw new Error('ready-hook-fixture-error')",
    )?;
    run("failed-reference")?;
    let report: Value = serde_json::from_slice(&std::fs::read(
        suite_dir.join("bitmaps_test/failed-reference/report.json"),
    )?)?;
    let tests = report["tests"].as_array().unwrap();
    assert_eq!(
        tests.iter().filter(|test| test["status"] == "fail").count(),
        2
    );
    assert_eq!(
        tests.iter().filter(|test| test["status"] == "pass").count(),
        1
    );
    for (path, bytes) in baselines {
        assert_eq!(
            std::fs::read(path)?,
            bytes,
            "a failing hook changed a reference"
        );
    }
    Ok(())
}

#[tokio::test]
#[ignore = "requires managed Chromium; run regressify install"]
async fn responsive_hamburger_menu_mobile_only_click_and_save() -> Result<()> {
    use crate::{
        diagnostics::{
            journal::{ArchiveState, JournalReader},
            record::{Field, FieldValue, SafeIdentity},
        },
        test::capture,
        types::ScreenshotType,
    };
    use clap::Parser;
    use serde_json::{Value, json};
    use std::{collections::BTreeMap, sync::Mutex};

    if let Some(root) = std::env::var_os("REGRESSIFY_RESPONSIVE_VIEWPORT_FIXTURE") {
        let arguments: Vec<std::ffi::OsString> = vec![
            "regressify".into(),
            "--root".into(),
            root,
            "--run-id".into(),
            "responsive".into(),
            "--async-capture-limit".into(),
            "2".into(),
            "--async-capture-ramp-ms".into(),
            "0".into(),
            "test".into(),
            "--fetch-browser".into(),
            "false".into(),
            "--skip-compare".into(),
        ];
        return crate::application::execute(crate::cli::Cli::try_parse_from(arguments)?).await;
    }
    let observed = Arc::new(Mutex::new(BTreeMap::<String, Value>::new()));
    let received = observed.clone();
    let site = Site::new(move |request| {
        let url = reqwest::Url::parse(&format!("http://fixture{}", request.url())).unwrap();
        if url.path() == "/observed" {
            let values = url.query_pairs().into_owned().collect::<BTreeMap<_, _>>();
            received.lock().unwrap().insert(
                values["viewport"].clone(),
                serde_json::from_str(&values["state"]).unwrap(),
            );
            let _ = request.respond(Response::empty(204));
            return;
        }
        let _ = request.respond(Response::from_string(r#"<!doctype html>
<html><head><style>
body { margin: 0; min-height: 100vh; background: rgb(200, 30, 30); }
button { position: absolute; left: 24px; top: 24px; }
#products-menu { top: 64px; } #target { top: 104px; }
</style></head><body>
<button id="products-menu">Products</button><button id="target" hidden>Widget</button>
<script>
const mobile = innerWidth < 600;
const path = [];
const products = document.getElementById('products-menu');
const target = document.getElementById('target');
products.hidden = mobile;
if (mobile) {
    const menu = document.createElement('button');
    menu.id = 'menu-toggle';
    menu.textContent = 'Menu';
    menu.onclick = () => {
        path.push('menu');
        products.hidden = false;
        document.body.style.background = 'rgb(40, 90, 200)';
    };
    document.body.appendChild(menu);
}
products.onclick = () => { path.push('products'); target.hidden = false; };
target.onclick = async () => {
    path.push('target');
    const state = { path, menuExists: !!document.getElementById('menu-toggle') };
    await fetch('/observed?viewport=' + (mobile ? 'mobile' : 'desktop') + '&state=' + encodeURIComponent(JSON.stringify(state)));
    document.body.style.background = 'rgb(14, 160, 80)';
};
</script></body></html>"#).with_header(Header::from_bytes("Content-Type", "text/html").unwrap()));
    })?;
    let workspace = tempfile::tempdir()?;
    let root = workspace.path();
    std::fs::create_dir_all(root.join("visual_tests"))?;
    std::fs::create_dir_all(root.join("common"))?;
    std::fs::write(
        root.join("common/_viewports.yaml"),
        "- label: desktop\n  width: 960\n  height: 300\n- label: mobile\n  width: 390\n  height: 300\n",
    )?;
    std::fs::write(root.join("regressify.yaml"), "elementTimeout: 1500\n")?;
    let suite = json!({ "scenarios": [{
        "id": "products-submenu", "url": site.origin, "viewportNames": ["desktop", "mobile"],
        "delay": 0, "noScrollTop": true, "selectors": ["viewport"],
        "actions": [
            { "click": "#menu-toggle", "viewportNames": "mobile" },
            { "save": "menu-open", "viewportNames": ["mobile"] },
            { "click": "#products-menu" },
            { "click": "#target" }
        ]
    }] });
    std::fs::write(
        root.join("visual_tests/responsive.tests.json"),
        serde_json::to_vec_pretty(&suite)?,
    )?;
    let output = std::process::Command::new(std::env::current_exe()?)
        .args([
            "--ignored",
            "--exact",
            "test::browser_tests::responsive_hamburger_menu_mobile_only_click_and_save",
            "--nocapture",
        ])
        .env("REGRESSIFY_RESPONSIVE_VIEWPORT_FIXTURE", root)
        .env_remove("REPLACEMENT_PROFILE")
        .env_remove("REGRESSIFY_DISABLE_DIAGNOSTIC_ARCHIVE")
        .env_remove("REGRESSIFY_DIAGNOSTIC_DISK_BUDGET_BYTES")
        .output()?;
    anyhow::ensure!(
        output.status.success(),
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        observed.lock().unwrap().get("desktop"),
        Some(&json!({ "path": ["products", "target"], "menuExists": false }))
    );
    assert_eq!(
        observed.lock().unwrap().get("mobile"),
        Some(&json!({ "path": ["menu", "products", "target"], "menuExists": true }))
    );
    let output_dir = root.join(".regressify");
    let run = output_dir.join("responsive/bitmaps_test/responsive");
    let mut expected = Vec::new();
    for (label, width) in [("desktop", 960), ("mobile", 390)] {
        let checkpoint_key = capture::final_screenshot_order_key(
            "products-submenu",
            Some(label),
            &ScreenshotType::Step("menu-open".into()),
        );
        let checkpoint = run.join(format!("{checkpoint_key}.png"));
        if label == "mobile" {
            expected.push(checkpoint_key);
            let image = lodepng::decode32(std::fs::read(&checkpoint)?)?;
            let pixel = image.buffer.first().unwrap();
            assert_eq!((pixel.r, pixel.g, pixel.b), (40, 90, 200));
            assert!(!capture::error_marker_path(&checkpoint).exists());
        } else {
            assert!(!checkpoint.exists());
            assert!(!capture::error_marker_path(&checkpoint).exists());
        }
        let final_key = capture::final_screenshot_order_key(
            "products-submenu",
            Some(label),
            &ScreenshotType::Viewport,
        );
        let final_image = run.join(format!("{final_key}.png"));
        expected.push(final_key);
        let image = lodepng::decode32(std::fs::read(&final_image)?)?;
        assert_eq!((image.width, image.height), (width, 300));
        let pixel = image.buffer.first().unwrap();
        assert_eq!((pixel.r, pixel.g, pixel.b), (14, 160, 80), "{label}");
        assert!(!capture::error_marker_path(&final_image).exists());
        let log = capture::read_capture_log(&output_dir, &final_image).unwrap();
        assert_eq!(log.state, ArchiveState::Complete);
        let archive = log.archive.unwrap();
        let reader = JournalReader::open(&output_dir, &archive.operation_id)?;
        let menu_target = FieldValue::Identity(SafeIdentity::source("#menu-toggle"));
        let mut menu_dispatched = false;
        let mut execution_seen = false;
        reader.visit(|record| {
            if let Some(execution) = &record.execution {
                execution_seen = true;
                assert_eq!(execution.suite_id.as_str(), "responsive");
                assert_eq!(execution.scenario_id.as_str(), "products-submenu");
                assert_eq!(execution.viewport.name.as_str(), label);
            }
            for value in record.fields.values() {
                if let FieldValue::Identity(source) = value {
                    assert!(!source.as_str().starts_with("sha256:"));
                }
            }
            menu_dispatched |= record.fields.get(&Field::Target) == Some(&menu_target);
        })?;
        assert!(
            execution_seen,
            "capture records must retain readable execution names"
        );
        assert_eq!(
            menu_dispatched,
            label == "mobile",
            "{label}: unexpected menu dispatch"
        );
    }
    let manifest: Value =
        serde_json::from_slice(&std::fs::read(run.join(capture::REPORT_ORDER_FILE))?)?;
    assert_eq!(manifest["captures"], json!(expected));
    assert_eq!(
        crate::compare::report_gen::validate_capture_run_inventory(&run)?.len(),
        3
    );
    Ok(())
}

fn auth_site(
    expected: &'static str,
    credentials_seen: Arc<AtomicBool>,
    redirect: Option<String>,
) -> Result<Site> {
    Site::new(move |request| {
        if request.url() == "/redirect"
            && let Some(location) = &redirect
        {
            let response = Response::from_string("")
                .with_status_code(StatusCode(302))
                .with_header(Header::from_bytes("Location", location.as_str()).unwrap());
            let _ = request.respond(response);
            return;
        }
        let authorization = request
            .headers()
            .iter()
            .find(|header| header.field.equiv("Authorization"));
        if authorization.is_some() {
            credentials_seen.store(true, Ordering::Relaxed);
        }
        let response = if authorization.is_some_and(|header| header.value.as_str() == expected) {
            Response::from_string("<body>authenticated</body>")
        } else {
            Response::from_string("<body>unauthorized</body>")
                .with_status_code(StatusCode(401))
                .with_header(
                    Header::from_bytes("WWW-Authenticate", "Basic realm=\"fixture\"").unwrap(),
                )
        };
        let _ =
            request.respond(response.with_header(
                Header::from_bytes("Content-Type", "text/html; charset=utf-8").unwrap(),
            ));
    })
}

#[tokio::test]
#[ignore = "requires managed Chromium; run regressify install"]
async fn basic_auth_is_origin_scoped_and_cancels_bad_credentials() -> Result<()> {
    let allowed = Arc::new(AtomicBool::new(false));
    let other_allowed = Arc::new(AtomicBool::new(false));
    let forbidden = Arc::new(AtomicBool::new(false));
    let third = auth_site("Basic dXNlcjpwYXNz", forbidden.clone(), None)?;
    let first = auth_site(
        "Basic dXNlcjpwYXNz",
        allowed.clone(),
        Some(third.origin.clone()),
    )?;
    let second = auth_site("Basic b3RoZXI6cGFzcw==", other_allowed.clone(), None)?;
    let (mut browser, handler, directory) =
        launch_browser("fixture", LaunchOptions::default()).await?;
    let result: Result<()> = async {
        let page = browser.new_page("about:blank").await?;
        let entries = [
            BasicAuth {
                origin: first.origin.clone(),
                username: "user".into(),
                password: "pass".into(),
            },
            BasicAuth {
                origin: second.origin.clone(),
                username: "other".into(),
                password: "pass".into(),
            },
        ];
        let authentication = Authentication::install(&browser, &page, &entries)
            .await?
            .unwrap();
        for site in [&first, &second] {
            page.goto(site.origin.as_str()).await?;
            let text: String = page
                .evaluate("document.body.textContent")
                .await?
                .into_value()?;
            anyhow::ensure!(
                text == "authenticated",
                "unexpected authentication response: {text}"
            );
        }
        let _ =
            tokio::time::timeout(Duration::from_secs(10), page.goto(third.origin.as_str())).await?;
        let _ = tokio::time::timeout(
            Duration::from_secs(10),
            page.goto(format!("{}/redirect", first.origin)),
        )
        .await?;
        anyhow::ensure!(
            !forbidden.load(Ordering::Relaxed),
            "credentials leaked to an unconfigured origin"
        );
        anyhow::ensure!(
            allowed.load(Ordering::Relaxed),
            "first origin did not authenticate"
        );
        anyhow::ensure!(
            other_allowed.load(Ordering::Relaxed),
            "second origin did not authenticate"
        );
        authentication.check()?;
        let wrong_page = browser.new_page("about:blank").await?;
        let _wrong_authentication = Authentication::install(
            &browser,
            &wrong_page,
            &[BasicAuth {
                origin: third.origin.clone(),
                username: "wrong".into(),
                password: "wrong".into(),
            }],
        )
        .await?;
        let _ = tokio::time::timeout(
            Duration::from_secs(10),
            wrong_page.goto(third.origin.as_str()),
        )
        .await?;
        Ok(())
    }
    .await;
    browser.close().await?;
    handler.await?;
    let _ = std::fs::remove_dir_all(directory);
    result
}

fn interaction_page(request: Request) {
    let content = match request.url() {
        "/done" => "<body>done<script src='/slow.js'></script></body>",
        "/slow.js" => {
            std::thread::sleep(Duration::from_millis(150));
            "document.body.dataset.loaded = 'yes';"
        }
        _ => {
            r#"<input id="typed" maxlength="3"><select id="choice"><option value="old">Old</option></select>
            <a id="next" href="/done">Next</a><script>
            const typed = document.querySelector('#typed');
            const keys = [];
            typed.addEventListener('keydown', event => { keys.push(event.key); typed.dataset.keys = JSON.stringify(keys); typed.dataset.trusted = String(event.isTrusted); });
            setTimeout(() => document.querySelector('#choice').add(new Option('Ready', 'ready')), 600);
            </script>"#
        }
    };
    let mime = if request.url() == "/slow.js" {
        "application/javascript"
    } else {
        "text/html"
    };
    let _ = request.respond(
        Response::from_string(content)
            .with_header(Header::from_bytes("Content-Type", mime).unwrap()),
    );
}

#[tokio::test]
#[ignore = "requires managed Chromium; run regressify install"]
async fn navigation_default_allows_slow_document_responses() -> Result<()> {
    let site = Site::new(|request| {
        match request.url() {
            "/slow" => std::thread::sleep(Duration::from_secs(31)),
            "/timeout" => std::thread::sleep(Duration::from_millis(250)),
            _ => {}
        }
        let _ = request.respond(
            Response::from_string("<body>ready</body>")
                .with_header(Header::from_bytes("Content-Type", "text/html").unwrap()),
        );
    })?;
    let (mut browser, handler, directory) =
        launch_browser("navigation-timeout", LaunchOptions::default()).await?;
    let result: Result<()> = async {
        let page = browser.new_page("about:blank").await?;
        let runtime = super::runtime::Runtime::new(&browser, &page).await?;
        let target = runtime.target(&None, Duration::from_secs(5)).await?;
        target
            .navigate(&format!("{}/slow", site.origin), &Default::default())
            .await?;
        let loaded: bool = target
            .evaluate("document.readyState === 'complete'")
            .await?
            .into_value()?;
        anyhow::ensure!(loaded, "navigation returned before the document loaded");

        let error = target
            .navigate(
                &format!("{}/timeout", site.origin),
                &crate::types::GotoParameters {
                    timeout: Some(25),
                    ..Default::default()
                },
            )
            .await
            .expect_err("explicit navigation timeout must remain bounded");
        let message = error.to_string();
        anyhow::ensure!(
            message == "Navigation exceeded 25ms"
                || message == "Timed out waiting for CDP Page.navigate",
            "unexpected navigation timeout error: {message}"
        );
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
async fn navigation_options_wait_for_load_and_network_idle() -> Result<()> {
    let site = Site::new(interaction_page)?;
    let (mut browser, handler, directory) =
        launch_browser("navigation-options", LaunchOptions::default()).await?;
    let result: Result<()> = async {
        let page = browser.new_page("about:blank").await?;
        let runtime = super::runtime::Runtime::new(&browser, &page).await?;
        let target = runtime.target(&None, Duration::from_secs(5)).await?;
        for wait in [
            crate::types::NavigationWait::Load,
            crate::types::NavigationWait::NetworkIdle,
        ] {
            target
                .navigate(
                    &format!("{}/done", site.origin),
                    &crate::types::GotoParameters {
                        wait_until: Some(wait),
                        timeout: Some(5000),
                        referer: Some(format!("{}/source", site.origin)),
                    },
                )
                .await?;
            let loaded: bool = target
                .evaluate("document.body.dataset.loaded === 'yes'")
                .await?
                .into_value()?;
            anyhow::ensure!(
                loaded,
                "navigation returned before external script completed"
            );
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
async fn action_keyboard_selection_and_navigation_work_in_frames() -> Result<()> {
    use super::{
        runtime::Runtime,
        storage_state::StorageState,
        test_action::{ActionExecutionContext, exec_test_action},
    };
    use crate::{
        config::ResolvedConfig,
        types::{Frame, TestAction, TestScenario, TestSuite},
    };
    let inner = Site::new(interaction_page)?;
    let outer = Site::new(interaction_page)?;
    let (mut browser, handler, directory) =
        launch_browser("interactions", LaunchOptions::default()).await?;
    let result: Result<()> = async {
        let page = browser.new_page("about:blank").await?;
        let runtime = Runtime::new(&browser, &page).await?;
        let storage = StorageState::install(&browser, &page, None, &outer.origin).await?;
        page.goto(outer.origin.as_str()).await?;
        page.evaluate(format!("(() => {{ const child = document.createElement('iframe'); child.src = {}; document.body.append(child); }})()", serde_json::to_string(&inner.origin.replace("127.0.0.1", "localhost"))?)).await?;
        let suite = TestSuite::default();
        let scenario = TestScenario { url: outer.origin.clone(), ..Default::default() };
        let config = ResolvedConfig { element_timeout_ms: 4000, actionability_timeout_ms: 2000, ..Default::default() };
        let context = || ActionExecutionContext { page: &page, runtime: &runtime, storage: &storage, test_suite: &suite, scenario: &scenario, config: &config, url_replacements: &[], viewport_label: None, log_prefix: "fixture" };
        for frame in [None, Some(Frame::Single("iframe".into()))] {
            for action in [
                serde_json::json!({"select":"#choice", "value":"ready", "frame":frame}),
                serde_json::json!({"input":"#typed", "value":"Ab+c", "append":true, "frame":frame}),
            ] {
                let action: TestAction = serde_json::from_value(action)?;
                exec_test_action(context(), &action).await?;
            }
            let target = runtime.target(&frame, Duration::from_secs(4)).await?;
            let expected_url = if frame.is_some() {
                format!("{}/", inner.origin.replace("127.0.0.1", "localhost"))
            } else {
                format!("{}/", outer.origin)
            };
            anyhow::ensure!(target.url().await?.as_deref() == Some(expected_url.as_str()), "URL read did not use the selected frame");
            let action: TestAction = serde_json::from_value(serde_json::json!({"wait":"#typed", "url":expected_url, "frame":frame}))?;
            exec_test_action(context(), &action).await?;
            let values: serde_json::Value = target.evaluate("(() => { const input = document.querySelector('#typed'); return {value:input.value, keys:JSON.parse(input.dataset.keys), trusted:input.dataset.trusted, choice:document.querySelector('#choice').value}; })()").await?.into_value()?;
            anyhow::ensure!(values["value"] == "Ab+", "maxlength was bypassed: {values}");
            anyhow::ensure!(values["keys"] == serde_json::json!(["A", "b", "+", "c"]), "typing did not dispatch per-character key events: {values}");
            anyhow::ensure!(values["trusted"] == "true" && values["choice"] == "ready", "trusted typing or delayed options failed");
            for key in ["Shift", "ControlOrMeta+A", "Backspace", "+"] {
                let action: TestAction = serde_json::from_value(serde_json::json!({"press":"#typed", "key":key, "frame":frame}))?;
                exec_test_action(context(), &action).await?;
            }
            let value: String = target.evaluate("document.querySelector('#typed').value").await?.into_value()?;
            anyhow::ensure!(value == "+", "keyboard shortcut or literal plus failed: {value}");
        }
        let action: TestAction = serde_json::from_value(serde_json::json!({"click":"#next"}))?;
        exec_test_action(context(), &action).await?;
        let loaded: String = page.evaluate("document.body.dataset.loaded").await?.into_value()?;
        anyhow::ensure!(loaded == "yes", "click completed before the navigation loaded");
        Ok(())
    }.await;
    browser.close().await?;
    handler.await?;
    let _ = std::fs::remove_dir_all(directory);
    result
}

#[tokio::test]
#[ignore = "requires managed Chromium; run regressify install"]
async fn url_waits_follow_delayed_redirects_and_same_document_navigation() -> Result<()> {
    use super::{
        runtime::Runtime,
        storage_state::StorageState,
        test_action::{ActionExecutionContext, exec_test_action},
    };
    use crate::{
        config::ResolvedConfig,
        types::{TestAction, TestScenario, TestSuite},
    };

    let destination = Site::new(|request| {
        let content = if request.url() == "/callback" {
            "<body><script>setTimeout(() => location.replace('/done?flow=login#signed-in'), 150);</script></body>"
        } else {
            "<body><div id='signed-in'>Signed in</div></body>"
        };
        let _ =
            request.respond(Response::from_string(content).with_header(
                Header::from_bytes("Content-Type", "text/html; charset=utf-8").unwrap(),
            ));
    })?;
    let destination_origin = destination.origin.replace("127.0.0.1", "localhost");
    let callback_url = format!("{destination_origin}/callback");
    let source = Site::new(move |request| {
        let response = if request.url() == "/authorize" {
            Response::from_string("")
                .with_status_code(StatusCode(302))
                .with_header(Header::from_bytes("Location", callback_url.as_str()).unwrap())
        } else {
            Response::from_string(
                "<body><button id='login' onclick=\"setTimeout(() => location.assign('/authorize'), 250)\">Log in</button></body>",
            )
        };
        let _ =
            request.respond(response.with_header(
                Header::from_bytes("Content-Type", "text/html; charset=utf-8").unwrap(),
            ));
    })?;
    let (mut browser, handler, directory) =
        launch_browser("url-waits", LaunchOptions::default()).await?;
    let result: Result<()> = async {
        let page = browser.new_page("about:blank").await?;
        let runtime = Runtime::new(&browser, &page).await?;
        let storage = StorageState::install(&browser, &page, None, &source.origin).await?;
        page.goto(source.origin.as_str()).await?;
        let suite = TestSuite::default();
        let scenario = TestScenario {
            url: source.origin.clone(),
            ..Default::default()
        };
        let config = ResolvedConfig {
            element_timeout_ms: 6000,
            actionability_timeout_ms: 2000,
            ..Default::default()
        };
        let context = || ActionExecutionContext {
            page: &page,
            runtime: &runtime,
            storage: &storage,
            test_suite: &suite,
            scenario: &scenario,
            config: &config,
            url_replacements: &[],
            viewport_label: None,
            log_prefix: "fixture",
        };
        for action in [
            serde_json::json!({"click": "#login"}),
            serde_json::json!({
                "wait": "#signed-in",
                "url": format!("{destination_origin}/done?flow=login#signed-in")
            }),
        ] {
            let action: TestAction = serde_json::from_value(action)?;
            exec_test_action(context(), &action).await?;
        }
        let target = runtime.target(&None, Duration::from_secs(5)).await?;
        target
            .evaluate("history.pushState({}, '', '?flow=spa#ready')")
            .await?;
        let action: TestAction = serde_json::from_value(serde_json::json!({
            "wait": "#signed-in",
            "url": format!("{destination_origin}/done?flow=spa#ready")
        }))?;
        exec_test_action(context(), &action).await?;

        let timeout_config = ResolvedConfig {
            element_timeout_ms: 300,
            ..Default::default()
        };
        let action: TestAction = serde_json::from_value(serde_json::json!({
            "wait": 0,
            "url": format!("{destination_origin}/never")
        }))?;
        let error = tokio::time::timeout(
            Duration::from_secs(3),
            exec_test_action(
                ActionExecutionContext {
                    config: &timeout_config,
                    ..context()
                },
                &action,
            ),
        )
        .await?
        .err()
        .ok_or_else(|| anyhow!("URL wait unexpectedly matched a different destination"))?;
        anyhow::ensure!(
            error.to_string().contains("Timeout waiting for URL"),
            "unexpected URL wait error: {error}"
        );
        Ok(())
    }
    .await;
    browser.close().await?;
    handler.await?;
    let _ = std::fs::remove_dir_all(directory);
    result
}

const SIMPLIFICATION_BLOCK: usize = 8;

/// Deterministic opaque noise: no block of it is uniform, so every pixelated
/// block is distinguishable from the source.
fn noise_pixels(seed: u32, width: usize, height: usize) -> Vec<rgb::RGBA8> {
    let mut state = seed.wrapping_mul(0x9E37_79B9) ^ 0x5EED;
    (0..width * height)
        .map(|_| {
            state = state.wrapping_mul(1_103_515_245).wrapping_add(12_345);
            let [low, red, green, blue] = state.to_le_bytes();
            rgb::RGBA8::new(red, green, blue ^ low, 255)
        })
        .collect()
}

fn stripe_svg() -> String {
    let mut svg =
        String::from(r#"<svg xmlns="http://www.w3.org/2000/svg" width="96" height="96">"#);
    for column in 0..96 {
        svg.push_str(&format!(
            r#"<rect x="{column}" y="0" width="1" height="96" fill="rgb({},{},{})"/>"#,
            column * 37 % 256,
            column * 91 % 256,
            column * 53 % 256
        ));
    }
    for row in (0..96).step_by(3) {
        svg.push_str(&format!(
            r#"<rect x="0" y="{row}" width="96" height="1" fill="rgb({},0,{})"/>"#,
            row * 7 % 256,
            255 - row * 5 % 256
        ));
    }
    svg.push_str("</svg>");
    svg
}

const SIMPLIFICATION_SCENES: &str = r#"<!doctype html>
<html><head><style>
body { margin: 0; background: rgb(255, 255, 255); font: 20px/24px sans-serif; color: rgb(0, 0, 0); }
.scene { position: absolute; }
.scene > img { display: block; }
.badge { position: absolute; left: 8px; top: 8px; width: 30px; height: 20px; }
.pseudo-badge::before { content: ''; position: absolute; left: 8px; top: 8px; width: 30px; height: 20px; background: rgb(255, 0, 255); }
</style></head><body>
<img id="plain" class="scene" src="/noise/1/96/96.png" width="96" height="96" style="left:10px;top:10px">
<img id="svgimg" class="scene" src="/pattern.svg" width="96" height="96" style="left:120px;top:10px">
<video id="video" class="scene" poster="/noise/2/96/96.png" width="96" height="96" style="left:230px;top:10px"></video>
<img id="partial" class="scene" src="/noise/3/67/67.png" width="67" height="67" style="left:340px;top:10px">
<img id="small" class="scene" src="/noise/4/40/40.png" width="40" height="40" style="left:430px;top:10px">
<img id="excluded" class="scene keep" src="/noise/5/96/96.png" width="96" height="96" style="left:480px;top:10px">
<div class="scene keep-zone" style="left:590px;top:10px"><img id="zone" src="/noise/6/96/96.png" width="96" height="96"></div>
<img id="hidden" src="/noise/7/96/96.png" width="96" height="96" style="display:none">
<img id="filtered" class="scene" src="/noise/8/96/96.png" width="96" height="96" style="left:10px;top:130px;filter:grayscale(1)">
<img id="scaled" class="scene" src="/noise/9/96/96.png" width="96" height="96" style="left:120px;top:130px;transform:scale(0.75);transform-origin:0 0">
<div class="scene" style="left:230px;top:130px;display:grid"><img id="grid-img" src="/noise/10/96/96.png" width="96" height="96" style="grid-area:1/1"><span style="grid-area:1/1;align-self:center;justify-self:center;color:rgb(255,0,0);background:rgb(255,255,0)">GRID</span></div>
<div class="scene" style="left:340px;top:130px;width:220px"><img id="negative-img" src="/noise/11/96/96.png" width="96" height="96" style="display:inline"><span style="display:inline-block;margin-left:-60px;background:rgb(255,200,0)">NEW</span></div>
<svg class="scene" width="96" height="96" style="left:590px;top:130px"><image id="svgimage" href="/noise/12/96/96.png" width="96" height="96"/></svg>
<svg class="scene" width="96" height="96" viewBox="0 0 48 48" style="left:700px;top:130px"><image id="svgscaled" href="/noise/13/48/48.png" width="48" height="48"/></svg>
<div class="scene" style="left:10px;top:250px"><span id="earlier-badge" class="badge" style="background:rgb(0,0,255)"></span><img id="earlier-img" src="/noise/14/96/96.png" width="96" height="96"></div>
<div class="scene" style="left:120px;top:250px"><img id="later-img" src="/noise/15/96/96.png" width="96" height="96"><span id="later-badge" class="badge" style="background:rgb(0,160,0)"></span></div>
<div class="scene" style="left:230px;top:250px"><span id="earlier-z-badge" class="badge" style="z-index:1;background:rgb(0,0,255)"></span><img id="earlier-z-img" src="/noise/16/96/96.png" width="96" height="96"></div>
<div class="scene pseudo-badge" style="left:340px;top:250px"><img id="pseudo-img" src="/noise/17/96/96.png" width="96" height="96"></div>
<div class="scene" style="left:450px;top:250px;width:96px"><div id="hero-clip" style="height:64px;overflow:hidden"><img id="hero-img" src="/noise/18/96/128.png" width="96" height="128" style="display:block"></div><p style="margin:0">Caption</p></div>
<div class="scene" style="left:560px;top:250px;width:96px"><img id="caption-over-img" src="/noise/19/96/96.png" width="96" height="96"><div style="margin-top:-30px;color:rgb(255,255,255)">Over</div></div>
<div class="scene" style="left:670px;top:250px;width:96px"><img id="bg-under-img" src="/noise/20/96/96.png" width="96" height="96"><div style="margin-top:-30px;height:20px;background:rgb(0,0,0)"></div></div>
</body></html>"#;

/// Cases beyond the basic scenes: misaligned SVG images and quarter turns,
/// decorated images, and overlays that only a transform, relative offset, or
/// shadow moves over an image. Each whole-pixel 120 px cell holds one case.
const SIMPLIFICATION_SHARP_CASES: &str = r#"<!doctype html>
<html><head><style>
body { margin: 0; background: rgb(255, 255, 255); font: 16px/20px sans-serif; color: rgb(0, 0, 0); }
.cell { position: absolute; box-sizing: border-box; width: 120px; height: 120px; padding: 12px; }
.cell img, .cell video, .cell svg { display: block; }
.shift::before { content: ''; position: absolute; left: 96px; top: 20px; width: 20px; height: 20px; background: rgb(255, 0, 255); transform: translateX(-60px); }
.nudge { height: 12px; }
.nudge::before { content: ''; display: block; position: relative; top: 30px; width: 40px; height: 12px; background: rgb(0, 200, 200); }
.caption { color: rgb(255, 255, 255); text-shadow: 0 -30px 0 rgb(255, 0, 0); }
.drop { position: absolute; left: 20px; top: 100px; width: 30px; height: 10px; background: rgb(255, 255, 255); filter: drop-shadow(0 -40px 0 rgb(0, 0, 255)); }
</style></head><body>
<div class="cell" id="c-svg-offset" style="left:10px;top:10px"><svg width="96" height="96"><image id="svg-offset" x="20" href="/noise/40/76/96.png" width="76" height="96"/></svg></div>
<div class="cell" id="c-svg-fraction" style="left:140px;top:10px"><svg width="96" height="96"><g transform="translate(0.5, 0.5)"><image id="svg-fraction" href="/noise/41/88/88.png" width="88" height="88"/></g></svg></div>
<div class="cell" id="c-svg-whole" style="left:270px;top:10px"><svg width="96" height="96"><g transform="translate(8, 4)"><image id="svg-whole" href="/noise/42/80/80.png" width="80" height="80"/></g></svg></div>
<div class="cell" id="c-odd-turn" style="left:400px;top:10px"><img id="odd-turn" src="/noise/43/97/96.png" width="97" height="96" style="transform:rotate(90deg)"></div>
<div class="cell" id="c-even-turn" style="left:530px;top:10px"><img id="even-turn" src="/noise/44/96/96.png" width="96" height="96" style="transform:rotate(90deg)"></div>
<div class="cell" id="c-odd-flip" style="left:660px;top:10px"><img id="odd-flip" src="/noise/45/97/96.png" width="97" height="96" style="transform:scale(-1, 1)"></div>
<div class="cell" id="c-shadowed" style="left:10px;top:140px"><img id="shadowed" src="/noise/46/80/80.png" width="80" height="80" style="box-shadow:6px 6px 0 rgb(0, 0, 0)"></div>
<div class="cell" id="c-bordered" style="left:140px;top:140px"><img id="bordered" src="/noise/47/80/80.png" width="80" height="80" style="border:4px solid rgb(0, 128, 255)"></div>
<div class="cell" id="c-padded" style="left:270px;top:140px"><img id="padded" src="/noise/48/80/80.png" width="80" height="80" style="padding:4px;background:rgb(255, 220, 0)"></div>
<div class="cell" id="c-rounded" style="left:400px;top:140px"><img id="rounded" src="/noise/49/80/80.png" width="80" height="80" style="border-radius:12px"></div>
<div class="cell" id="c-outlined" style="left:530px;top:140px"><img id="outlined" src="/noise/50/80/80.png" width="80" height="80" style="outline:3px solid rgb(255, 0, 0)"></div>
<div class="cell" id="c-clipped" style="left:660px;top:140px"><img id="clipped" src="/noise/51/80/80.png" width="80" height="80" style="clip-path:inset(4px)"></div>
<div class="cell" id="c-masked" style="left:10px;top:270px"><img id="masked" src="/noise/52/80/80.png" width="80" height="80" style="mask-image:linear-gradient(rgb(0, 0, 0), transparent)"></div>
<div class="cell" id="c-rounded-video" style="left:140px;top:270px"><video id="rounded-video" poster="/noise/53/96/96.png" width="96" height="96" style="border-radius:12px"></video></div>
<div class="cell shift" id="c-shift-img" style="left:270px;top:270px"><img id="shift-img" src="/noise/54/80/80.png" width="80" height="80"></div>
<div class="cell" id="c-nudge-img" style="left:400px;top:270px"><div class="nudge"></div><img id="nudge-img" src="/noise/55/80/80.png" width="80" height="80"></div>
<div class="cell" id="c-caption-img" style="left:530px;top:270px"><img id="caption-img" src="/noise/56/80/80.png" width="80" height="80"><div class="caption">Shadow</div></div>
<div class="cell" id="c-drop-img" style="left:660px;top:270px"><span class="drop"></span><img id="drop-img" src="/noise/57/80/80.png" width="80" height="80"></div>
</body></html>"#;

const SIMPLIFICATION_GALLERY: &str = r#"<!doctype html>
<html><head><style>
body { margin: 0; background: rgb(255, 255, 255); font: 16px/20px sans-serif; }
img { display: block; }
#header { position: sticky; top: 0; height: 40px; background: rgb(200, 0, 120); }
#intro { height: 280px; }
#gallery { width: 400px; }
#spacer { height: 600px; }
#status { position: absolute; left: 600px; top: 50px; width: 30px; height: 30px; background: rgb(128, 128, 128); }
#toggle { position: absolute; left: 540px; top: 100px; appearance: none; border: 0; padding: 0; background: rgb(0, 0, 0); color: rgb(255, 255, 255); font: 16px/20px sans-serif; }
#open, #open:visited { position: absolute; left: 540px; top: 150px; color: rgb(0, 0, 0); text-decoration: none; }
:focus { outline: none; }
</style></head><body>
<div id="header"></div>
<div id="intro"><img id="hero" src="/noise/31/128/128.png" width="128" height="128"><div class="keep-zone"><img id="kept" src="/noise/32/96/96.png" width="96" height="96"></div></div>
<div id="gallery"><img id="g1" src="/noise/33/400/300.png" width="400" height="300"><img id="g2" src="/noise/34/400/300.png" width="400" height="300"><img id="g3" src="/noise/35/400/300.png" width="400" height="300"></div>
<div id="spacer"></div>
<div id="status"></div>
<button id="toggle">Toggle</button>
<a id="open" href="/popup" target="_blank">Open popup</a>
<script>
document.getElementById('toggle').addEventListener('click', () => {
  const flags = document.querySelectorAll('[data-regressify-simplify]').length;
  const filtered = [...document.images].some(image => getComputedStyle(image).filter !== 'none');
  document.getElementById('status').style.background = flags === 0 && !filtered ? 'rgb(0, 160, 0)' : 'rgb(220, 0, 0)';
});
</script>
</body></html>"#;

const SIMPLIFICATION_POPUP: &str = r#"<!doctype html>
<html><head><style>body { margin: 0; background: rgb(255, 255, 255); } img { display: block; }</style></head>
<body><img id="popup-image" src="/noise/36/160/120.png" width="160" height="120"></body></html>"#;

fn simplification_page(request: Request) {
    let path = request
        .url()
        .split('?')
        .next()
        .unwrap_or_default()
        .to_owned();
    let respond = |request: Request, body: Vec<u8>, content_type: &str, csp: Option<&str>| {
        let mut response = Response::from_data(body)
            .with_header(Header::from_bytes("Content-Type", content_type).unwrap());
        if let Some(csp) = csp {
            response =
                response.with_header(Header::from_bytes("Content-Security-Policy", csp).unwrap());
        }
        let _ = request.respond(response);
    };
    if let Some(spec) = path
        .strip_prefix("/noise/")
        .and_then(|spec| spec.strip_suffix(".png"))
    {
        let parts: Vec<usize> = spec
            .split('/')
            .filter_map(|part| part.parse().ok())
            .collect();
        if let [seed, width, height] = parts[..] {
            let png = lodepng::encode32(&noise_pixels(seed as u32, width, height), width, height)
                .unwrap();
            return respond(request, png, "image/png", None);
        }
    }
    match path.as_str() {
        "/pattern.svg" => respond(request, stripe_svg().into_bytes(), "image/svg+xml", None),
        "/scenes" => respond(request, SIMPLIFICATION_SCENES.into(), "text/html", None),
        "/gallery" => respond(request, SIMPLIFICATION_GALLERY.into(), "text/html", None),
        "/sharp-cases" => respond(request, SIMPLIFICATION_SHARP_CASES.into(), "text/html", None),
        "/popup" => respond(request, SIMPLIFICATION_POPUP.into(), "text/html", None),
        // Inline <style> elements and style attributes are blocked; only
        // same-origin resources and CSSOM changes remain.
        "/csp" => respond(
            request,
            br#"<!doctype html><html><head></head><body><p id="csp-hidden">Visible</p><img id="csp" src="/noise/21/96/96.png" width="96" height="96"></body></html>"#.to_vec(),
            "text/html",
            Some("default-src 'self'; style-src 'self'"),
        ),
        "/base" => {
            use base64::Engine as _;
            let png = lodepng::encode32(&noise_pixels(22, 96, 96), 96, 96).unwrap();
            let html = format!(
                r#"<!doctype html><html><head><base href="https://example.invalid/elsewhere/"></head><body style="margin:0"><img id="based" src="data:image/png;base64,{}" width="96" height="96" style="display:block"></body></html>"#,
                base64::engine::general_purpose::STANDARD.encode(png)
            );
            respond(request, html.into_bytes(), "text/html", None)
        }
        _ => {
            let _ = request.respond(Response::empty(404));
        }
    }
}

type Capture = lodepng::Bitmap<rgb::RGBA8>;

#[derive(Clone, Copy, Debug, PartialEq)]
struct Area {
    x: usize,
    y: usize,
    width: usize,
    height: usize,
}

impl Area {
    fn contains(&self, x: usize, y: usize) -> bool {
        x >= self.x && x < self.x + self.width && y >= self.y && y < self.y + self.height
    }

    fn intersects(&self, other: &Area) -> bool {
        self.x < other.x + other.width
            && other.x < self.x + self.width
            && self.y < other.y + other.height
            && other.y < self.y + self.height
    }
}

fn capture_pixel(image: &Capture, x: usize, y: usize) -> rgb::RGBA8 {
    image.buffer[y * image.width + x]
}

/// Pixels inside `area` that differ between two captures.
fn changed_pixels(first: &Capture, second: &Capture, area: Area) -> usize {
    (area.y..area.y + area.height)
        .flat_map(|y| (area.x..area.x + area.width).map(move |x| (x, y)))
        .filter(|&(x, y)| capture_pixel(first, x, y) != capture_pixel(second, x, y))
        .count()
}

/// Pixels breaking uniform blocks aligned to the area origin. Blocks touching
/// `skip` (an overlay drawn on top) are ignored.
fn nonuniform_block_pixels(image: &Capture, area: Area, block: usize, skip: Option<Area>) -> usize {
    let mut broken = 0;
    for top in (0..area.height).step_by(block) {
        for left in (0..area.width).step_by(block) {
            let cell = Area {
                x: area.x + left,
                y: area.y + top,
                width: block.min(area.width - left),
                height: block.min(area.height - top),
            };
            if skip.is_some_and(|skip| skip.intersects(&cell)) {
                continue;
            }
            let first = capture_pixel(image, cell.x, cell.y);
            broken += (cell.y..cell.y + cell.height)
                .flat_map(|y| (cell.x..cell.x + cell.width).map(move |x| (x, y)))
                .filter(|&(x, y)| capture_pixel(image, x, y) != first)
                .count();
        }
    }
    broken
}

/// Blocks whose colour is not the block average of the source image (edge
/// pixels duplicated for partial blocks), allowing one level of rounding.
/// `rows` limits the check to visible source rows.
fn block_mean_errors(
    image: &Capture,
    origin: (usize, usize),
    source: &[rgb::RGBA8],
    (width, height): (usize, usize),
    block: usize,
    rows: std::ops::Range<usize>,
) -> usize {
    let mut errors = 0;
    for top in (0..height).step_by(block) {
        for left in (0..width).step_by(block) {
            let mut sum = [0u32; 3];
            for y in top..top + block {
                for x in left..left + block {
                    let pixel = source[y.min(height - 1) * width + x.min(width - 1)];
                    sum[0] += u32::from(pixel.r);
                    sum[1] += u32::from(pixel.g);
                    sum[2] += u32::from(pixel.b);
                }
            }
            let count = (block * block) as f64;
            let mean = sum.map(|channel| f64::from(channel) / count);
            for y in top..(top + block).min(height) {
                if !rows.contains(&y) {
                    continue;
                }
                for x in left..(left + block).min(width) {
                    let pixel = capture_pixel(image, origin.0 + x, origin.1 + y);
                    let actual = [pixel.r, pixel.g, pixel.b].map(f64::from);
                    if actual
                        .iter()
                        .zip(mean)
                        .any(|(actual, expected)| (actual - expected).abs() > 1.0)
                    {
                        errors += 1;
                    }
                }
            }
        }
    }
    errors
}

/// Full-page capture matching the product's capture parameters.
async fn full_page_capture(page: &chromiumoxide::Page) -> Result<Capture> {
    use chromiumoxide::{
        cdp::browser_protocol::page::{CaptureScreenshotFormat, Viewport},
        page::ScreenshotParams,
    };
    let metrics = page.layout_metrics().await?;
    let png = page
        .screenshot(
            ScreenshotParams::builder()
                .format(CaptureScreenshotFormat::Png)
                .clip(Viewport {
                    x: 0.0,
                    y: 0.0,
                    width: metrics.css_content_size.width,
                    height: metrics.css_content_size.height,
                    scale: 1.0,
                })
                .capture_beyond_viewport(true)
                .from_surface(true)
                .omit_background(false)
                .build(),
        )
        .await?;
    Ok(lodepng::decode32(png)?)
}

async fn content_size(page: &chromiumoxide::Page) -> Result<(f64, f64)> {
    let size = page.layout_metrics().await?.css_content_size;
    Ok((size.width, size.height))
}

/// Wait for images, then for video posters, which document.images omits.
async fn settle_simplification_page(page: &chromiumoxide::Page) -> Result<()> {
    let settled = super::page_stability::wait_for_page_settle(page, Duration::from_secs(5)).await?;
    anyhow::ensure!(settled.settled, "fixture page did not settle");
    super::runtime::evaluate_main_document(
        page,
        "Promise.all([...document.querySelectorAll('video[poster]')].map(video => new Promise(resolve => { \
           const image = new Image(); image.onload = image.onerror = resolve; image.src = video.poster; }))) \
         .then(() => new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve))))",
    )
    .await?;
    Ok(())
}

async fn element_areas(
    page: &chromiumoxide::Page,
    ids: &[&str],
) -> Result<std::collections::BTreeMap<String, Area>> {
    let rects: std::collections::BTreeMap<String, [f64; 4]> =
        super::runtime::evaluate_main_document(
            page,
            format!(
                "Object.fromEntries({}.map(id => {{ const rect = document.getElementById(id).getBoundingClientRect(); \
                 return [id, [rect.left + scrollX, rect.top + scrollY, rect.width, rect.height]]; }}))",
                serde_json::to_string(ids)?
            ),
        )
        .await?
        .into_value()?;
    rects
        .into_iter()
        .map(|(id, [x, y, width, height])| {
            anyhow::ensure!(
                [x, y, width, height]
                    .iter()
                    .all(|value| value.fract() == 0.0 && *value >= 0.0),
                "{id} is not on whole pixels: {x},{y} {width}x{height}"
            );
            Ok((
                id,
                Area {
                    x: x as usize,
                    y: y as usize,
                    width: width as usize,
                    height: height as usize,
                },
            ))
        })
        .collect()
}

/// Regressify-owned page state; every value must stay zero in mode `none`.
const SIMPLIFICATION_DOM_STATE: &str = "({ \
    hosts: document.querySelectorAll('svg[id^=\"regressify-simplify-host-\"]').length, \
    sheets: document.adoptedStyleSheets.length, \
    flags: document.querySelectorAll('[data-regressify-simplify]').length, \
    filters: [...document.querySelectorAll('img, video, svg image')].map(element => getComputedStyle(element).filter).filter(filter => filter !== 'none') })";

#[tokio::test]
#[ignore = "requires managed Chromium; run regressify install"]
async fn image_simplification_pixelates_only_unobscured_images_in_chromium() -> Result<()> {
    use super::{
        hooks::DocumentHooks,
        image_simplification::{ImageSimplifier, MarkCounts},
        page_stability::prepare_screenshot,
        runtime::evaluate_main_document,
    };
    use crate::{
        config::ResolvedConfig,
        types::{ImageSimplification, TestScenario},
    };
    use serde_json::{Value, json};

    const PIXELATED: [&str; 9] = [
        "plain",
        "svgimg",
        "video",
        "partial",
        "later-img",
        "earlier-z-img",
        "hero-img",
        "bg-under-img",
        "svgimage",
    ];
    // Promotion would paint these over grid text, an inline badge, an earlier
    // positioned badge, a generated badge, or later caption text.
    const GUARDED: [&str; 5] = [
        "grid-img",
        "negative-img",
        "earlier-img",
        "pseudo-img",
        "caption-over-img",
    ];
    const UNFLAGGED: [&str; 6] = [
        "small",
        "excluded",
        "zone",
        "filtered",
        "scaled",
        "svgscaled",
    ];
    let block = SIMPLIFICATION_BLOCK;
    let site = Site::new(simplification_page)?;
    let workspace = tempfile::tempdir()?;
    let pixelate = ResolvedConfig {
        image_simplification: ImageSimplification::Pixelate,
        image_simplification_block_size: block as u32,
        image_simplification_min_size: 64,
        image_simplification_exclude_selectors: vec![
            ".keep".into(),
            ".keep-zone".into(),
            "img[".into(),
        ],
        delay: Some(0.0),
        settle_timeout_ms: 2_000,
        ..Default::default()
    };
    let none = ResolvedConfig {
        image_simplification: ImageSimplification::None,
        ..pixelate.clone()
    };
    let simplifier = ImageSimplifier::from_config(&pixelate)?
        .ok_or_else(|| anyhow!("pixelate must build a simplifier"))?;
    let (mut browser, handler, directory) = launch_browser(
        "image-simplification",
        LaunchOptions {
            viewport: Some((800, 600)),
            ..Default::default()
        },
    )
    .await?;
    let result: Result<()> = async {
        let scenes = format!("{}/scenes", site.origin);
        let dom_state = |page: &chromiumoxide::Page| {
            let page = page.clone();
            async move {
                Ok::<Value, anyhow::Error>(
                    evaluate_main_document(&page, SIMPLIFICATION_DOM_STATE)
                        .await?
                        .into_value()?,
                )
            }
        };

        // Mode none performs no page work: same DOM and pixels as an untouched capture.
        let page = browser.new_page(scenes.as_str()).await?;
        settle_simplification_page(&page).await?;
        let untouched = full_page_capture(&page).await?;
        let untouched_size = content_size(&page).await?;
        DocumentHooks::load(workspace.path(), &TestScenario::default(), &none)?
            .apply(&page)
            .await?;
        prepare_screenshot(&page, &none, "image-simplification-none").await?;
        let state = dom_state(&page).await?;
        anyhow::ensure!(
            state == json!({"hosts": 0, "sheets": 0, "flags": 0, "filters": ["grayscale(1)"]}),
            "mode none changed the document: {state}"
        );
        let baseline = full_page_capture(&page).await?;
        anyhow::ensure!(
            (baseline.width, baseline.height) == (untouched.width, untouched.height)
                && baseline.buffer == untouched.buffer,
            "mode none changed captured pixels"
        );
        page.close().await?;

        let page = browser.new_page(scenes.as_str()).await?;
        settle_simplification_page(&page).await?;
        let hooks = DocumentHooks::load(workspace.path(), &TestScenario::default(), &pixelate)?;
        hooks.apply(&page).await?;
        let state = dom_state(&page).await?;
        anyhow::ensure!(
            state["hosts"] == 1 && state["sheets"] == 1 && state["flags"] == 0,
            "document setup did not install exactly one filter host and sheet: {state}"
        );
        anyhow::ensure!(
            content_size(&page).await? == untouched_size,
            "the filter host changed page dimensions"
        );
        prepare_screenshot(&page, &pixelate, "image-simplification").await?;
        let flags: std::collections::BTreeMap<String, Option<String>> = evaluate_main_document(
            &page,
            "Object.fromEntries([...document.querySelectorAll('img, video, svg image')]\
             .map(element => [element.id, element.getAttribute('data-regressify-simplify')]))",
        )
        .await?
        .into_value()?;
        let mut expected = std::collections::BTreeMap::new();
        for id in PIXELATED {
            expected.insert(id.to_owned(), Some("pixelate".to_owned()));
        }
        for id in GUARDED {
            expected.insert(id.to_owned(), Some("sharp".to_owned()));
        }
        for id in UNFLAGGED.into_iter().chain(["hidden"]) {
            expected.insert(id.to_owned(), None);
        }
        anyhow::ensure!(flags == expected, "unexpected image flags: {flags:?}");
        let first = full_page_capture(&page).await?;
        anyhow::ensure!(
            (first.width, first.height) == (baseline.width, baseline.height)
                && content_size(&page).await? == untouched_size,
            "pixelation changed page dimensions"
        );

        let ids: Vec<&str> = PIXELATED
            .iter()
            .chain(GUARDED.iter())
            .chain(UNFLAGGED.iter())
            .chain(["later-badge", "earlier-z-badge", "hero-clip"].iter())
            .copied()
            .collect();
        let areas = element_areas(&page, &ids).await?;
        let visible = |id: &str| {
            if id == "hero-img" {
                // Only the part inside the overflow clip is painted.
                Area {
                    height: areas["hero-clip"].height,
                    ..areas[id]
                }
            } else {
                areas[id]
            }
        };
        let overlays = [areas["later-badge"], areas["earlier-z-badge"]];
        for id in PIXELATED {
            let area = visible(id);
            let skip = overlays.iter().copied().find(|overlay| overlay.intersects(&area));
            let broken = nonuniform_block_pixels(&first, area, block, skip);
            anyhow::ensure!(broken == 0, "{id}: {broken} pixels break {block}px blocks");
            anyhow::ensure!(
                changed_pixels(&first, &baseline, area) > 0,
                "{id} was not pixelated"
            );
        }
        // Partial edge blocks are filled, never left transparent over the page.
        let partial = visible("partial");
        anyhow::ensure!(
            (partial.y..partial.y + partial.height)
                .flat_map(|y| (partial.x..partial.x + partial.width).map(move |x| (x, y)))
                .all(|(x, y)| capture_pixel(&first, x, y) != rgb::RGBA8::new(255, 255, 255, 255)),
            "partial edge blocks show the page background"
        );
        for id in GUARDED.iter().chain(UNFLAGGED.iter()) {
            let changed = changed_pixels(&first, &baseline, visible(id));
            anyhow::ensure!(changed == 0, "{id} should stay sharp; {changed} pixels changed");
        }
        for overlay in overlays {
            anyhow::ensure!(
                changed_pixels(&first, &baseline, overlay) == 0,
                "an overlay above a pixelated image changed"
            );
        }
        // Nothing outside pixelated images changes, including the hidden host.
        let pixelated_areas: Vec<Area> = PIXELATED.iter().map(|id| visible(id)).collect();
        let outside = (0..first.height)
            .flat_map(|y| (0..first.width).map(move |x| (x, y)))
            .filter(|&(x, y)| !pixelated_areas.iter().any(|area| area.contains(x, y)))
            .filter(|&(x, y)| capture_pixel(&first, x, y) != capture_pixel(&baseline, x, y))
            .count();
        anyhow::ensure!(outside == 0, "{outside} pixels changed outside pixelated images");

        // Restoring removes every flag and returns the exact original pixels.
        simplifier.restore(&page).await?;
        let state = dom_state(&page).await?;
        anyhow::ensure!(
            state["flags"] == 0 && state["filters"] == json!(["grayscale(1)"]),
            "flags or filters remained after restore: {state}"
        );
        anyhow::ensure!(
            full_page_capture(&page).await?.buffer == baseline.buffer,
            "restored page differs from the original"
        );

        // A single-page app removed the host and replaced adopted sheets.
        evaluate_main_document(
            &page,
            "document.querySelector('svg[id^=\"regressify-simplify-host-\"]').remove(); document.adoptedStyleSheets = [];",
        )
        .await?;
        anyhow::ensure!(dom_state(&page).await?["hosts"] == 0);
        prepare_screenshot(&page, &pixelate, "image-simplification-spa").await?;
        let state = dom_state(&page).await?;
        anyhow::ensure!(
            state["hosts"] == 1 && state["sheets"] == 1,
            "the filter was not re-installed: {state}"
        );
        let second = full_page_capture(&page).await?;
        anyhow::ensure!(
            second.buffer == first.buffer,
            "repeated pixelation of the same page changed pixels"
        );
        simplifier.restore(&page).await?;
        let counts = simplifier.prepare(&page).await?;
        anyhow::ensure!(
            counts
                == MarkCounts {
                    simplified: 9,
                    small: 1,
                    excluded: 2,
                    filtered: 1,
                    transformed: 2,
                    overlapped: 5,
                    invalid_selectors: 1,
                },
            "unexpected counts: {counts:?}"
        );
        simplifier.restore(&page).await?;
        page.close().await?;

        // A fresh document produces identical pixels.
        let page = browser.new_page(scenes.as_str()).await?;
        settle_simplification_page(&page).await?;
        hooks.apply(&page).await?;
        prepare_screenshot(&page, &pixelate, "image-simplification-fresh").await?;
        anyhow::ensure!(
            full_page_capture(&page).await?.buffer == first.buffer,
            "a second run produced different pixels"
        );
        page.close().await?;

        // Strict style-src CSP: injected <style> elements are ignored, the
        // constructed sheet and CSSOM-styled host still work.
        let csp = ResolvedConfig {
            hide_selectors: vec!["#csp-hidden".into()],
            ..pixelate.clone()
        };
        for (path, id) in [("csp", "csp"), ("base", "based")] {
            let page = browser.new_page(format!("{}/{path}", site.origin)).await?;
            settle_simplification_page(&page).await?;
            let original = full_page_capture(&page).await?;
            DocumentHooks::load(workspace.path(), &TestScenario::default(), &csp)?
                .apply(&page)
                .await?;
            prepare_screenshot(&page, &csp, "image-simplification-csp").await?;
            let state: Value = evaluate_main_document(
                &page,
                format!(
                    "({{ hidden: getComputedStyle(document.getElementById('csp-hidden') || document.body).visibility, \
                       flag: document.getElementById({0}).getAttribute('data-regressify-simplify'), \
                       filter: getComputedStyle(document.getElementById({0})).filter }})",
                    serde_json::to_string(id)?
                ),
            )
            .await?
            .into_value()?;
            if path == "csp" {
                anyhow::ensure!(
                    state["hidden"] == "visible",
                    "the CSP fixture did not block injected styles: {state}"
                );
            }
            anyhow::ensure!(
                state["flag"] == "pixelate"
                    && state["filter"]
                        .as_str()
                        .is_some_and(|filter| filter.contains("regressify-simplify-filter-")),
                "{path}: image was not filtered: {state}"
            );
            let image = full_page_capture(&page).await?;
            let area = element_areas(&page, &[id]).await?[id];
            anyhow::ensure!(nonuniform_block_pixels(&image, area, block, None) == 0);
            anyhow::ensure!(changed_pixels(&image, &original, area) > 0, "{path}: not pixelated");
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
async fn image_simplification_keeps_misaligned_and_decorated_images_sharp_in_chromium() -> Result<()>
{
    use super::{
        hooks::DocumentHooks,
        image_simplification::{ImageSimplifier, MarkCounts},
        page_stability::prepare_screenshot,
        runtime::evaluate_main_document,
    };
    use crate::{
        config::ResolvedConfig,
        types::{ImageSimplification, TestScenario},
    };

    // A whole-pixel SVG translation, an even-sized quarter turn, and a flip of
    // an odd-sized image keep exact blocks.
    const PIXELATED: [&str; 3] = ["svg-whole", "even-turn", "odd-flip"];
    // Promotion would paint these over a translated generated badge, a
    // relatively offset generated badge, caption text-shadow, or an earlier
    // badge's drop-shadow.
    const GUARDED: [&str; 4] = ["shift-img", "nudge-img", "caption-img", "drop-img"];
    // Filtering would erase or seam the first three (non-origin SVG image,
    // fractional SVG translation, odd-sized quarter turn) and blend or clip
    // the decorations of the rest.
    const UNFLAGGED: [&str; 11] = [
        "svg-offset",
        "svg-fraction",
        "odd-turn",
        "shadowed",
        "bordered",
        "padded",
        "rounded",
        "outlined",
        "clipped",
        "masked",
        "rounded-video",
    ];
    let block = SIMPLIFICATION_BLOCK;
    let site = Site::new(simplification_page)?;
    let workspace = tempfile::tempdir()?;
    let pixelate = ResolvedConfig {
        image_simplification: ImageSimplification::Pixelate,
        image_simplification_block_size: block as u32,
        image_simplification_min_size: 64,
        delay: Some(0.0),
        settle_timeout_ms: 2_000,
        ..Default::default()
    };
    let simplifier = ImageSimplifier::from_config(&pixelate)?
        .ok_or_else(|| anyhow!("pixelate must build a simplifier"))?;
    let (mut browser, handler, directory) = launch_browser(
        "image-simplification-sharp",
        LaunchOptions {
            viewport: Some((800, 600)),
            ..Default::default()
        },
    )
    .await?;
    let result: Result<()> = async {
        let page = browser
            .new_page(format!("{}/sharp-cases", site.origin))
            .await?;
        settle_simplification_page(&page).await?;
        let baseline = full_page_capture(&page).await?;
        DocumentHooks::load(workspace.path(), &TestScenario::default(), &pixelate)?
            .apply(&page)
            .await?;
        prepare_screenshot(&page, &pixelate, "image-simplification-sharp").await?;
        let flags: std::collections::BTreeMap<String, Option<String>> = evaluate_main_document(
            &page,
            "Object.fromEntries([...document.querySelectorAll('img, video, svg image')]\
             .map(element => [element.id, element.getAttribute('data-regressify-simplify')]))",
        )
        .await?
        .into_value()?;
        let mut expected = std::collections::BTreeMap::new();
        for id in PIXELATED {
            expected.insert(id.to_owned(), Some("pixelate".to_owned()));
        }
        for id in GUARDED {
            expected.insert(id.to_owned(), Some("sharp".to_owned()));
        }
        for id in UNFLAGGED {
            expected.insert(id.to_owned(), None);
        }
        anyhow::ensure!(flags == expected, "unexpected image flags: {flags:?}");
        let first = full_page_capture(&page).await?;

        let cells: Vec<String> = PIXELATED
            .iter()
            .chain(GUARDED.iter())
            .chain(UNFLAGGED.iter())
            .map(|id| format!("c-{id}"))
            .collect();
        let ids: Vec<&str> = cells
            .iter()
            .map(String::as_str)
            .chain(PIXELATED.iter().copied())
            .collect();
        let areas = element_areas(&page, &ids).await?;
        for id in PIXELATED {
            let area = areas[id];
            // Chromium filters the flipped image in screen orientation, so even
            // the 97 px flip keeps blocks aligned to its on-screen left edge.
            let broken = nonuniform_block_pixels(&first, area, block, None);
            anyhow::ensure!(broken == 0, "{id}: {broken} pixels break {block}px blocks");
            anyhow::ensure!(
                changed_pixels(&first, &baseline, area) > 0,
                "{id} was not pixelated"
            );
        }
        // Sharp images, their decorations, and every overlay keep their exact
        // pixels: nothing is erased, blended, or painted over.
        for id in GUARDED.iter().chain(UNFLAGGED.iter()) {
            let changed = changed_pixels(&first, &baseline, areas[&format!("c-{id}")]);
            anyhow::ensure!(
                changed == 0,
                "{id} should stay sharp; {changed} pixels changed"
            );
        }
        let pixelated_areas: Vec<Area> = PIXELATED.iter().map(|id| areas[*id]).collect();
        let outside = (0..first.height)
            .flat_map(|y| (0..first.width).map(move |x| (x, y)))
            .filter(|&(x, y)| !pixelated_areas.iter().any(|area| area.contains(x, y)))
            .filter(|&(x, y)| capture_pixel(&first, x, y) != capture_pixel(&baseline, x, y))
            .count();
        anyhow::ensure!(
            outside == 0,
            "{outside} pixels changed outside pixelated images"
        );

        simplifier.restore(&page).await?;
        let counts = simplifier.prepare(&page).await?;
        anyhow::ensure!(
            counts
                == MarkCounts {
                    simplified: 3,
                    small: 0,
                    excluded: 0,
                    filtered: 8,
                    transformed: 3,
                    overlapped: 4,
                    invalid_selectors: 0,
                },
            "unexpected counts: {counts:?}"
        );
        simplifier.restore(&page).await?;
        page.close().await?;
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
async fn image_simplification_ref_and_test_cover_saves_finals_and_popups() -> Result<()> {
    use crate::{
        diagnostics::{
            journal::{ArchiveState, JournalReader},
            record::{DiagnosticEvent, Field, FieldValue, SafeValue, Stage},
        },
        test::capture,
        types::ScreenshotType,
    };
    use clap::Parser;
    use serde_json::{Value, json};

    if let Some(root) = std::env::var_os("REGRESSIFY_IMAGE_SIMPLIFICATION_FIXTURE") {
        let mode = std::env::var("REGRESSIFY_IMAGE_SIMPLIFICATION_MODE")?;
        let command = if mode == "reference" { "ref" } else { "test" };
        let arguments: Vec<std::ffi::OsString> = vec![
            "regressify".into(),
            "--root".into(),
            root,
            "--run-id".into(),
            mode.into(),
            "--async-capture-limit".into(),
            "3".into(),
            "--async-capture-ramp-ms".into(),
            "0".into(),
            command.into(),
            "--fetch-browser".into(),
            "false".into(),
        ];
        return crate::application::execute(crate::cli::Cli::try_parse_from(arguments)?).await;
    }
    let block = SIMPLIFICATION_BLOCK;
    let site = Site::new(simplification_page)?;
    let workspace = tempfile::tempdir()?;
    let root = workspace.path();
    std::fs::create_dir_all(root.join("visual_tests"))?;
    std::fs::create_dir_all(root.join("common"))?;
    std::fs::write(
        root.join("common/_viewports.yaml"),
        "- label: desktop\n  width: 640\n  height: 480\n",
    )?;
    // The second exclusion matches nothing; its text must never reach a journal.
    let workspace_config = |block_size: u32| {
        format!(
            "elementTimeout: 5000\npostInteractionWait: 0\ndelay: 0\nimageSimplification: pixelate\n\
             imageSimplificationBlockSize: {block_size}\nimageSimplificationExcludeSelectors:\n  \
             - '.keep-zone'\n  - 'img[data-secret=\"exclusion-marker\"]'\n"
        )
    };
    std::fs::write(root.join("regressify.yaml"), workspace_config(8))?;
    let gallery = format!("{}/gallery", site.origin);
    let suite = json!({ "id": "pixelation", "scenarios": [
        { "id": "gallery", "url": gallery, "selectors": ["viewport", "#gallery"],
          "actions": [{ "save": "first" }, { "click": "#toggle" }, { "save": "second" }] },
        { "id": "popup", "url": gallery, "selectors": ["viewport"],
          "actions": [{ "click": "#open" }, { "switchTab": "new" }, { "wait": "#popup-image" },
                      { "save": "popup" }, { "closeTab": "current" }, { "save": "back" }] },
        { "id": "sharp", "url": gallery, "imageSimplification": "none", "selectors": ["viewport"] }
    ] });
    std::fs::write(
        root.join("visual_tests/pixelation.tests.json"),
        serde_json::to_vec_pretty(&suite)?,
    )?;
    // Returns the child's console output; a nonzero exit fails the fixture.
    let run = |mode: &str| -> Result<String> {
        let output = std::process::Command::new(std::env::current_exe()?)
            .args([
                "--ignored",
                "--exact",
                "test::browser_tests::image_simplification_ref_and_test_cover_saves_finals_and_popups",
                "--nocapture",
            ])
            .env("REGRESSIFY_IMAGE_SIMPLIFICATION_FIXTURE", root)
            .env("REGRESSIFY_IMAGE_SIMPLIFICATION_MODE", mode)
            .env_remove("REPLACEMENT_PROFILE")
            .env_remove("REGRESSIFY_DISABLE_DIAGNOSTIC_ARCHIVE")
            .env_remove("REGRESSIFY_DIAGNOSTIC_DISK_BUDGET_BYTES")
            .output()?;
        anyhow::ensure!(
            output.status.success(),
            "{mode}: {}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        Ok(format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ))
    };
    run("reference")?;
    run("test")?;

    let output_dir = root.join(".regressify");
    let suite_dir = output_dir.join("pixelation");
    let report: Value = serde_json::from_slice(&std::fs::read(
        suite_dir.join("bitmaps_test/test/report.json"),
    )?)?;
    let tests = report["tests"].as_array().unwrap();
    assert_eq!(tests.len(), 8, "{report}");
    assert!(
        tests.iter().all(|test| test["status"] == "pass"),
        "two pixelated runs must compare equal: {report}"
    );
    let step = |name: &str| ScreenshotType::Step(name.into());
    let load = |id: &str, kind: &ScreenshotType| -> Result<(Capture, std::path::PathBuf)> {
        let key = capture::final_screenshot_order_key(id, Some("desktop"), kind);
        let reference = suite_dir
            .join("bitmaps_reference")
            .join(format!("{key}.png"));
        let test = suite_dir
            .join("bitmaps_test/test")
            .join(format!("{key}.png"));
        for path in [&reference, &test] {
            anyhow::ensure!(
                !capture::error_marker_path(path).exists(),
                "{key} failed to capture"
            );
        }
        let reference_image = lodepng::decode32(std::fs::read(&reference)?)?;
        let test_image = lodepng::decode32(std::fs::read(&test)?)?;
        anyhow::ensure!(
            (reference_image.width, reference_image.height)
                == (test_image.width, test_image.height),
            "{key}: reference and test sizes differ"
        );
        let differing: Vec<(usize, usize)> = (0..test_image.height)
            .flat_map(|y| (0..test_image.width).map(move |x| (x, y)))
            .filter(|&(x, y)| {
                capture_pixel(&reference_image, x, y) != capture_pixel(&test_image, x, y)
            })
            .collect();
        anyhow::ensure!(
            differing.is_empty(),
            "{key}: {} reference and test pixels differ, x {:?}..{:?}, y {:?}..{:?}",
            differing.len(),
            differing.iter().map(|point| point.0).min(),
            differing.iter().map(|point| point.0).max(),
            differing.iter().map(|point| point.1).min(),
            differing.iter().map(|point| point.1).max()
        );
        Ok((test_image, test))
    };
    let hero = noise_pixels(31, 128, 128);
    let kept = noise_pixels(32, 96, 96);
    let photos = [33, 34, 35].map(|seed| noise_pixels(seed, 400, 300));
    let status = |image: &Capture| {
        let pixel = capture_pixel(image, 615, 65);
        (pixel.r, pixel.g, pixel.b)
    };
    // Full-page checkpoints at the top: every unobscured image is block-averaged,
    // the excluded image keeps its exact source pixels.
    for (name, expected_status) in [("first", (128, 128, 128)), ("second", (0, 160, 0))] {
        let (image, _) = load("gallery", &step(name))?;
        anyhow::ensure!(
            block_mean_errors(&image, (0, 40), &hero, (128, 128), block, 0..128) == 0,
            "{name}: hero is not block-averaged"
        );
        let kept_area = Area {
            x: 0,
            y: 168,
            width: 96,
            height: 96,
        };
        anyhow::ensure!(
            (0..96)
                .flat_map(|y| (0..96).map(move |x| (x, y)))
                .all(
                    |(x, y)| capture_pixel(&image, kept_area.x + x, kept_area.y + y)
                        == kept[y * 96 + x]
                ),
            "{name}: excluded image changed"
        );
        for (index, photo) in photos.iter().enumerate() {
            anyhow::ensure!(
                block_mean_errors(
                    &image,
                    (0, 320 + index * 300),
                    photo,
                    (400, 300),
                    block,
                    0..300
                ) == 0,
                "{name}: gallery image {index} is not block-averaged"
            );
        }
        // The click handler saw no flags and no filters after the first checkpoint.
        anyhow::ensure!(
            status(&image) == expected_status,
            "{name}: status {:?}",
            status(&image)
        );
    }
    let (viewport, _) = load("gallery", &ScreenshotType::Viewport)?;
    anyhow::ensure!(
        (viewport.width, viewport.height) == (640, 480)
            && block_mean_errors(&viewport, (0, 40), &hero, (128, 128), block, 0..128) == 0
            && block_mean_errors(&viewport, (0, 320), &photos[0], (400, 300), block, 0..160) == 0
            && status(&viewport) == (0, 160, 0),
        "viewport final is not pixelated"
    );
    // Centering #gallery scrolls the sticky header (z-index auto, earlier in the
    // document) over the first photo; the refreshed guard keeps that photo sharp
    // so the header stays on top, while the other photos stay pixelated.
    let (selected, _) = load("gallery", &ScreenshotType::Selector("#gallery".into()))?;
    anyhow::ensure!(
        (selected.width, selected.height) == (400, 900),
        "unexpected selector size {}x{}",
        selected.width,
        selected.height
    );
    let header_rows = 210..250;
    for y in 0..300 {
        for x in (0..400).step_by(7) {
            let pixel = capture_pixel(&selected, x, y);
            if header_rows.contains(&y) {
                anyhow::ensure!(
                    (pixel.r, pixel.g, pixel.b) == (200, 0, 120),
                    "sticky header hidden at {x},{y}: {pixel:?}"
                );
            } else {
                anyhow::ensure!(
                    pixel == photos[0][y * 400 + x],
                    "overlapped photo was pixelated at {x},{y}"
                );
            }
        }
    }
    for (index, photo) in photos.iter().enumerate().skip(1) {
        anyhow::ensure!(
            block_mean_errors(
                &selected,
                (0, index * 300),
                photo,
                (400, 300),
                block,
                0..300
            ) == 0,
            "selector capture photo {index} is not block-averaged"
        );
    }
    // Popups after switchTab, the original after closeTab, and its final.
    let (popup, _) = load("popup", &step("popup"))?;
    anyhow::ensure!(
        block_mean_errors(
            &popup,
            (0, 0),
            &noise_pixels(36, 160, 120),
            (160, 120),
            block,
            0..120
        ) == 0,
        "popup checkpoint is not pixelated"
    );
    for kind in [step("back"), ScreenshotType::Viewport] {
        let (image, _) = load("popup", &kind)?;
        anyhow::ensure!(
            block_mean_errors(&image, (0, 40), &hero, (128, 128), block, 0..128) == 0
                && status(&image) == (128, 128, 128),
            "{kind:?} after closeTab is not pixelated"
        );
    }
    // A scenario override of none captures exact source pixels.
    let (sharp, _) = load("sharp", &ScreenshotType::Viewport)?;
    for (origin, source, width, rows) in
        [((0, 40), &hero, 128, 128), ((0, 320), &photos[0], 400, 160)]
    {
        anyhow::ensure!(
            (0..rows)
                .flat_map(|y| (0..width).map(move |x| (x, y)))
                .all(|(x, y)| capture_pixel(&sharp, origin.0 + x, origin.1 + y)
                    == source[y * width + x]),
            "mode none changed image pixels"
        );
    }

    // Diagnostics: counts only, never the configured selector text.
    let count_fields = [
        Field::SimplifiedImages,
        Field::SmallImages,
        Field::ExcludedImages,
        Field::FilteredImages,
        Field::TransformedImages,
        Field::OverlappedImages,
        Field::InvalidSelectors,
    ];
    let number =
        |record: &crate::diagnostics::record::DiagnosticRecordV1, field: Field| match record
            .fields
            .get(&field)
        {
            Some(FieldValue::Number(value)) => Some(*value),
            _ => None,
        };
    let mut journals_checked = 0;
    for (id, kind, expected_marks, expected_refreshes) in [
        (
            "gallery",
            ScreenshotType::Viewport,
            vec![vec![4, 0, 1, 0, 0, 0, 0]; 3],
            vec![(3, 1), (4, 0)],
        ),
        (
            "popup",
            ScreenshotType::Viewport,
            vec![
                vec![1, 0, 0, 0, 0, 0, 0],
                vec![4, 0, 1, 0, 0, 0, 0],
                vec![4, 0, 1, 0, 0, 0, 0],
            ],
            vec![],
        ),
        ("sharp", ScreenshotType::Viewport, vec![], vec![]),
    ] {
        let (_, path) = load(id, &kind)?;
        let log = capture::read_capture_log(&output_dir, &path)
            .ok_or_else(|| anyhow!("{id}: missing capture log"))?;
        anyhow::ensure!(log.state == ArchiveState::Complete, "{id}: incomplete log");
        let archive = log.archive.ok_or_else(|| anyhow!("{id}: no archive"))?;
        let reader = JournalReader::open(&output_dir, &archive.operation_id)?;
        let mut marks = Vec::new();
        let mut refreshes = Vec::new();
        let mut leaked = false;
        reader.visit(|record| {
            if record
                .execution
                .as_ref()
                .is_some_and(|execution| execution.scenario_id.as_str() != id)
            {
                return;
            }
            for value in record.fields.values() {
                if let FieldValue::Identity(identity) = value {
                    leaked |= identity.as_str().contains("exclusion-marker");
                }
            }
            if record.event_name != DiagnosticEvent::Decision
                || record.fields.get(&Field::Condition)
                    != Some(&FieldValue::Kind(SafeValue::Images))
                || !record.fields.contains_key(&Field::SimplifiedImages)
            {
                return;
            }
            // Only the condition and counts are recorded.
            leaked |= record
                .fields
                .keys()
                .any(|field| *field != Field::Condition && !count_fields.contains(field));
            if record.stage == Stage::ScreenshotSettling {
                marks.push(
                    count_fields
                        .iter()
                        .map(|field| number(&record, *field).unwrap_or(u64::MAX))
                        .collect::<Vec<_>>(),
                );
            } else {
                refreshes.push((
                    number(&record, Field::SimplifiedImages).unwrap_or(u64::MAX),
                    number(&record, Field::OverlappedImages).unwrap_or(u64::MAX),
                ));
            }
        })?;
        anyhow::ensure!(
            !leaked,
            "{id}: image simplification diagnostics leaked data"
        );
        anyhow::ensure!(marks == expected_marks, "{id}: marks {marks:?}");
        anyhow::ensure!(
            refreshes == expected_refreshes,
            "{id}: refreshes {refreshes:?}"
        );
        journals_checked += 1;
    }
    anyhow::ensure!(journals_checked == 3);

    // Every capture link records the effective setting, and `ref` promoted it
    // with each reference image. Equal settings raise no report warning.
    let pixelated = json!({
        "mode": "pixelate",
        "blockSize": block,
        "minSize": 64,
        "excludeSelectorsDigest": crate::test::image_simplification::exclude_selectors_digest(&[
            ".keep-zone".into(),
            "img[data-secret=\"exclusion-marker\"]".into(),
        ]),
    });
    let sharp_setting = json!({ "mode": "none" });
    let expected_setting = |name: &str| {
        if name.starts_with("sharp_") {
            &sharp_setting
        } else {
            &pixelated
        }
    };
    for test in tests {
        let name = test["pair"]["fileName"].as_str().unwrap();
        for path in [
            suite_dir.join("bitmaps_reference").join(name),
            suite_dir.join("bitmaps_test/test").join(name),
        ] {
            let setting = capture::read_capture_metadata(&output_dir, &path)
                .and_then(|metadata| metadata.image_simplification)
                .ok_or_else(|| anyhow!("{}: no recorded setting", path.display()))?;
            anyhow::ensure!(
                serde_json::to_value(&setting)? == *expected_setting(name),
                "{}: recorded {setting:?}",
                path.display()
            );
            let link = std::fs::read_to_string(path.with_extension("log.json"))?;
            anyhow::ensure!(
                !link.contains("exclusion-marker") && !link.contains("keep-zone"),
                "{}: selector text in link",
                path.display()
            );
        }
        anyhow::ensure!(
            test["pair"].get("imageSimplificationWarning").is_none(),
            "{name}: equal settings must not warn"
        );
    }

    // Changing a setting without capturing new references warns in the report
    // and on the console, without failing the comparison or the command.
    std::fs::write(
        root.join("regressify.yaml"),
        workspace_config(block as u32 / 2),
    )?;
    let console = run("changed")?;
    let changed: Value = serde_json::from_slice(&std::fs::read(
        suite_dir.join("bitmaps_test/changed/report.json"),
    )?)?;
    let changed_tests = changed["tests"].as_array().unwrap();
    anyhow::ensure!(changed_tests.len() == 8, "{changed}");
    for test in changed_tests {
        let name = test["pair"]["fileName"].as_str().unwrap();
        let warning = test["pair"].get("imageSimplificationWarning");
        if name.starts_with("sharp_") {
            anyhow::ensure!(warning.is_none(), "{name}: none on both sides warned");
            continue;
        }
        let warning = warning.ok_or_else(|| anyhow!("{name}: missing settings warning"))?;
        anyhow::ensure!(
            warning["reference"] == pixelated
                && warning["test"]["blockSize"] == block / 2
                && warning["test"]["excludeSelectorsDigest"] == pixelated["excludeSelectorsDigest"],
            "{name}: {warning}"
        );
        anyhow::ensure!(
            console.contains(&format!(
                "{name} → Reference and test captures used different image simplification settings"
            )),
            "{name}: no console warning in {console}"
        );
    }
    anyhow::ensure!(
        console.contains("7 comparison(s) used different image simplification settings"),
        "{console}"
    );
    anyhow::ensure!(
        !console.contains("exclusion-marker") && !changed.to_string().contains("exclusion-marker"),
        "selector text leaked into the console or report"
    );
    Ok(())
}

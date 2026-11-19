use std::{collections::HashMap, sync::Arc, time::Duration};

use anyhow::{Result, anyhow};
use chromiumoxide::{
    Browser, Command, Connection,
    cdp::browser_protocol::target::SessionId,
    types::{CallId, EventMessage, Message, Method, MethodId},
};
use futures::StreamExt;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::{broadcast, mpsc, oneshot};

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Event {
    pub method: String,
    pub session_id: Option<String>,
    pub params: Value,
}

impl Method for Event {
    fn identifier(&self) -> MethodId {
        self.method.clone().into()
    }
}

impl EventMessage for Event {
    fn session_id(&self) -> Option<&str> {
        self.session_id.as_deref()
    }
}

struct Request {
    session: Option<String>,
    method: &'static str,
    params: Value,
    reply: oneshot::Sender<Result<Value>>,
}

struct ConnectionTask(tokio::task::JoinHandle<()>);

impl Drop for ConnectionTask {
    fn drop(&mut self) {
        self.0.abort();
    }
}

#[derive(Clone)]
pub(super) struct Client {
    browser_ws: Arc<str>,
    requests: mpsc::Sender<Request>,
    events: broadcast::Sender<Event>,
    _task: Arc<ConnectionTask>,
}

impl Client {
    pub async fn connect(browser: &Browser) -> Result<Self> {
        Self::connect_to(Arc::from(browser.websocket_address().as_str())).await
    }

    pub async fn isolated_connection(&self) -> Result<Self> {
        Self::connect_to(self.browser_ws.clone()).await
    }

    async fn connect_to(browser_ws: Arc<str>) -> Result<Self> {
        let mut connection = tokio::time::timeout(
            Duration::from_secs(30),
            Connection::<Event>::connect(browser_ws.as_ref()),
        )
        .await??;
        let (requests, mut receiver) = mpsc::channel::<Request>(64);
        let (events, _) = broadcast::channel(1024);
        let event_sender = events.clone();
        let task = tokio::spawn(async move {
            let mut pending: HashMap<CallId, oneshot::Sender<Result<Value>>> = HashMap::new();
            loop {
                tokio::select! {
                    request = receiver.recv() => {
                        let Some(request) = request else { break };
                        match connection.submit_command(request.method.into(), request.session.map(SessionId::from), request.params) {
                            Ok(id) => { pending.insert(id, request.reply); }
                            Err(_) => { let _ = request.reply.send(Err(anyhow!("Cannot serialize CDP command"))); }
                        }
                    }
                    message = connection.next() => {
                        match message {
                            Some(Ok(Message::Response(response))) => {
                                if let Some(reply) = pending.remove(&response.id) {
                                    let result = match response.error {
                                        Some(error) => Err(anyhow!("CDP command failed: {}", error.message)),
                                        None => Ok(response.result.unwrap_or(Value::Null)),
                                    };
                                    let _ = reply.send(result);
                                }
                            }
                            Some(Ok(Message::Event(event))) => { let _ = event_sender.send(event); }
                            _ => break,
                        }
                    }
                }
                pending.retain(|_, reply| !reply.is_closed());
            }
            for (_, reply) in pending {
                let _ = reply.send(Err(anyhow!("CDP connection closed")));
            }
        });
        Ok(Self {
            browser_ws,
            requests,
            events,
            _task: Arc::new(ConnectionTask(task)),
        })
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.events.subscribe()
    }

    pub async fn execute<C: Command>(&self, session: &str, command: C) -> Result<C::Response> {
        let std::borrow::Cow::Borrowed(method) = command.identifier() else {
            anyhow::bail!("CDP command identifier must be static");
        };
        let result = self
            .call(Some(session), method, serde_json::to_value(command)?)
            .await?;
        Ok(serde_json::from_value(result)?)
    }

    pub async fn call(
        &self,
        session: Option<&str>,
        method: &'static str,
        params: Value,
    ) -> Result<Value> {
        self.call_with_timeout(session, method, params, Duration::from_secs(30))
            .await
    }

    pub async fn call_all(
        &self,
        session: &str,
        commands: Vec<(&'static str, Value)>,
    ) -> Result<()> {
        tokio::time::timeout(Duration::from_secs(30), async {
            let mut responses = Vec::with_capacity(commands.len());
            for (method, params) in commands {
                let (reply, response) = oneshot::channel();
                self.requests
                    .send(Request {
                        session: Some(session.to_string()),
                        method,
                        params,
                        reply,
                    })
                    .await
                    .map_err(|_| anyhow!("CDP connection closed during {method}"))?;
                responses.push((method, response));
            }
            // Paused targets can defer setup replies until runIfWaitingForDebugger.
            // Send the complete ordered batch before awaiting any of its replies.
            for (method, response) in responses {
                response
                    .await
                    .map_err(|_| anyhow!("CDP connection closed during {method}"))??;
            }
            Ok(())
        })
        .await
        .map_err(|_| anyhow!("Timed out initializing CDP target"))?
    }

    pub async fn call_with_timeout(
        &self,
        session: Option<&str>,
        method: &'static str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value> {
        let (reply, response) = oneshot::channel();
        tokio::time::timeout(timeout, async {
            self.requests
                .send(Request {
                    session: session.map(str::to_string),
                    method,
                    params,
                    reply,
                })
                .await
                .map_err(|_| anyhow!("CDP connection closed during {}", method))?;
            response
                .await
                .map_err(|_| anyhow!("CDP connection closed during {}", method))?
        })
        .await
        .map_err(|_| anyhow!("Timed out waiting for CDP {}", method))?
    }

    pub async fn attach(&self, target: &str) -> Result<String> {
        let response = self
            .call(
                None,
                "Target.attachToTarget",
                json!({"targetId": target, "flatten": true}),
            )
            .await?;
        response
            .get("sessionId")
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| anyhow!("CDP did not return an attached session"))
    }
}

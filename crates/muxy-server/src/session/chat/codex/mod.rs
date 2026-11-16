//! Native Codex engine ownership, hydration and cooperating-client requests.

pub(super) mod context;
mod normalize;
mod transport;

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Sender, SyncSender};
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant, SystemTime};

use muxy_protocol::{
    AgentState, ChatCapabilities, ChatEngine, ChatEvent, ChatImage, ChatItem, ChatItemKind,
    ChatMode, ChatModel, ChatProvider, ChatRefusal, ChatReply, ChatRequest, ChatSelection,
    ChatSnapshot, ChatState, ChatTarget, ChatThread, Feature, MAX_CHAT_EFFORTS, MAX_CHAT_MODELS,
    MAX_CHAT_NAME, MAX_CHAT_PREVIEW, MAX_CHAT_SNAPSHOT_BYTES, MetadataEvent, OperationId,
    PermissionResponse, ServerPath, SessionInfo, SubmissionOutcome,
};
use muxy_terminal::pty::Pty;
use serde_json::{Value, json};

use self::context::Context;
use self::normalize::Parser;
use self::transport::{Client, Error};
use super::boundary::Change;
use super::flush::{FlushTarget, gate_and_flush};
use super::host::base64;
use super::normalize::{clip, str_at};
use super::state::ChatLog;
use super::supervisor::{foreground_group, wait_foreground_is};
use crate::connection::Connection;
use crate::session::input_gate::InputGate;
use crate::session::{ChatCommand, ChatReplyTo, OwnerEvent};

const TIMEOUT: Duration = Duration::from_secs(20);

fn refused(detail: impl AsRef<str>) -> ChatReply {
    ChatReply::Refused {
        reason: ChatRefusal::UnsupportedEnvironment,
        detail: Some(clip(detail.as_ref(), MAX_CHAT_PREVIEW)),
    }
}

fn send(events: &Sender<OwnerEvent>, event: Event) {
    let _ = events.send(OwnerEvent::Chat(super::host::Event::Codex(Box::new(event))));
}

pub(crate) enum Event {
    Ready {
        token: u64,
        result: Result<Box<Prepared>, String>,
        reply: ChatReplyTo,
    },
    Changes {
        token: u64,
        changes: Vec<Change>,
        permit: Permit,
    },
    Rpc {
        token: u64,
        id: u64,
        result: Result<Value, Error>,
    },
    Approval {
        token: u64,
        id: u64,
        result: Result<(), String>,
    },
    Accepted {
        token: u64,
        operation: OperationId,
    },
    Fault {
        token: u64,
        error: Error,
    },
    Left {
        token: u64,
        result: Result<bool, String>,
        reply: ChatReplyTo,
    },
    Resynced {
        token: u64,
        result: Result<(Vec<Change>, bool), String>,
    },
}

impl std::fmt::Debug for Event {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        out.write_str("CodexEvent")
    }
}

#[allow(
    clippy::struct_excessive_bools,
    reason = "Independent history, persistence and recovery acknowledgments"
)]
pub(crate) struct Prepared {
    context: Context,
    client: Client,
    parser: Parser,
    changes: Vec<Change>,
    capabilities: ChatCapabilities,
    fresh: bool,
    since: SystemTime,
    stopped: bool,
    recovery_typed: bool,
    has_earlier: bool,
    blocked: Option<String>,
    session_tree: String,
}

struct Subscriber {
    outbox: Weak<Connection>,
    features: Vec<Feature>,
}
enum PendingCall {
    Submit(OperationId),
    Reconcile(OperationId),
    Model(String, ChatReplyTo),
    Effort(String, ChatReplyTo),
    Other(ChatReplyTo),
}

pub(crate) struct Host {
    info: SessionInfo,
    events: Sender<OwnerEvent>,
    gate: Arc<InputGate>,
    target: Arc<FlushTarget>,
    token: u64,
    generation: u64,
    mode: ChatMode,
    changed: bool,
    log: ChatLog,
    actor: Option<Actor>,
    next: u64,
    calls: HashMap<u64, PendingCall>,
    subscribers: Vec<Subscriber>,
    activity: Option<AgentState>,
    shared: bool,
}

impl Host {
    #[allow(
        clippy::too_many_arguments,
        reason = "Captures the owner and terminal context before asynchronous preflight"
    )]
    pub(crate) fn open(
        info: SessionInfo,
        events: Sender<OwnerEvent>,
        gate: Arc<InputGate>,
        pty: &Pty,
        selection: ChatSelection,
        fresh: bool,
        token: u64,
        generation: u64,
        reply: ChatReplyTo,
    ) -> Result<Self, ChatReplyTo> {
        let Ok(target) = FlushTarget::new(pty) else {
            reply.send(refused("This terminal cannot switch to Codex chat"));
            return Err(ChatReplyTo::Nobody);
        };
        let target = Arc::new(target);
        let base = pty.environment().to_vec();
        let shell = pty.child_pid();
        let foreground = pty.foreground_pid();
        let worker_events = events.clone();
        let worker_gate = gate.clone();
        let worker_target = target.clone();
        let (reply_send, reply_receive) = mpsc::channel();
        if std::thread::Builder::new()
            .name("codex-preflight".into())
            .spawn(move || {
                let result = prepare(
                    shell,
                    foreground,
                    &selection,
                    &base,
                    fresh,
                    token,
                    &worker_gate,
                    &worker_target,
                );
                if let Ok(reply) = reply_receive.recv() {
                    send(
                        &worker_events,
                        Event::Ready {
                            token,
                            result: result.map(Box::new),
                            reply,
                        },
                    );
                }
            })
            .is_err()
        {
            return Err(reply);
        }
        let _ = reply_send.send(reply);
        Ok(Self {
            info,
            events,
            gate,
            target,
            token,
            generation,
            mode: ChatMode::Transitioning,
            changed: true,
            log: ChatLog::default(),
            actor: None,
            next: 0,
            calls: HashMap::new(),
            subscribers: Vec::new(),
            activity: None,
            shared: false,
        })
    }

    pub(crate) fn mode(&self) -> ChatMode {
        self.mode
    }
    pub(crate) fn generation(&self) -> u64 {
        self.generation
    }
    pub(crate) fn mode_event(&self) -> MetadataEvent {
        MetadataEvent::ChatMode {
            mode: self.mode,
            generation: self.generation,
        }
    }
    pub(crate) fn take_mode_change(&mut self) -> Option<MetadataEvent> {
        std::mem::take(&mut self.changed).then(|| self.mode_event())
    }
    pub(crate) fn activity(&mut self) -> Option<(AgentState, bool)> {
        if self.mode == ChatMode::Console {
            return None;
        }
        let state = if self.mode == ChatMode::Blocked || self.log.pending().next().is_some() {
            AgentState::Blocked
        } else if self.mode == ChatMode::Transitioning || !self.log.is_quiet() {
            AgentState::Working
        } else {
            AgentState::Idle
        };
        let completed = state == AgentState::Idle
            && matches!(
                self.activity,
                Some(AgentState::Working | AgentState::Blocked)
            );
        self.activity = Some(state);
        Some((state, completed))
    }

    #[allow(
        clippy::too_many_lines,
        reason = "Native request dispatch keeps capability and generation gates together"
    )]
    pub(crate) fn command(&mut self, command: ChatCommand) {
        let ChatCommand {
            request,
            from,
            reply,
        } = command;
        if let ChatRequest::Leave { .. } = &request
            && !self.authorized(&from)
        {
            reply.send(ChatReply::Left {resumed:false,detail:Some("Update Muxy to control Codex chat. This Console is read-only while Chat owns the session.".into())});
            if let Some(outbox) = from.upgrade() {
                outbox.chat_console_read_only(self.info.id, self.generation);
            }
            return;
        }
        if let ChatRequest::Subscribe {
            subscription,
            features,
            ..
        } = request
        {
            if !features.contains(&Feature::CHAT_PROVIDERS)
                || !features.contains(&Feature::CHAT_NATIVE_APPROVALS)
                || !features.contains(&Feature::CHAT_QUESTIONS)
            {
                if let Some(outbox) = from.upgrade() {
                    outbox.chat_subscribe(muxy_protocol::ChatBatch {session:self.info.id,generation:self.generation,
                        from_rev:0,to_rev:0,events:Vec::new(),snapshot:Some(Box::new(ChatSnapshot {subscription,
                            items:vec![ChatItem {id:"muxy:upgrade".into(),turn_id:"muxy:upgrade".into(),seq:0,
                                kind:ChatItemKind::Notice {text:"Update Muxy to view and control this Codex chat. Console is available read-only while Chat owns the session.".into()}}],
                            state:ChatState::default(),capabilities:ChatCapabilities::default(),pending:Vec::new(),has_earlier:false}))});
                }
                return reply.send(ChatReply::Subscribed {
                    generation: self.generation,
                    subscription,
                });
            }
            let Some(outbox) = from.upgrade() else { return };
            let snapshot = self.log.snapshot(
                self.info.id,
                self.generation,
                subscription,
                MAX_CHAT_SNAPSHOT_BYTES,
            );
            self.subscribers
                .retain(|subscriber| !subscriber.outbox.ptr_eq(&from));
            if outbox.chat_subscribe(snapshot) {
                self.subscribers.push(Subscriber {
                    outbox: from,
                    features,
                });
            }
            return reply.send(ChatReply::Subscribed {
                generation: self.generation,
                subscription,
            });
        }
        if let ChatRequest::Unsubscribe { session } = request {
            self.subscribers
                .retain(|subscriber| !subscriber.outbox.ptr_eq(&from));
            if let Some(outbox) = from.upgrade() {
                outbox.chat_unsubscribe(session);
            }
            return reply.send(ChatReply::Ok);
        }
        let generation = match &request {
            ChatRequest::Send { generation, .. }
            | ChatRequest::Submit { generation, .. }
            | ChatRequest::Respond { generation, .. }
            | ChatRequest::SetModel { generation, .. }
            | ChatRequest::SetEffort { generation, .. }
            | ChatRequest::Interrupt { generation, .. }
            | ChatRequest::LeaveProvider { generation, .. } => *generation,
            _ => return reply.send(refused("Update Muxy to control this Codex conversation")),
        };
        if generation != self.generation {
            return reply.send(ChatReply::Refused {
                reason: ChatRefusal::StaleGeneration,
                detail: None,
            });
        }
        if !self.authorized(&from) {
            return reply.send(refused(
                "Subscribe with Codex approval capabilities before changing this conversation",
            ));
        }
        match request {
            ChatRequest::Respond {
                request_id,
                response,
                ..
            } => {
                self.next += 1;
                let id = self.next;
                let queued = self.actor.as_ref().is_some_and(|actor| {
                    actor.send(
                        Command::Respond {
                            id,
                            key: request_id,
                            response,
                        },
                        1024,
                    )
                });
                if queued {
                    self.calls.insert(id, PendingCall::Other(reply));
                } else {
                    reply.send(refused("Codex is not taking input"));
                }
            }
            ChatRequest::SetModel { model, .. } => {
                if !self.log.is_quiet()
                    || !self
                        .log
                        .capabilities()
                        .models
                        .iter()
                        .any(|offered| offered.alias == model)
                {
                    return reply.send(refused("Choose an offered model while Codex is idle"));
                }
                self.call(
                    "thread/settings/update",
                    json!({"model":model}),
                    PendingCall::Model(model, reply),
                );
            }
            ChatRequest::SetEffort { effort, .. } => {
                let model = self.log.current_state().model.as_ref();
                if !self.log.is_quiet()
                    || !self.log.capabilities().models.iter().any(|offered| {
                        Some(&offered.alias) == model && offered.efforts.contains(&effort)
                    })
                {
                    return reply.send(refused(
                        "Choose an offered effort level while Codex is idle",
                    ));
                }
                self.call(
                    "thread/settings/update",
                    json!({"effort":effort}),
                    PendingCall::Effort(effort, reply),
                );
            }
            ChatRequest::Interrupt { .. } => {
                self.call("muxy/interrupt", json!({}), PendingCall::Other(reply));
            }
            ChatRequest::LeaveProvider { interrupt, .. } => {
                if self.mode == ChatMode::Transitioning {
                    return reply.send(refused("A switch is already running"));
                }
                let Some(actor) = self.actor.as_ref() else {
                    return reply.send(refused("The Codex connection is unavailable"));
                };
                if !actor.send(Command::Leave { interrupt, reply }, 1024) {
                    return;
                }
                self.mode = ChatMode::Transitioning;
                self.changed = true;
            }
            _ => reply.send(refused(
                "Use acknowledged submission operations to send to Codex",
            )),
        }
    }

    pub(crate) fn authorized(&self, from: &Weak<Connection>) -> bool {
        self.subscribers.iter().any(|subscriber| {
            subscriber.outbox.ptr_eq(from)
                && subscriber.features.contains(&Feature::CHAT_PROVIDERS)
                && subscriber
                    .features
                    .contains(&Feature::CHAT_NATIVE_APPROVALS)
                && subscriber.features.contains(&Feature::CHAT_QUESTIONS)
        })
    }

    pub(crate) fn submit(&mut self, operation: OperationId, text: &str, images: Vec<ChatImage>) {
        if self.mode != ChatMode::Chat
            || self
                .calls
                .values()
                .any(|call| matches!(call, PendingCall::Submit(_)))
        {
            return self.update(
                operation,
                SubmissionOutcome::Rejected,
                Some("Codex is not ready for another message".into()),
            );
        }
        let state = self.log.current_state();
        if !images.is_empty()
            && self
                .log
                .capabilities()
                .models
                .iter()
                .find(|model| Some(&model.alias) == state.model.as_ref())
                .is_none_or(|model| model.images != Some(true))
        {
            return self.update(
                operation,
                SubmissionOutcome::Rejected,
                Some("The selected Codex model does not offer image input".into()),
            );
        }
        let mut input = Vec::new();
        if !text.is_empty() {
            input.push(json!({"type":"text","text":text,"text_elements":[]}));
        }
        for image in images {
            input.push(json!({"type":"image","url":format!("data:{};base64,{}",image.mime.mime(),base64(&image.bytes))}));
        }
        self.call(
            "turn/start",
            json!({"input":input,"model":state.model,"effort":state.effort,
            "clientUserMessageId":super::submissions::native_id(operation)}),
            PendingCall::Submit(operation),
        );
    }

    pub(crate) fn reconcile(&mut self, operation: OperationId) {
        if !self
            .calls
            .values()
            .any(|call| matches!(call,PendingCall::Reconcile(id) if *id==operation))
        {
            self.call(
                "muxy/reconcile",
                json!({"clientId":super::submissions::native_id(operation)}),
                PendingCall::Reconcile(operation),
            );
        }
    }

    fn call(&mut self, method: &str, params: Value, call: PendingCall) {
        self.next += 1;
        let id = self.next;
        let bytes = params.to_string().len();
        if self.calls.len() >= 32
            || !self.actor.as_ref().is_some_and(|actor| {
                actor.send(
                    Command::Call {
                        id,
                        method: method.into(),
                        params,
                    },
                    bytes,
                )
            })
        {
            match call {
                PendingCall::Submit(operation) => self.update(
                    operation,
                    SubmissionOutcome::Rejected,
                    Some("Codex input queue is full".into()),
                ),
                PendingCall::Reconcile(_) => {}
                PendingCall::Model(_, reply)
                | PendingCall::Effort(_, reply)
                | PendingCall::Other(reply) => reply.send(refused("Codex input queue is full")),
            }
        } else {
            self.calls.insert(id, call);
        }
    }

    fn update(&self, operation: OperationId, outcome: SubmissionOutcome, detail: Option<String>) {
        let _ = self
            .events
            .send(OwnerEvent::Chat(super::host::Event::Submission {
                operation,
                outcome,
                detail,
            }));
    }

    #[allow(
        clippy::too_many_lines,
        reason = "Native completion dispatch shares one ownership state machine"
    )]
    pub(crate) fn event(&mut self, event: Event) {
        match event {
            Event::Ready {
                token,
                result,
                reply,
            } if token == self.token => match result {
                Ok(mut prepared) => {
                    let blocked = prepared.blocked.take();
                    self.shared = prepared.client.endpoint.is_some();
                    self.log.seed_changes(std::mem::take(&mut prepared.changes));
                    if prepared.has_earlier {
                        self.log.earlier();
                    }
                    let capabilities = prepared.capabilities.clone();
                    self.log.apply_changes([Change::Capabilities(capabilities)]);
                    if let Ok(actor) = Actor::spawn(
                        *prepared,
                        self.token,
                        self.events.clone(),
                        self.gate.clone(),
                        self.target.clone(),
                    ) {
                        self.actor = Some(actor);
                        self.mode = if blocked.is_some() {
                            ChatMode::Blocked
                        } else {
                            ChatMode::Chat
                        };
                        self.changed = true;
                        if let Some(error) = blocked {
                            let events = self.log.blocked(&error);
                            self.publish(&events);
                            reply.send(refused(error));
                        } else {
                            reply.send(ChatReply::Entered {
                                generation: self.generation,
                            });
                        }
                    } else {
                        self.mode = ChatMode::Blocked;
                        self.changed = true;
                        reply.send(refused("Could not start the Codex connection worker"));
                    }
                }
                Err(error) => {
                    self.mode = ChatMode::Console;
                    self.changed = true;
                    self.gate.open();
                    reply.send(refused(error));
                }
            },
            Event::Changes {
                token,
                changes,
                permit: _permit,
            } if token == self.token => {
                let events = self.log.apply_changes(changes);
                self.publish(&events);
            }
            Event::Accepted { token, operation } if token == self.token => {
                self.update(operation, SubmissionOutcome::Accepted, None);
            }
            Event::Rpc { token, id, result } if token == self.token => {
                let Some(call) = self.calls.remove(&id) else {
                    return;
                };
                match call {
                    PendingCall::Reconcile(operation)=>{if result.is_ok_and(|value|value["found"]==true){self.update(operation,SubmissionOutcome::Accepted,None);}},
                    PendingCall::Submit(operation)=>match result {
                        Ok(value) if value["turn"]["id"].is_string()=>self.update(operation,SubmissionOutcome::Accepted,None),
                        Err(Error::Refused)=>self.update(operation,SubmissionOutcome::Rejected,Some("Codex rejected this message".into())),
                        _=>self.update(operation,SubmissionOutcome::Uncertain,Some("Codex acceptance is uncertain. Check native history; the message has not been resent.".into()))
                    },
                    PendingCall::Model(model,reply)|PendingCall::Effort(model,reply)=> {
                        if result.is_ok(){reply.send(ChatReply::Ok);}else{reply.send(refused("Codex refused this setting"));}
                        let _=model;
                    }
                    PendingCall::Other(reply)=>reply.send(if result.is_ok(){ChatReply::Ok}else{refused("Codex could not confirm this request")}),
                }
            }
            Event::Approval { token, id, result } if token == self.token => {
                if let Some(PendingCall::Other(reply)) = self.calls.remove(&id) {
                    reply.send(result.map_or_else(refused, |()| ChatReply::Ok));
                }
            }
            Event::Fault { token, error } if token == self.token => {
                self.mode = ChatMode::Blocked;
                self.changed = true;
                let events = self.log.blocked(&error.to_string());
                self.publish(&events);
                if let Some(actor) = &self.actor {
                    actor.send(Command::Resync, 64);
                }
            }
            Event::Resynced { token, result } if token == self.token => match result {
                Ok((changes, earlier)) => {
                    let capabilities = self.log.capabilities().clone();
                    self.log = ChatLog::default();
                    self.log.seed_changes(changes);
                    if earlier {
                        self.log.earlier();
                    }
                    self.log
                        .seed_changes(vec![Change::Capabilities(capabilities)]);
                    self.generation += 1;
                    self.subscribers.clear();
                    self.mode = ChatMode::Chat;
                    self.changed = true;
                }
                Err(error) => {
                    let events = self.log.blocked(&error);
                    self.publish(&events);
                }
            },
            Event::Left {
                token,
                result,
                reply,
            } if token == self.token => {
                if let Err(error) = result {
                    self.mode = ChatMode::Blocked;
                    self.changed = true;
                    reply.send(refused(error));
                } else {
                    self.mode = ChatMode::Console;
                    self.changed = true;
                    self.gate.open();
                    self.actor = None;
                    reply.send(ChatReply::Left {
                        resumed: result.unwrap_or(false),
                        detail: None,
                    });
                }
            }
            _ => {}
        }
    }

    fn publish(&mut self, events: &[ChatEvent]) {
        if events.is_empty() {
            return;
        }
        let to = self.log.revision();
        let from = to - events.len() as u64;
        self.subscribers.retain(|subscriber| {
            subscriber.outbox.upgrade().is_some_and(|outbox| {
                outbox.push_chat(self.info.id, self.generation, (from, to), events.to_vec())
            })
        });
    }
}

struct Actor {
    commands: SyncSender<(Command, Permit)>,
    budget: Arc<AtomicUsize>,
}
enum Command {
    Call {
        id: u64,
        method: String,
        params: Value,
    },
    Respond {
        id: u64,
        key: String,
        response: PermissionResponse,
    },
    Leave {
        interrupt: bool,
        reply: ChatReplyTo,
    },
    Resync,
}
pub(crate) struct Permit {
    budget: Arc<AtomicUsize>,
    bytes: usize,
}
impl Drop for Permit {
    fn drop(&mut self) {
        self.budget.fetch_sub(self.bytes, Ordering::AcqRel);
    }
}
fn reserve(budget: &Arc<AtomicUsize>, bytes: usize, limit: usize) -> Option<Permit> {
    let mut current = budget.load(Ordering::Acquire);
    loop {
        if current.checked_add(bytes)? > limit {
            return None;
        }
        match budget.compare_exchange_weak(
            current,
            current + bytes,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => {
                return Some(Permit {
                    budget: budget.clone(),
                    bytes,
                });
            }
            Err(actual) => current = actual,
        }
    }
}

impl Actor {
    fn send(&self, command: Command, bytes: usize) -> bool {
        let Some(permit) = reserve(&self.budget, bytes, 64 << 20) else {
            return false;
        };
        self.commands.try_send((command, permit)).is_ok()
    }
    fn spawn(
        mut prepared: Prepared,
        token: u64,
        events: Sender<OwnerEvent>,
        gate: Arc<InputGate>,
        target: Arc<FlushTarget>,
    ) -> Result<Self, ()> {
        let (commands, queued) = mpsc::sync_channel(64);
        let budget = Arc::new(AtomicUsize::new(0));
        std::thread::Builder::new().name("codex-engine".into()).spawn(move||{
            let output=Arc::new(AtomicUsize::new(0));let mut failed=false;let mut resyncs=0;
            loop {
                match queued.try_recv(){
                    Ok((Command::Call {id,method,mut params},_permit))=>{
                        params["threadId"]=json!(prepared.parser.thread());
                        let settings=(method=="thread/settings/update").then(||params.clone());
                        let result=if method=="muxy/interrupt" {interrupt(&mut prepared.client,prepared.parser.thread(),Instant::now()+TIMEOUT)}
                            else if method=="muxy/reconcile"{reconcile(&mut prepared.client,prepared.parser.thread(),&params)}
                            else {prepared.client.request(&method,params,Instant::now()+TIMEOUT)};
                        if method=="turn/start" && result.as_ref().is_ok_and(|result|result["turn"]["id"].is_string()){prepared.fresh=false;}
                        if result.is_ok()&&let Some(settings)=settings{
                            let state=prepared.parser.state();
                            let change=prepared.parser.configure(str_at(&settings,"model").map(str::to_owned).or_else(||state.model.clone()),
                                str_at(&settings,"effort").map(str::to_owned).or_else(||state.effort.clone()),state.busy);
                            if let Some(permit)=reserve(&output,1024,transport::MAX_BUFFER){send(&events,Event::Changes {token,changes:vec![change],permit});}
                        }
                        send(&events,Event::Rpc {token,id,result});
                        for message in prepared.client.take_buffered(){
                            if message["params"]["threadId"].as_str()==Some(prepared.parser.thread())&&message["params"]["item"]["type"]=="userMessage"{prepared.fresh=false;}
                            if !emit(&events,token,&mut prepared.parser,&message,&output){failed=true;}
                        }
                    }
                    Ok((Command::Respond {id,key,response},_permit))=>{
                        let mut failed_send=false;
                        let result=prepared.parser.response(&key,response).map_err(str::to_owned)
                            .and_then(|response|prepared.client.send(&response).map_err(|error|{failed_send=true;error.to_string()}));
                        send(&events,Event::Approval {token,id,result});
                        if failed_send {failed=true;send(&events,Event::Fault {token,error:Error::Io});}
                    }
                    Ok((Command::Leave {interrupt:confirmed,reply},_permit))=>{
                        let result=leave(&mut prepared,&gate,&target,confirmed);
                        let success=result.is_ok();send(&events,Event::Left {token,result,reply});
                        if success {break;}
                        if prepared.stopped{failed=true;}
                    }
                    Ok((Command::Resync,_permit))=>{
                        if resyncs>=3{continue;}resyncs+=1;
                        let result=(||{
                            if prepared.client.endpoint.is_some(){
                                prepared.client.revalidate().map_err(|error|error.to_string())?;
                                let replacement=Client::connect(&prepared.context,Instant::now()+TIMEOUT).map_err(|error|error.to_string())?;
                                if replacement.engine!=prepared.client.engine{return Err("The verified Codex engine changed; recovery is blocked".into());}
                                prepared.client=replacement;
                            }
                            prepared.client.request("thread/unsubscribe",json!({"threadId":prepared.parser.thread()}),Instant::now()+TIMEOUT).map_err(|error|error.to_string())?;
                            let response=prepared.client.request("thread/resume",json!({"threadId":prepared.parser.thread(),"excludeTurns":true,
                                "initialTurnsPage":{"limit":5,"itemsView":"full"}}),Instant::now()+TIMEOUT).map_err(|error|error.to_string())?;
                            let mut epoch=[0;8];getrandom::fill(&mut epoch).map_err(|_|"Could not establish a fresh request epoch")?;
                            let mut parser=Parser::new(prepared.parser.thread().into(),u64::from_ne_bytes(epoch));
                            if let Some(target)=prepared.parser.state().target.clone(){parser.target(target,prepared.client.endpoint.is_some());}
                            let (mut changes,earlier)=hydrate(&mut prepared.client,&response,&mut parser)?;
                            changes.push(parser.configure(str_at(&response,"model").map(str::to_owned),str_at(&response,"reasoningEffort").map(str::to_owned),response["thread"]["status"]["type"]=="active"));
                            for message in prepared.client.take_buffered(){changes.extend(parser.event(&message));}
                            Ok((parser,changes,earlier))
                        })();
                        match result{
                            Ok((parser,changes,earlier))=>{
                                prepared.parser=parser;failed=false;
                                send(&events,Event::Resynced {token,result:Ok((changes,earlier))});
                            }
                            Err(error)=>send(&events,Event::Resynced {token,result:Err(error)}),
                        }
                    }
                    Err(mpsc::TryRecvError::Disconnected)=>break,
                    Err(mpsc::TryRecvError::Empty)=>{},
                }
                if failed {std::thread::sleep(Duration::from_millis(50));continue;}
                match prepared.client.poll(Instant::now()+Duration::from_millis(50)){
                    Ok(Some(message))=>{
                        if message["params"]["threadId"].as_str()==Some(prepared.parser.thread())&&message["params"]["item"]["type"]=="userMessage"{prepared.fresh=false;}
                        if !emit(&events,token,&mut prepared.parser,&message,&output){failed=true;}
                    }
                    Ok(None)=>{},Err(error)=>{failed=true;send(&events,Event::Fault {token,error});}
                }
            }
        }).map_err(|_|())?;
        Ok(Self { commands, budget })
    }
}

fn emit(
    events: &Sender<OwnerEvent>,
    token: u64,
    parser: &mut Parser,
    message: &Value,
    budget: &Arc<AtomicUsize>,
) -> bool {
    if message["params"]["threadId"].as_str() != Some(parser.thread()) {
        return true;
    }
    if message["params"]["item"]["type"] == "userMessage"
        && let Some(operation) = message["params"]["item"]["clientId"]
            .as_str()
            .and_then(|id| id.parse().ok())
    {
        send(events, Event::Accepted { token, operation });
    }
    let bytes = message.to_string().len();
    let Some(permit) = reserve(budget, bytes, transport::MAX_BUFFER) else {
        send(
            events,
            Event::Fault {
                token,
                error: Error::Overflow,
            },
        );
        return false;
    };
    let changes = parser.event(message);
    if !changes.is_empty() {
        send(
            events,
            Event::Changes {
                token,
                changes,
                permit,
            },
        );
    }
    true
}

pub(crate) fn discover(
    shell: u32,
    foreground: Option<u32>,
    epoch: u64,
    base: &[(std::ffi::OsString, std::ffi::OsString)],
) -> Result<ChatReply, String> {
    let context = if foreground == Some(shell) {
        Context::shell(shell, TIMEOUT, base)
    } else {
        Context::terminal(shell, foreground, None)
    }
    .map_err(str::to_owned)?;
    let mut client =
        Client::connect(&context, Instant::now() + TIMEOUT).map_err(|error| error.to_string())?;
    preflight(&mut client, &context)?;
    let (capabilities, model, effort) = models(&mut client)?;
    let mut threads = Vec::new();
    if let Some(endpoint) = client.endpoint.clone() {
        let listing = page(
            &mut client,
            "thread/list",
            json!({"cwd":context.cwd,"modelProviders":["openai"],"sourceKinds":["cli","appServer"]}),
            200,
        )?;
        for thread in listing {
            let Some(id) = str_at(&thread, "id")
                .filter(|id| crate::session::metadata::agent_session::uuid_shaped(id))
            else {
                continue;
            };
            if thread["modelProvider"] != "openai" {
                continue;
            }
            threads.push(ChatThread {
                target: ChatTarget {
                    provider: ChatProvider::Codex,
                    thread_id: id.into(),
                    home: ServerPath::from(context.home.clone()),
                    executable: ServerPath::from(context.executable.clone()),
                    endpoint: Some(ServerPath::from(endpoint.clone())),
                    engine: Some(engine_target(&client.engine)),
                },
                title: clip(
                    str_at(&thread, "name")
                        .or_else(|| str_at(&thread, "preview"))
                        .unwrap_or("Codex conversation"),
                    MAX_CHAT_PREVIEW,
                ),
                updated_at: thread["updatedAt"].as_i64().unwrap_or(0),
                busy: thread["status"]["type"] == "active",
            });
        }
    }
    Ok(ChatReply::Discovered {
        host_epoch: epoch,
        provider: ChatProvider::Codex,
        capabilities,
        model,
        effort,
        threads,
        launch_context: Some(context.launch_stamp()),
    })
}

fn preflight(client: &mut Client, context: &Context) -> Result<(), String> {
    let config = client
        .request(
            "config/read",
            json!({"cwd":context.cwd,"includeLayers":false}),
            Instant::now() + TIMEOUT,
        )
        .map_err(|error| error.to_string())?;
    if config["config"]["model_provider"]
        .as_str()
        .is_some_and(|provider| provider != "openai")
    {
        return Err("This Codex context uses a custom backend; keep it in Console".into());
    }
    let account = client
        .request(
            "account/read",
            json!({"refreshToken":false}),
            Instant::now() + TIMEOUT,
        )
        .map_err(|error| error.to_string())?;
    check_account(&account, &context.env).map_err(str::to_owned)
}

fn check_account(
    account: &Value,
    env: &[(std::ffi::OsString, std::ffi::OsString)],
) -> Result<(), &'static str> {
    if account["account"].is_null() {
        return Err("Sign in with `codex login` in Console first");
    }
    let inherited_key = env.iter().any(|(name, value)| {
        !value.is_empty() && matches!(name.to_str(), Some("OPENAI_API_KEY" | "CODEX_API_KEY"))
    });
    if inherited_key && account["account"]["type"] != "chatgpt" {
        return Err(
            "This terminal sets API credentials. Sign in to Codex with ChatGPT to use your subscription, or use Console for API billing",
        );
    }
    Ok(())
}

fn models(
    client: &mut Client,
) -> Result<(ChatCapabilities, Option<String>, Option<String>), String> {
    let data = page(
        client,
        "model/list",
        json!({"modelProvider":"openai"}),
        MAX_CHAT_MODELS,
    )?;
    let default = data
        .iter()
        .find(|model| model["isDefault"] == true && model["hidden"] != true);
    let model = default
        .and_then(|model| str_at(model, "model"))
        .map(str::to_owned);
    let effort = default
        .and_then(|model| str_at(model, "defaultReasoningEffort"))
        .map(str::to_owned);
    let models = data
        .iter()
        .filter(|model| model["hidden"] != true)
        .filter_map(|model| {
            let alias = str_at(model, "model")?;
            if alias.len() > MAX_CHAT_NAME {
                return None;
            }
            Some(ChatModel {
                alias: alias.into(),
                label: clip(str_at(model, "displayName").unwrap_or(alias), MAX_CHAT_NAME),
                efforts: model["supportedReasoningEfforts"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|effort| str_at(effort, "reasoningEffort"))
                    .filter(|effort| effort.len() <= MAX_CHAT_NAME)
                    .take(MAX_CHAT_EFFORTS)
                    .map(str::to_owned)
                    .collect(),
                default_effort: str_at(model, "defaultReasoningEffort").map(str::to_owned),
                images: Some(
                    model["inputModalities"]
                        .as_array()
                        .is_some_and(|values| values.iter().any(|value| value == "image")),
                ),
            })
        })
        .collect();
    Ok((
        ChatCapabilities {
            models,
            commands: Vec::new(),
        },
        model,
        effort,
    ))
}

fn page(
    client: &mut Client,
    method: &str,
    mut params: Value,
    maximum: usize,
) -> Result<Vec<Value>, String> {
    let mut data = Vec::new();
    let mut seen = HashSet::new();
    let deadline = Instant::now() + TIMEOUT;
    params["limit"] = json!(maximum.min(50));
    for _ in 0..16 {
        let result = client
            .request(method, params.clone(), deadline)
            .map_err(|error| error.to_string())?;
        for value in result["data"]
            .as_array()
            .ok_or("Codex returned an invalid page")?
        {
            if data.len() >= maximum {
                return Ok(data);
            }
            data.push(value.clone());
        }
        let Some(cursor) = str_at(&result, "nextCursor") else {
            return Ok(data);
        };
        if !seen.insert(cursor.to_owned()) {
            return Err("Codex repeated a pagination cursor".into());
        }
        params["cursor"] = json!(cursor);
    }
    Err("Codex history exceeded its pagination budget".into())
}

fn engine_target(identity: &context::Identity) -> ChatEngine {
    ChatEngine {
        pid: u32::try_from(identity.pid).unwrap_or(0),
        started_seconds: identity.started.0,
        started_micros: identity.started.1,
    }
}

fn hydrate(
    client: &mut Client,
    response: &Value,
    parser: &mut Parser,
) -> Result<(Vec<Change>, bool), String> {
    let deadline = Instant::now() + TIMEOUT;
    let mut turns = Vec::new();
    let mut bytes = 0;
    let mut earlier = false;
    let mut page = response
        .get("initialTurnsPage")
        .filter(|page| page.is_object())
        .cloned()
        .unwrap_or_else(|| json!({"data":response["thread"]["turns"],"nextCursor":null}));
    let descending = response["initialTurnsPage"].is_object();
    let mut cursors = HashSet::new();
    for index in 0..8 {
        for turn in page["data"]
            .as_array()
            .ok_or("Codex returned invalid turn history")?
        {
            let size = turn.to_string().len();
            if bytes + size > MAX_CHAT_SNAPSHOT_BYTES / 2 {
                earlier = true;
                break;
            }
            bytes += size;
            turns.push(turn.clone());
        }
        if earlier {
            break;
        }
        let Some(cursor) = str_at(&page, "nextCursor") else {
            break;
        };
        if !cursors.insert(cursor.to_owned()) {
            return Err("Codex repeated a history cursor".into());
        }
        if index == 7 {
            earlier = true;
            break;
        }
        page = client
            .request(
                "thread/turns/list",
                json!({"threadId":parser.thread(),"cursor":cursor,
            "limit":20,"itemsView":"full","sortDirection":"desc"}),
                deadline,
            )
            .map_err(|error| error.to_string())?;
    }
    if descending {
        turns.reverse();
    }
    for turn in &mut turns {
        if turn["itemsView"]
            .as_str()
            .is_some_and(|view| view != "full")
        {
            if turn["status"] == "inProgress" {
                return Err("Codex did not provide an atomic active-turn snapshot; keep this thread in Console".into());
            }
            let mut items = Vec::new();
            let mut cursor = None;
            let mut seen = HashSet::new();
            for index in 0..8 {
                let page = client
                    .request(
                        "thread/items/list",
                        json!({"threadId":parser.thread(),"turnId":turn["id"],
                    "cursor":cursor,"limit":50,"sortDirection":"desc"}),
                        deadline,
                    )
                    .map_err(|error| error.to_string())?;
                for entry in page["data"]
                    .as_array()
                    .ok_or("Codex returned invalid item history")?
                {
                    let size = entry.to_string().len();
                    if bytes + size > MAX_CHAT_SNAPSHOT_BYTES / 2 {
                        earlier = true;
                        break;
                    }
                    bytes += size;
                    items.push(entry["item"].clone());
                }
                let Some(next) = str_at(&page, "nextCursor") else {
                    break;
                };
                if !seen.insert(next.to_owned()) {
                    return Err("Codex repeated an item cursor".into());
                }
                if earlier || index == 7 {
                    earlier = true;
                    break;
                }
                cursor = Some(next.to_owned());
            }
            items.reverse();
            turn["items"] = json!(items);
        }
    }
    Ok((parser.history(&turns), earlier))
}

#[allow(
    clippy::too_many_lines,
    clippy::too_many_arguments,
    reason = "Preflight and terminal handoff are ordered to preserve recovery context"
)]
fn prepare(
    shell: u32,
    foreground: Option<u32>,
    selection: &ChatSelection,
    base: &[(std::ffi::OsString, std::ffi::OsString)],
    fresh: bool,
    token: u64,
    gate: &Arc<InputGate>,
    target: &Arc<FlushTarget>,
) -> Result<Prepared, String> {
    let started = SystemTime::now();
    let mut context = if foreground == Some(shell) {
        Context::shell(shell, TIMEOUT, base)
    } else {
        if selection.target.is_none() {
            return Err(
                "Choose the exact conversation from this Codex engine before switching".into(),
            );
        }
        Context::terminal(shell, foreground, selection.target.as_ref())
    }
    .map_err(str::to_owned)?;
    if context.terminal.is_none()
        && let Some(selected) = &selection.target
    {
        context = context.for_target(selected).map_err(str::to_owned)?;
    }
    if selection
        .launch_context
        .is_some_and(|stamp| stamp != context.launch_stamp())
    {
        return Err("The Codex launch context changed; reopen the provider picker".into());
    }
    let mut client =
        Client::connect(&context, Instant::now() + TIMEOUT).map_err(|error| error.to_string())?;
    if let Some(selected) = &selection.target {
        if selected.engine.as_ref() != Some(&engine_target(&client.engine)) {
            return Err(
                "The selected Codex engine restarted; choose the thread again from Console".into(),
            );
        }
        if ServerPath::from(context.home.clone()) != selected.home
            || client
                .endpoint
                .as_ref()
                .map(|path| ServerPath::from(path.clone()))
                != selected.endpoint
        {
            return Err("The selected thread belongs to another Codex engine or home".into());
        }
    }
    preflight(&mut client, &context)?;
    let (capabilities, default_model, _) = models(&mut client)?;
    let model = selection.model.clone().or(default_model);
    let offered = capabilities
        .models
        .iter()
        .find(|offered| Some(&offered.alias) == model.as_ref())
        .ok_or(
            "The selected Codex model is unavailable; keep the draft and choose another model",
        )?;
    let effort = selection
        .effort
        .clone()
        .or_else(|| offered.default_effort.clone());
    if effort
        .as_ref()
        .is_some_and(|effort| !offered.efforts.contains(effort))
    {
        return Err("The selected effort level is unavailable for this Codex model".into());
    }
    let response = if let Some(selected) = &selection.target {
        client.request(
            "thread/resume",
            json!({"threadId":selected.thread_id,"excludeTurns":true,
            "initialTurnsPage":{"limit":20,"itemsView":"full","sortDirection":"desc"}}),
            Instant::now() + TIMEOUT,
        )
    } else if fresh {
        client.request(
            "thread/start",
            json!({"cwd":context.cwd,"modelProvider":"openai","model":model}),
            Instant::now() + TIMEOUT,
        )
    } else {
        return Err("Choose the exact Codex conversation to enter".into());
    }
    .map_err(|error| error.to_string())?;
    let thread = &response["thread"];
    let id = str_at(thread, "id")
        .filter(|id| crate::session::metadata::agent_session::uuid_shaped(id))
        .ok_or("Codex returned an invalid thread ID")?;
    if selection
        .target
        .as_ref()
        .is_some_and(|target| target.thread_id != id)
    {
        return Err("Codex resumed another thread".into());
    }
    if thread["modelProvider"] != "openai"
        || str_at(thread, "cwd").and_then(|cwd| PathBuf::from(cwd).canonicalize().ok())
            != context.cwd.canonicalize().ok()
    {
        return Err(
            "The thread provider or working directory does not match this launch context".into(),
        );
    }
    let mut parser = Parser::new(id.into(), token);
    parser.target(
        ChatTarget {
            provider: ChatProvider::Codex,
            thread_id: id.into(),
            home: ServerPath::from(context.home.clone()),
            executable: ServerPath::from(context.executable.clone()),
            endpoint: client
                .endpoint
                .as_ref()
                .map(|path| ServerPath::from(path.clone())),
            engine: Some(engine_target(&client.engine)),
        },
        client.endpoint.is_some(),
    );
    let (mut changes, has_earlier) = hydrate(&mut client, &response, &mut parser)?;
    changes.push(parser.configure(
        if fresh {
            model
        } else {
            str_at(&response, "model").map(str::to_owned)
        },
        if fresh {
            effort
        } else {
            str_at(&response, "reasoningEffort").map(str::to_owned)
        },
        thread["status"]["type"] == "active",
    ));
    for message in client.take_buffered() {
        changes.extend(parser.event(&message));
    }
    context
        .verify_engine(&client.engine)
        .map_err(str::to_owned)?;
    client.revalidate().map_err(|error| error.to_string())?;
    gate_and_flush(gate, target, Duration::from_secs(2)).map_err(|error| error.to_string())?;
    let mut detach_uncertain = false;
    if let Some(terminal) = &context.terminal {
        terminal.revalidate().map_err(str::to_owned)?;
        // Only the TUI client is detached; never signal its daemon or another group member.
        rustix::process::kill_process(
            rustix::process::Pid::from_raw(terminal.pid).ok_or("Invalid Codex PID")?,
            rustix::process::Signal::TERM,
        )
        .map_err(|_| "Could not detach the Codex terminal")?;
        let deadline = Instant::now() + Duration::from_secs(5);
        while terminal.revalidate().is_ok() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        detach_uncertain = terminal.revalidate().is_ok();
    }
    let blocked = match super::context::check_shell(shell, None) {
        Ok(group)
            if !detach_uncertain
                && wait_foreground_is(
                    target.master(),
                    group,
                    Instant::now() + Duration::from_secs(2),
                )
                && super::context::children_since(shell, group, started) == 0 =>
        {
            None
        }
        _ => Some(
            "The shell is unavailable after Codex detached; exact-thread recovery remains blocked"
                .into(),
        ),
    };
    context.terminal = None;
    let session_tree = str_at(thread, "sessionId").unwrap_or(id).to_owned();
    Ok(Prepared {
        context,
        client,
        parser,
        changes,
        capabilities,
        fresh,
        since: started,
        stopped: false,
        recovery_typed: false,
        has_earlier,
        blocked,
        session_tree,
    })
}

fn reconcile(client: &mut Client, thread: &str, params: &Value) -> Result<Value, Error> {
    let deadline = Instant::now() + TIMEOUT;
    let mut cursor = None;
    let mut seen = HashSet::new();
    for _ in 0..8 {
        let page=client.request("thread/turns/list",json!({"threadId":thread,"limit":20,"itemsView":"full","sortDirection":"desc","cursor":cursor}),deadline)?;
        let turns = page["data"].as_array().ok_or(Error::Protocol)?;
        if turns.iter().any(|turn| {
            turn["items"]
                .as_array()
                .into_iter()
                .flatten()
                .any(|item| item["type"] == "userMessage" && item["clientId"] == params["clientId"])
        }) {
            return Ok(json!({"found":true}));
        }
        let Some(next) = str_at(&page, "nextCursor") else {
            break;
        };
        if !seen.insert(next.to_owned()) {
            return Err(Error::Protocol);
        }
        cursor = Some(next.to_owned());
    }
    Ok(json!({"found":false}))
}

fn interrupt(client: &mut Client, thread: &str, deadline: Instant) -> Result<Value, Error> {
    let read = client.request(
        "thread/turns/list",
        json!({"threadId":thread,"limit":20,"itemsView":"notLoaded","sortDirection":"desc"}),
        deadline,
    )?;
    let turn = read["data"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|turn| turn["status"] == "inProgress");
    if let Some(turn) = turn {
        client.request(
            "turn/interrupt",
            json!({"threadId":thread,"turnId":turn["id"]}),
            deadline,
        )
    } else {
        Ok(json!({}))
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "Shutdown and exact-thread Console recovery must remain ordered"
)]
fn leave(
    prepared: &mut Prepared,
    gate: &InputGate,
    target: &FlushTarget,
    confirmed: bool,
) -> Result<bool, String> {
    let deadline = Instant::now() + TIMEOUT;
    let thread = prepared.parser.thread().to_owned();
    if !prepared.stopped {
        prepared
            .client
            .revalidate()
            .map_err(|error| error.to_string())?;
    }
    if prepared.client.endpoint.is_none() && !prepared.stopped {
        let loaded = prepared
            .client
            .request("thread/loaded/list", json!({"limit":100}), deadline)
            .map_err(|error| error.to_string())?;
        if loaded["nextCursor"].is_string() {
            return Err(
                "The owned engine has more loaded threads than can be checked safely".into(),
            );
        }
        let mut threads = vec![thread.clone()];
        for id in loaded["data"]
            .as_array()
            .ok_or("Codex did not identify its loaded threads")?
        {
            let id = id
                .as_str()
                .ok_or("Codex returned an invalid loaded thread")?;
            if id == thread {
                continue;
            }
            let read = prepared
                .client
                .request("thread/read", json!({"threadId":id}), deadline)
                .map_err(|error| error.to_string())?;
            if read["thread"]["sessionId"].as_str() != Some(&prepared.session_tree) {
                return Err(
                    "Another conversation is loaded in this engine; shutdown remains blocked"
                        .into(),
                );
            }
            threads.push(id.to_owned());
        }
        for id in &threads {
            let read = prepared
                .client
                .request("thread/read", json!({"threadId":id}), deadline)
                .map_err(|error| error.to_string())?;
            if read["thread"]["status"]["type"] != "idle" {
                if !confirmed {
                    return Err(
                        "Confirm interruption before leaving this standalone Codex session tree"
                            .into(),
                    );
                }
                interrupt(&mut prepared.client, id, deadline).map_err(|error| error.to_string())?;
            }
        }
        loop {
            let mut idle = true;
            for id in &threads {
                let read = prepared
                    .client
                    .request("thread/read", json!({"threadId":id}), deadline)
                    .map_err(|error| error.to_string())?;
                idle &= read["thread"]["status"]["type"] == "idle";
            }
            if idle {
                break;
            }
            if Instant::now() >= deadline {
                return Err("Codex did not confirm completion; its engine was left running".into());
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
    let line = prepared
        .context
        .recovery(&thread, prepared.client.endpoint.as_deref())
        .map_err(str::to_owned)?;
    if !prepared.recovery_typed {
        let group = super::context::check_shell(prepared.context.shell, None)
            .map_err(|_| "The shell is no longer idle")?;
        if !wait_foreground_is(target.master(), group, deadline)
            || super::context::children_since(prepared.context.shell, group, prepared.since) > 0
        {
            return Err("The shell is running another command; Console recovery is blocked".into());
        }
    }
    if !prepared.stopped {
        prepared
            .client
            .request("thread/unsubscribe", json!({"threadId":thread}), deadline)
            .map_err(|error| error.to_string())?;
        if prepared.client.endpoint.is_none() {
            prepared
                .client
                .shutdown_owned(deadline)
                .map_err(|error| error.to_string())?;
            prepared.stopped = true;
        }
        if prepared.fresh {
            return Ok(false);
        }
    }
    if !prepared.recovery_typed {
        let (written, ack) = mpsc::channel();
        gate.send_authorized(format!("{line}\r").into_bytes(), Some(written))
            .map_err(|_| "Could not type the Console recovery command")?;
        prepared.recovery_typed = true;
        if !matches!(ack.recv_timeout(Duration::from_secs(2)), Ok(Ok(()))) {
            return Err(
                "Console recovery delivery is uncertain; the command was not resent".into(),
            );
        }
    }
    while Instant::now() < deadline {
        let foreground = foreground_group(target.master());
        let selected = ChatTarget {
            provider: ChatProvider::Codex,
            thread_id: thread.clone(),
            home: ServerPath::from(prepared.context.home.clone()),
            executable: ServerPath::from(prepared.context.executable.clone()),
            endpoint: prepared
                .client
                .endpoint
                .as_ref()
                .map(|path| ServerPath::from(path.clone())),
            engine: Some(engine_target(&prepared.client.engine)),
        };
        if prepared.client.endpoint.is_none() {
            if prepared
                .context
                .standalone_recovered(foreground, &thread, &prepared.client.engine)
            {
                return Ok(true);
            }
        } else if let Ok(context) =
            Context::terminal(prepared.context.shell, foreground, Some(&selected))
            && context.verify_engine(&prepared.client.engine).is_ok()
        {
            return Ok(true);
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Err("Console recovery could not be confirmed; input remains blocked".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inherited_keys_require_verified_chatgpt_auth_without_exposing_credentials() {
        let chatgpt = json!({"account":{"type":"chatgpt","planType":"pro"}});
        let api_key = json!({"account":{"type":"apiKey"}});
        for name in ["OPENAI_API_KEY", "CODEX_API_KEY"] {
            let env = vec![(name.into(), "private-test-key".into())];
            assert!(check_account(&chatgpt, &env).is_ok());
            let error = check_account(&api_key, &env).unwrap_err();
            assert!(!error.contains("private-test-key"));
            assert!(check_account(&json!({"account":null}), &env).is_err());
        }
        assert!(
            check_account(&api_key, &[]).is_ok(),
            "existing explicit API-key login without inherited keys stays supported"
        );
        assert!(check_account(&api_key, &[("OPENAI_API_KEY".into(), "".into())]).is_ok());
    }

    #[test]
    #[ignore = "run only through scripts/check-codex-chat.py --rust-native-test with an isolated engine"]
    fn native_cli_transport_and_terminal_binding() {
        let path =
            |key| PathBuf::from(std::env::var_os(key).expect("isolated native probe context"));
        let terminal = context::Identity::read(
            std::env::var("MUXY_CODEX_PROBE_TUI_PID")
                .unwrap()
                .parse()
                .unwrap(),
        )
        .unwrap();
        let home = path("MUXY_CODEX_PROBE_HOME");
        let cwd = path("MUXY_CODEX_PROBE_PROJECT");
        let context = Context {
            shell: 0,
            group: terminal.group,
            cwd,
            home,
            executable: terminal.executable.clone(),
            env: Vec::new(),
            terminal: Some(terminal),
            endpoint: Some(path("MUXY_CODEX_PROBE_ENDPOINT")),
        };
        let mut client = Client::connect(&context, Instant::now() + TIMEOUT).unwrap();
        client.revalidate().unwrap();
        let thread = std::env::var("MUXY_CODEX_PROBE_THREAD").unwrap();
        let response = client
            .request(
                "thread/resume",
                json!({"threadId":thread,"excludeTurns":true,
            "initialTurnsPage":{"limit":20,"itemsView":"full"}}),
                Instant::now() + TIMEOUT,
            )
            .unwrap();
        assert_eq!(response["thread"]["id"], thread);
        let mut turns = response["initialTurnsPage"]["data"]
            .as_array()
            .unwrap()
            .clone();
        turns.reverse();
        let mut parser = Parser::new(thread, 1);
        let changes = parser.history(&turns);
        assert!(changes.iter().any(|change|matches!(change,Change::Item {kind:ChatItemKind::User {text},..} if text.contains("MUXY_TARGET_legacy"))));
    }
}

//! One-document, newline-delimited JSON-RPC session. Stdout is exclusively ASCII JSON.
//!
//! The reader queues bounded requests while a single synchronous runner owns the Lean worker.
//! Input and scheduling take bounded turns so continuous requests cannot starve jobs. Traits keep
//! model transport, durable human decisions and incremental Lean replay independently testable.

use crate::{CliError, Transport, review};
use mls_core::{BlockId, BlockKind};
use mls_engine::decisions::{
    Basis, Decider, DecisionConfig, DecisionLayer, EditKind, FieldFlag, ModelDecider, ProseKind,
};
use mls_engine::live::{
    Document, Engine, EngineConfig, GateConfig, Job, JobContext, JobOutcome, JobRunner,
    PipelineRunner, ReviewAction, UpdateHints,
};
use mls_engine::{
    BlockReport, CheckState, LeanStatementChecker, PipelineConfig, ReviewStore, UsageSummary,
    compare_structure, declaration_name,
};
use mls_lean::{EnvSnapshot, LeanWorker, StatementRequest};
use mls_models::{
    DecisionReply, DecisionRequest, GenerationReply, GenerationRequest, ModelClient, ModelConfig,
    ModelError, OpenRouterClient, RecordingClient, ReplayClient,
};
use serde::{Deserialize, de::DeserializeOwned};
use serde_json::{Value, json};
use std::{
    cell::RefCell,
    collections::{BTreeMap, BTreeSet},
    error::Error,
    ffi::OsString,
    io::{self, BufRead, Write},
    path::{Path, PathBuf},
    rc::Rc,
    sync::mpsc::{self, Receiver, RecvTimeoutError, TryRecvError},
    time::{Duration, Instant},
};

const MAX_LINE_BYTES: usize = 4 * 1024 * 1024;
const POLL: Duration = Duration::from_millis(25);
const INPUT_QUEUE_CAPACITY: usize = 16;

pub(super) fn run(arguments: &[OsString]) -> Result<(), Box<dyn Error>> {
    let mut replay = None;
    let mut live = false;
    let mut model_config = crate::workspace_root().join("config/models.json");
    let mut index = 0;
    while index < arguments.len() {
        match arguments[index].to_str() {
            Some("--live") if !live => live = true,
            Some(option @ ("--replay" | "--model-config")) => {
                index += 1;
                let value = arguments
                    .get(index)
                    .ok_or_else(|| CliError(format!("{option} requires a path")))?;
                if option == "--replay" {
                    if replay.replace(PathBuf::from(value)).is_some() {
                        return Err(Box::new(CliError("duplicate --replay".into())));
                    }
                } else {
                    model_config = PathBuf::from(value);
                }
            }
            Some("--help" | "-h") => {
                println!("Usage: mls serve (--replay DIR | --live) [--model-config FILE]");
                return Ok(());
            }
            _ => return Err(Box::new(CliError("unknown serve argument".into()))),
        }
        index += 1;
    }
    if live == replay.is_some() {
        return Err(Box::new(CliError(
            "serve requires exactly one of --replay DIR or --live".into(),
        )));
    }
    if live && std::env::var("MLS_LIVE").as_deref() != Ok("1") {
        return Err(Box::new(CliError("--live requires MLS_LIVE=1".into())));
    }
    let config = ModelConfig::from_path(model_config)?;
    // Constructing the client checks credentials/replay data but makes no provider call.
    let transport = crate::build_transport(replay, &config)?;
    let backend = Production::new(config, transport);
    let mut session = Session::new(backend);
    let (sender, receiver) = mpsc::sync_channel(INPUT_QUEUE_CAPACITY);
    std::thread::spawn(move || {
        let stdin = io::stdin();
        let mut input = stdin.lock();
        loop {
            let line = read_line(&mut input);
            let done = matches!(line, Ok(None) | Err(_));
            if sender.send(line).is_err() || done {
                break;
            }
        }
    });
    let clock = Instant::now();
    let mut output = io::stdout().lock();
    drive(&mut session, &receiver, &mut output, || {
        u64::try_from(clock.elapsed().as_millis()).unwrap_or(u64::MAX)
    })?;
    session.persist().map_err(CliError)?;
    Ok(())
}

type Input = Result<Option<Vec<u8>>, String>;

fn drive<B: Backend>(
    session: &mut Session<B>,
    receiver: &Receiver<Input>,
    output: &mut impl Write,
    mut now: impl FnMut() -> u64,
) -> io::Result<()> {
    let mut deadline = 0_u64;
    while !session.shutdown {
        let wait = Duration::from_millis(deadline.saturating_sub(now())).min(POLL);
        match receiver.recv_timeout(wait) {
            Ok(input) => {
                if !process_input(session, input, output, now())? {
                    return Ok(());
                }
            }
            Err(RecvTimeoutError::Disconnected) => return Ok(()),
            Err(RecvTimeoutError::Timeout) => {}
        }
        // Honor already-queued changes, shutdown and EOF before spending tokens. Capture at most
        // one queue's worth of ready inputs, then process that fixed snapshot in arrival order.
        // Refills cannot extend this turn indefinitely and starve scheduling.
        let mut ready = Vec::with_capacity(INPUT_QUEUE_CAPACITY);
        for _ in 0..INPUT_QUEUE_CAPACITY {
            match receiver.try_recv() {
                Ok(input) => ready.push(input),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    ready.push(Ok(None));
                    break;
                }
            }
        }
        for input in ready {
            if !process_input(session, input, output, now())? {
                return Ok(());
            }
        }
        let current = now();
        if !session.shutdown && current >= deadline {
            write_messages(output, session.tick(current))?;
            deadline = now().saturating_add(u64::try_from(POLL.as_millis()).unwrap_or(u64::MAX));
        }
    }
    Ok(())
}

fn process_input<B: Backend>(
    session: &mut Session<B>,
    input: Input,
    output: &mut impl Write,
    now: u64,
) -> io::Result<bool> {
    match input {
        Ok(Some(line)) => write_messages(output, session.handle(&line, now))?,
        Ok(None) => return Ok(false),
        Err(error) => {
            write_messages(output, vec![rpc_error(Value::Null, -32700, error)])?;
            return Ok(false);
        }
    }
    Ok(!session.shutdown)
}

// Never allocate an unbounded line, even when a peer forgets its newline. EOF is a disconnect,
// not an instruction to spend tokens finishing dirty blocks.
fn read_line(input: &mut impl BufRead) -> Result<Option<Vec<u8>>, String> {
    let mut bytes = Vec::new();
    loop {
        let available = input.fill_buf().map_err(|error| error.to_string())?;
        if available.is_empty() {
            return if bytes.is_empty() {
                Ok(None)
            } else {
                Err("truncated JSON line at EOF".into())
            };
        }
        let consumed = available
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(available.len(), |position| position + 1);
        if bytes.len().saturating_add(consumed) > MAX_LINE_BYTES {
            return Err("JSON line exceeds 4 MiB".into());
        }
        bytes.extend_from_slice(&available[..consumed]);
        input.consume(consumed);
        if bytes.last() == Some(&b'\n') {
            return Ok(Some(bytes));
        }
    }
}

fn write_messages(output: &mut impl Write, messages: Vec<Value>) -> io::Result<()> {
    for message in messages {
        let serialized = serde_json::to_string(&message)?;
        for character in serialized.chars() {
            if character.is_ascii() {
                write!(output, "{character}")?;
            } else {
                let mut pair = [0; 2];
                for unit in character.encode_utf16(&mut pair) {
                    write!(output, "\\u{unit:04x}")?;
                }
            }
        }
        writeln!(output)?;
    }
    output.flush()
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    jsonrpc: String,
    id: Value,
    method: String,
    #[serde(default = "empty_object")]
    params: Value,
}

fn empty_object() -> Value {
    json!({})
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Empty {}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Open {
    document: String,
    revision: u64,
    source: String,
    #[serde(default)]
    active: Option<BlockId>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Change {
    document: String,
    revision: u64,
    source: String,
    #[serde(default)]
    active: Option<BlockId>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Cursor {
    document: String,
    revision: u64,
    active: Option<BlockId>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Review {
    document: String,
    revision: u64,
    block: BlockId,
    action: String,
    sir: Option<Value>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Retry {
    document: String,
    revision: u64,
    block: BlockId,
}

struct Fault {
    code: i32,
    message: String,
}

impl Fault {
    fn params(error: impl std::fmt::Display) -> Self {
        Self {
            code: -32602,
            message: error.to_string(),
        }
    }
    fn state(error: impl std::fmt::Display) -> Self {
        Self {
            code: -32000,
            message: error.to_string(),
        }
    }
}

fn params<T: DeserializeOwned>(value: Value) -> Result<T, Fault> {
    serde_json::from_value(value).map_err(Fault::params)
}

fn rpc_error(id: Value, code: i32, message: impl Into<String>) -> Value {
    json!({"jsonrpc":"2.0","id":id,"error":{"code":code,"message":message.into()}})
}

/// Session-specific I/O boundary. Only `run` may invoke a model; a failed persistence or
/// environment operation stops further scheduling, while shutdown remains available.
trait Backend {
    fn model(&self) -> &str;
    fn open(&mut self, document: &Path) -> Result<ReviewStore, String>;
    fn synchronize(&mut self, engine: &Engine) -> Result<(), String>;
    fn run(&mut self, job: &Job, context: &JobContext<'_>) -> JobOutcome;
    fn persist(&mut self, store: &ReviewStore) -> Result<(), String>;
    fn configure(&mut self, _engine: &mut Engine) {}
    fn update_hints(&mut self, _engine: &Engine, _source: &[u8]) -> UpdateHints {
        UpdateHints::default()
    }
    fn annotations(&self) -> Value {
        json!({})
    }
    fn health(&mut self) -> Result<(), String> {
        Ok(())
    }
}

struct Session<B> {
    backend: B,
    engine: Option<Engine>,
    document: Option<String>,
    revision: u64,
    initialized: bool,
    shutdown: bool,
    fatal: Option<String>,
}

impl<B: Backend> Session<B> {
    fn new(backend: B) -> Self {
        Self {
            backend,
            engine: None,
            document: None,
            revision: 0,
            initialized: false,
            shutdown: false,
            fatal: None,
        }
    }

    fn handle(&mut self, line: &[u8], now: u64) -> Vec<Value> {
        let value: Value = match serde_json::from_slice(line) {
            Ok(value) => value,
            Err(error) => return vec![rpc_error(Value::Null, -32700, error.to_string())],
        };
        let id = value
            .get("id")
            .filter(|id| id.is_string() || id.is_number())
            .cloned();
        let request: Request = match serde_json::from_value::<Request>(value) {
            Ok(request) if id.is_some() && request.jsonrpc == "2.0" => request,
            _ => {
                return vec![rpc_error(
                    id.unwrap_or(Value::Null),
                    -32600,
                    "invalid request",
                )];
            }
        };
        let id = request.id.clone();
        match self.dispatch(request, now) {
            Ok(result) => {
                let mut messages = vec![json!({"jsonrpc":"2.0","id":id,"result":result})];
                if self.engine.is_some() && !self.shutdown {
                    messages.extend(self.pushes());
                }
                messages
            }
            Err(error) => {
                let mut messages = vec![rpc_error(id, error.code, error.message)];
                if self.fatal.is_some() {
                    messages.extend(self.pushes());
                }
                messages
            }
        }
    }

    fn dispatch(&mut self, request: Request, now: u64) -> Result<Value, Fault> {
        if self.shutdown {
            return Err(Fault::state("session is shut down"));
        }
        if request.method == "initialize" {
            let _: Empty = params(request.params)?;
            if self.initialized {
                return Err(Fault::state("already initialized"));
            }
            self.initialized = true;
            return Ok(json!({"protocol":1,"capabilities":{
                "singleDocument":true,"asciiJson":true,"incrementalLean":true,
                "decisionOracle":true,"implicitLinks":true,
                "cosmeticDecisions":true,"fieldReviewHints":true,
                "concurrentRequestProcessing":false
            }}));
        }
        if !self.initialized {
            return Err(Fault::state("initialize first"));
        }
        if request.method == "shutdown" {
            let _: Empty = params(request.params)?;
            self.shutdown = true;
            self.persist().map_err(Fault::state)?;
            return Ok(json!({"usage":{
                "formalization":self.engine.as_ref().map(Engine::usage),
                "decisions":self.backend.annotations()["usage"]
            },"fatal":self.fatal}));
        }
        if let Some(error) = &self.fatal {
            return Err(Fault::state(error));
        }
        match request.method.as_str() {
            "didOpen" => {
                let open: Open = params(request.params)?;
                if self.engine.is_some() {
                    return Err(Fault::state("one document per session"));
                }
                let path =
                    crate::absolute_path(Path::new(&open.document)).map_err(Fault::params)?;
                if path.extension().is_none_or(|extension| extension != "tm") {
                    return Err(Fault::params("document must be a .tm path"));
                }
                // Validate IDs and source transactionally before preparing a project or worker.
                let mut checked = Document::new();
                checked
                    .update(open.source.as_bytes())
                    .map_err(Fault::params)?;
                validate_active(&checked, open.active)?;
                let store = self.backend.open(&path).map_err(Fault::state)?;
                let mut engine = Engine::new(
                    EngineConfig {
                        document: open.document.clone(),
                        model: self.backend.model().into(),
                        gate: GateConfig::default(),
                        sir_cache_capacity: 256,
                        lean_cache_capacity: 256,
                    },
                    store,
                );
                let hints = self.backend.update_hints(&engine, open.source.as_bytes());
                engine
                    .on_change_with_hints(open.source.as_bytes(), open.active, now, &hints)
                    .map_err(Fault::params)?;
                prune_review(&mut engine);
                self.backend.configure(&mut engine);
                self.revision = open.revision;
                self.document = Some(open.document);
                self.engine = Some(engine);
                self.persist_or_stop()?;
            }
            "didChange" => {
                let change: Change = params(request.params)?;
                self.check_document(&change.document)?;
                if change.revision <= self.revision {
                    return Err(Fault {
                        code: -32001,
                        message: "change revision must increase".into(),
                    });
                }
                let mut checked = Document::new();
                checked
                    .update(change.source.as_bytes())
                    .map_err(Fault::params)?;
                validate_active(&checked, change.active)?;
                let hints = self.backend.update_hints(
                    self.engine.as_ref().expect("checked document"),
                    change.source.as_bytes(),
                );
                let engine = self.engine.as_mut().expect("checked document");
                engine
                    .on_change_with_hints(change.source.as_bytes(), change.active, now, &hints)
                    .map_err(Fault::params)?;
                // Removed decisions cannot attach to another block when a document is reopened.
                prune_review(engine);
                self.revision = change.revision;
                self.synchronize()?;
                self.persist_or_stop()?;
            }
            "cursor" => {
                let cursor: Cursor = params(request.params)?;
                self.check_revision(&cursor.document, cursor.revision)?;
                let engine = self.engine.as_mut().expect("checked document");
                validate_active(engine.document(), cursor.active)?;
                engine.on_cursor(cursor.active);
            }
            "review" => {
                let review: Review = params(request.params)?;
                self.check_revision(&review.document, review.revision)?;
                let action = match (review.action.as_str(), review.sir) {
                    ("confirm", None) => ReviewAction::Confirm(review.block),
                    ("reject", None) => ReviewAction::Reject(review.block),
                    ("edit", Some(sir)) if sir.is_object() => ReviewAction::Edit {
                        block: review.block,
                        sir_json: sir.to_string(),
                    },
                    _ => {
                        return Err(Fault::params(
                            "confirm/reject take no sir; edit requires a sir object",
                        ));
                    }
                };
                self.engine
                    .as_mut()
                    .expect("checked document")
                    .on_review(action, now)
                    .map_err(Fault::params)?;
                // Persist the decision before any new model work, including after reject/edit.
                self.persist_or_stop()?;
                self.synchronize()?;
            }
            "retry" => {
                let retry: Retry = params(request.params)?;
                self.check_revision(&retry.document, retry.revision)?;
                if !self
                    .engine
                    .as_mut()
                    .expect("checked document")
                    .retry(retry.block, now)
                {
                    return Err(Fault::params("unknown formal block"));
                }
                self.synchronize()?;
            }
            _ => {
                return Err(Fault {
                    code: -32601,
                    message: "unknown method".into(),
                });
            }
        }
        Ok(json!({"document":self.document,"revision":self.revision}))
    }

    fn check_document(&self, document: &str) -> Result<(), Fault> {
        if self.document.as_deref() != Some(document) {
            return Err(Fault::state("document is not open"));
        }
        Ok(())
    }
    fn check_revision(&self, document: &str, revision: u64) -> Result<(), Fault> {
        self.check_document(document)?;
        if revision != self.revision {
            return Err(Fault {
                code: -32001,
                message: "stale document revision".into(),
            });
        }
        Ok(())
    }
    fn synchronize(&mut self) -> Result<(), Fault> {
        if let Some(engine) = &self.engine
            && let Err(error) = self.backend.synchronize(engine)
        {
            self.fatal = Some(error.clone());
            return Err(Fault::state(error));
        }
        Ok(())
    }
    fn persist(&mut self) -> Result<(), String> {
        if let Some(engine) = &self.engine {
            self.backend.persist(engine.review())?;
        }
        Ok(())
    }
    fn persist_or_stop(&mut self) -> Result<(), Fault> {
        self.persist().map_err(|error| {
            self.fatal = Some(error.clone());
            Fault::state(error)
        })
    }
    fn tick(&mut self, now: u64) -> Vec<Value> {
        if self.shutdown || self.fatal.is_some() || self.engine.is_none() {
            return Vec::new();
        }
        if self.synchronize().is_err() {
            return self.pushes();
        }
        let engine = self.engine.as_mut().expect("engine present");
        let Some(job) = engine.next_job(now) else {
            return Vec::new();
        };
        let outcome = match engine.context(&job) {
            Some(context) => self.backend.run(&job, &context),
            None => JobOutcome::Failed {
                message: "job context disappeared".into(),
                usage: UsageSummary::default(),
            },
        };
        engine.complete(job, outcome);
        // Account even terminal and recording failures before stopping scheduling.
        if let Err(error) = self.backend.health() {
            self.fatal = Some(error);
        }
        let _ = self.persist_or_stop();
        self.pushes()
    }
    fn pushes(&self) -> Vec<Value> {
        let Some(engine) = &self.engine else {
            return Vec::new();
        };
        let view = engine.view();
        let reviews: Vec<_> = view
            .blocks
            .iter()
            .filter(|block| {
                block.sir.is_some()
                    && (block.review == "none" || block.superseded_confirmation.is_some())
            })
            .collect();
        let errors: Vec<_> = view
            .blocks
            .iter()
            .filter(|block| !block.errors.is_empty())
            .map(|block| json!({"block":block.block_id,"errors":block.errors}))
            .collect();
        let notification = |method: &str, payload: Value| {
            json!({"jsonrpc":"2.0","method":method,
            "params":{"document":self.document,"revision":self.revision,"data":payload}})
        };
        vec![
            notification(
                "blockStatus",
                json!({"view":view,"dirty":engine.dirty(),"usage":{
                    "formalization":engine.usage(),"decisions":self.backend.annotations()["usage"]},
                    "decision_annotations":self.backend.annotations()}),
            ),
            notification("reviewRequest", json!({"blocks":reviews})),
            notification(
                "diagnostics",
                json!({"source":engine.document().analysis().diagnostics,
                "blocks":errors,"fatal":self.fatal}),
            ),
        ]
    }
}

fn validate_active(document: &Document, active: Option<BlockId>) -> Result<(), Fault> {
    if active.is_some_and(|id| !document.contains(id)) {
        return Err(Fault::params("active block is not in this document"));
    }
    Ok(())
}

fn prune_review(engine: &mut Engine) {
    for id in engine.review().block_ids() {
        if mls_engine::formal_block(engine.document().analysis(), id).is_none() {
            engine.review_mut().forget(id);
        }
    }
}

/// A saved environment precedes each accepted declaration. Restore the first invalid entry;
/// replay every retained later declaration, rebuilding all subsequent snapshots. Position alone
/// never invalidates a declaration. A replay error poisons the session instead of using a partial
/// environment for model work.
trait DeclarationEnvironment {
    type Snapshot: Copy;
    fn snapshot(&self) -> Self::Snapshot;
    fn restore(&mut self, snapshot: Self::Snapshot) -> Result<(), String>;
    fn replay(&mut self, report: &BlockReport) -> Result<(), String>;
}

struct Accepted<S> {
    before: S,
    report: BlockReport,
}
struct Journal<S> {
    accepted: Vec<Accepted<S>>,
}
impl<S: Copy> Journal<S> {
    fn new() -> Self {
        Self {
            accepted: Vec::new(),
        }
    }
    fn record_completed(&mut self, proof_only: bool, before: S, report: &BlockReport) {
        if !proof_only && report.accepted_speculatively {
            self.accepted.push(Accepted {
                before,
                report: report.clone(),
            });
        }
    }
    fn synchronize<E: DeclarationEnvironment<Snapshot = S>>(
        &mut self,
        environment: &mut E,
        engine: &Engine,
    ) -> Result<(), String> {
        let view = engine.view();
        let retained: BTreeMap<_, _> = view
            .blocks
            .iter()
            .filter(|block| !engine.dirty().contains(&block.block_id))
            .map(|block| (block.block_id, block.lean_statement.as_deref()))
            .collect();
        let valid = |entry: &Accepted<S>| {
            retained.get(&entry.report.block_id).copied().flatten()
                == entry.report.lean_statement.as_deref()
                && entry.report.lean_statement.is_some()
        };
        let Some(first) = self.accepted.iter().position(|entry| !valid(entry)) else {
            return Ok(());
        };
        environment.restore(self.accepted[first].before)?;
        let suffix = self.accepted.split_off(first);
        for entry in suffix.into_iter().filter(valid) {
            let before = environment.snapshot();
            environment.replay(&entry.report)?;
            self.accepted.push(Accepted {
                before,
                report: entry.report,
            });
        }
        Ok(())
    }
}

impl DeclarationEnvironment for LeanWorker {
    type Snapshot = EnvSnapshot;
    fn snapshot(&self) -> EnvSnapshot {
        self.snapshot()
    }
    fn restore(&mut self, snapshot: EnvSnapshot) -> Result<(), String> {
        self.restore(snapshot).map_err(|error| error.to_string())
    }
    fn replay(&mut self, report: &BlockReport) -> Result<(), String> {
        let name = declaration_name(report.block_id);
        let source = report
            .lean_statement
            .as_deref()
            .ok_or("missing accepted Lean source")?;
        let checked = self
            .check_statement(StatementRequest {
                block_id: report.block_id,
                declaration_name: &name,
                source,
            })
            .map_err(|error| error.to_string())?;
        let sir = report.sir.as_ref().ok_or("missing accepted SIR")?;
        if compare_structure(sir, &name, &checked.extracted)
            .iter()
            .any(|check| check.hard && check.state == CheckState::Mismatch)
        {
            return Err("accepted declaration failed structural comparison during replay".into());
        }
        self.accept(checked.candidate)
            .map_err(|error| error.to_string())?;
        Ok(())
    }
}

enum Models {
    Replay(ReplayClient),
    Live(Box<RecordingClient<OpenRouterClient>>),
}
impl ModelClient for Models {
    fn generate(&mut self, request: &GenerationRequest) -> Result<GenerationReply, ModelError> {
        match self {
            Self::Replay(client) => client.generate(request),
            Self::Live(client) => client.generate(request),
        }
    }
    fn decide(&mut self, request: &DecisionRequest) -> Result<DecisionReply, ModelError> {
        match self {
            Self::Replay(client) => client.decide(request),
            Self::Live(client) => client.decide(request),
        }
    }
}

#[derive(Clone)]
struct SharedModels(Rc<RefCell<Models>>);
impl ModelClient for SharedModels {
    fn generate(&mut self, request: &GenerationRequest) -> Result<GenerationReply, ModelError> {
        self.0.borrow_mut().generate(request)
    }
    fn decide(&mut self, request: &DecisionRequest) -> Result<DecisionReply, ModelError> {
        self.0.borrow_mut().decide(request)
    }
}

struct SharedFinished(Rc<RefCell<DecisionLayer<ModelDecider<SharedModels>>>>);
impl mls_engine::live::FinishedOracle for SharedFinished {
    fn is_finished(
        &mut self,
        query: &mls_engine::live::FinishedQuery<'_>,
    ) -> mls_engine::live::OracleAnswer {
        mls_engine::live::FinishedOracle::is_finished(&mut *self.0.borrow_mut(), query)
    }
}

struct Production {
    config: ModelConfig,
    transport: Option<Transport>,
    models: Option<Rc<RefCell<Models>>>,
    decisions: Option<Rc<RefCell<DecisionLayer<ModelDecider<SharedModels>>>>>,
    decision_flags: BTreeMap<BlockId, Vec<FieldFlag>>,
    differs: BTreeMap<BlockId, Value>,
    edit_hints: BTreeMap<BlockId, Value>,
    link_hints: BTreeMap<BlockId, Value>,
    prose_classifications: BTreeMap<BlockId, ProseClassification>,
    worker: Option<LeanWorker>,
    document: Option<PathBuf>,
    journal: Journal<EnvSnapshot>,
}

struct ProseClassification {
    text_hash: String,
    kind: ProseKind,
    basis: Basis,
    annotation: Value,
}

/// Reuse a prose decision only while its persistent ID and canonical text still match.
fn classify_untagged_prose<D: Decider>(
    next: &Document,
    layer: &mut DecisionLayer<D>,
    classifications: &mut BTreeMap<BlockId, ProseClassification>,
) -> BTreeSet<BlockId> {
    let transient: BTreeSet<_> = next
        .analysis()
        .insertions
        .iter()
        .map(|insertion| insertion.id)
        .collect();
    classifications.retain(|id, classification| {
        !transient.contains(id)
            && next.block(*id).is_some_and(|state| {
                state.kind == BlockKind::Commentary && state.text_hash == classification.text_hash
            })
    });
    let mut claims = BTreeSet::new();
    for id in next.order().iter().copied() {
        let (Some(state), Some(text)) = (next.block(id), next.canonical(id)) else {
            continue;
        };
        if state.kind != BlockKind::Commentary || transient.contains(&id) {
            continue;
        }
        let classification = classifications.entry(id).or_insert_with(|| {
            let outcome = layer.classify_prose(&id.to_string(), &String::from_utf8_lossy(text));
            ProseClassification {
                text_hash: state.text_hash.clone(),
                kind: outcome.value,
                basis: outcome.basis,
                annotation: serde_json::to_value(outcome).unwrap_or(Value::Null),
            }
        });
        if classification.kind == ProseKind::Claim && classification.basis == Basis::Jev {
            claims.insert(id);
        }
    }
    claims
}

impl Production {
    fn new(config: ModelConfig, transport: Transport) -> Self {
        Self {
            config,
            transport: Some(transport),
            models: None,
            decisions: None,
            decision_flags: BTreeMap::new(),
            differs: BTreeMap::new(),
            edit_hints: BTreeMap::new(),
            link_hints: BTreeMap::new(),
            prose_classifications: BTreeMap::new(),
            worker: None,
            document: None,
            journal: Journal::new(),
        }
    }
}
impl Backend for Production {
    fn model(&self) -> &str {
        &self.config.generation_model
    }
    fn open(&mut self, document: &Path) -> Result<ReviewStore, String> {
        let live = matches!(self.transport, Some(Transport::Live(_)));
        crate::preflight_batch(document, live).map_err(|error| error.to_string())?;
        let store = review::serve_load_store(document)?;
        let project = crate::initialize_project(document).map_err(|error| error.to_string())?;
        let worker = crate::spawn_worker(&project).map_err(|error| error.to_string())?;
        let models = match self
            .transport
            .take()
            .ok_or("transport already opened; restart session")?
        {
            Transport::Replay(client) => Models::Replay(client),
            Transport::Live(client) => {
                let log = project.join("log");
                crate::ensure_owned_dir(&log).map_err(|error| error.to_string())?;
                let directory = log.join(format!("serve-{}", BlockId::new()));
                // Create and verify writable recording storage before admitting model work.
                crate::ensure_owned_dir(&directory).map_err(|error| error.to_string())?;
                let probe = tempfile::NamedTempFile::new_in(&directory)
                    .map_err(|error| error.to_string())?;
                probe
                    .as_file()
                    .sync_all()
                    .map_err(|error| error.to_string())?;
                eprintln!("Model call records: {}", directory.display());
                Models::Live(Box::new(RecordingClient::new(
                    client,
                    directory,
                    &self.config.generation_model,
                    &self.config.decision_model,
                    "live",
                )))
            }
        };
        let models = Rc::new(RefCell::new(models));
        self.decisions = Some(Rc::new(RefCell::new(
            DecisionLayer::new(
                ModelDecider::new(
                    SharedModels(models.clone()),
                    self.config.decision_model.clone(),
                ),
                DecisionConfig::default(),
            )
            .map_err(|error| error.to_string())?,
        )));
        self.models = Some(models);
        self.worker = Some(worker);
        self.document = Some(document.into());
        // Surface an unwritable store before making the first provider call.
        self.persist(&store)?;
        Ok(store)
    }
    fn configure(&mut self, engine: &mut Engine) {
        if let Some(decisions) = &self.decisions {
            engine.set_oracle(Box::new(SharedFinished(decisions.clone())));
        }
    }
    fn update_hints(&mut self, engine: &Engine, source: &[u8]) -> UpdateHints {
        let old = engine.document();
        let mut next = Document::new();
        if next.update(source).is_err() {
            return UpdateHints::default();
        }
        let current: BTreeSet<_> = next.order().iter().copied().collect();
        self.decision_flags.retain(|id, _| current.contains(id));
        self.differs.retain(|id, _| current.contains(id));
        self.edit_hints.retain(|id, _| current.contains(id));
        self.link_hints.retain(|id, _| current.contains(id));
        let mut hints = UpdateHints::default();
        let Some(decisions) = &self.decisions else {
            return hints;
        };
        let mut layer = decisions.borrow_mut();
        hints.prose_claims =
            classify_untagged_prose(&next, &mut layer, &mut self.prose_classifications);
        for id in next.order().iter().copied() {
            let (Some(new_state), Some(new_text)) = (next.block(id), next.canonical(id)) else {
                continue;
            };
            let text_changed = old
                .block(id)
                .is_some_and(|old| old.text_hash != new_state.text_hash);
            let same_prose_claim = old.block(id).is_some_and(|old| {
                old.kind == BlockKind::ProseClaim
                    && new_state.kind == BlockKind::Commentary
                    && self.prose_classifications.contains_key(&id)
            });
            let kind_changed = old
                .block(id)
                .is_some_and(|old| old.kind != new_state.kind && !same_prose_claim);
            if text_changed || kind_changed {
                self.decision_flags.remove(&id);
                self.differs.remove(&id);
                self.edit_hints.remove(&id);
                self.link_hints.remove(&id);
            }
            if let (Some(old_state), Some(old_text)) = (old.block(id), old.canonical(id))
                && old_state.text_hash != new_state.text_hash
                && engine.cosmetic_hint_eligible(id)
            {
                let outcome = layer.is_semantic_edit(
                    &id.to_string(),
                    &String::from_utf8_lossy(old_text),
                    &String::from_utf8_lossy(new_text),
                );
                self.edit_hints
                    .insert(id, serde_json::to_value(&outcome).unwrap_or(Value::Null));
                if outcome.value == EditKind::Cosmetic {
                    hints.cosmetic_edits.insert(id);
                }
            }
            let classified_prose_claim = hints.prose_claims.contains(&id);
            if !new_state.deps.is_empty()
                || (new_state.label.is_none() && !classified_prose_claim)
                || (mls_engine::formal_block(next.analysis(), id).is_none()
                    && !classified_prose_claim)
            {
                continue;
            }
            let mut labels = Vec::new();
            let mut targets = BTreeMap::new();
            for earlier in next
                .order()
                .iter()
                .copied()
                .take_while(|earlier| *earlier != id)
            {
                if mls_engine::formal_block(next.analysis(), earlier).is_none()
                    && !hints.prose_claims.contains(&earlier)
                {
                    continue;
                }
                if let Some(label) = next.block(earlier).and_then(|state| state.label.clone()) {
                    labels.push(label.clone());
                    targets.entry(label).or_insert(earlier);
                }
            }
            if labels.is_empty() {
                continue;
            }
            let outcome =
                layer.link_implicit(&id.to_string(), &String::from_utf8_lossy(new_text), &labels);
            self.link_hints
                .insert(id, serde_json::to_value(&outcome).unwrap_or(Value::Null));
            if let Some(label) = outcome.value
                && let Some(target) = targets.get(&label)
            {
                hints.implicit_links.insert(id, *target);
            }
        }
        hints
    }
    fn synchronize(&mut self, engine: &Engine) -> Result<(), String> {
        self.journal
            .synchronize(self.worker.as_mut().ok_or("worker is not open")?, engine)
    }
    fn run(&mut self, job: &Job, context: &JobContext<'_>) -> JobOutcome {
        let worker = self.worker.as_mut().expect("opened worker");
        let before = worker.snapshot();
        let mut checker = LeanStatementChecker::new(worker);
        let models = self.models.as_ref().expect("opened models").clone();
        let outcome = PipelineRunner::new(
            &mut *models.borrow_mut(),
            &mut checker,
            PipelineConfig {
                max_attempts: 3,
                formalization_max_tokens: self.config.default_max_tokens,
            },
        )
        .run(job, context);
        if let JobOutcome::Completed(result) = &outcome
            && let Some(sir) = &result.report.sir
            && let Some(decisions) = &self.decisions
        {
            let mut layer = decisions.borrow_mut();
            let text = context
                .canonical
                .map(String::from_utf8_lossy)
                .unwrap_or_default();
            let differs = layer.sir_obviously_differs(&job.block.to_string(), &text, sir);
            let flags = layer.check_sir_fields(&job.block.to_string(), &text, sir);
            self.differs.insert(
                job.block,
                serde_json::to_value(differs).unwrap_or(Value::Null),
            );
            self.decision_flags.insert(job.block, flags);
        }
        if let JobOutcome::Completed(result) = &outcome {
            self.journal
                .record_completed(job.proof_only, before, &result.report);
        }
        outcome
    }
    fn persist(&mut self, store: &ReviewStore) -> Result<(), String> {
        if let Some(document) = &self.document {
            review::serve_persist_store(document, store)?;
        }
        Ok(())
    }
    fn annotations(&self) -> Value {
        json!({"fields":self.decision_flags,"differs":self.differs,
            "edit_hints":self.edit_hints,"implicit_links":self.link_hints,
            "prose_classifications":self.prose_classifications.iter().map(|(id, classification)| (id.to_string(), classification.annotation.clone())).collect::<BTreeMap<_, _>>(),
            "usage":self.decisions.as_ref().map(|decisions| decisions.borrow().usage().clone())})
    }
    fn health(&mut self) -> Result<(), String> {
        if let Some(models) = &self.models
            && let Models::Live(client) = &mut *models.borrow_mut()
            && let Some(error) = client.persistence_failure()
        {
            return Err(format!(
                "model recording incomplete; restart required: {error}"
            ));
        }
        // A transport failure poisons the worker. Validate its local readiness before another
        // block can spend tokens; restoring the current snapshot performs no REPL request.
        if let Some(worker) = &mut self.worker {
            worker
                .restore(worker.snapshot())
                .map_err(|error| error.to_string())?;
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "../tests/serve/session.rs"]
mod tests;

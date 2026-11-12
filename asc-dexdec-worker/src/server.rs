//! asc's decompile backend protocol (asc-rs `docs/decompile-backends.md`
//! §5–§6) served over stdin / stdout.
//!
//! * stdout carries protocol frames only; logs go to stderr (see [`stdio`]).
//! * Requests run on threads of their own, so `notifications/cancelled` is
//!   read while they work.
//! * A program session keeps its file open and loads classes on demand.
//!   Requests on one session take turns; sessions work in parallel.
//! * A panic while decompiling a class fails that class only.

use std::collections::HashMap;
use std::io::{self, BufRead, BufReader, Write};
use std::num::NonZeroU32;
use std::panic::{self, AssertUnwindSafe};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use asc_decompile_protocol::methods::{
    Cancelled, CancelledParams, DecompileClasses, DecompileClassesParams, DecompileClassesResult,
    Empty, Implementation, Initialize, InitializeParams, InitializeResult, InputKind, Language,
    ProgramClose, ProgramCloseParams, ProgramId, ProgramInput, ProgramOpen, ProgramOpenParams,
    ProgramOpenResult, Progress, ProgressParams, Shutdown,
};
use asc_decompile_protocol::server::Outgoing;
use asc_decompile_protocol::{
    ErrorCode, ErrorObject, FrameError, FrameReader, Message, Method, NotificationMethod,
    PROTOCOL_VERSION, Request, RequestId, Response,
};
use dexdec::SourceLanguage;
use serde_json::Value;

use crate::fault::Fault;
use crate::observer::CancelFlag;
use crate::options::{WorkerOptions, invalid_params};
use crate::session::{ClassRequest, Session};
use crate::{VERSION, pool, stdio};

/// `backend.name` in the `initialize` result.
pub(crate) const BACKEND_NAME: &str = "dexdec";

/// `maxConcurrency` in the `initialize` result.
///
/// One class already spreads its methods over every core, so concurrency
/// only pays off across program sessions; requests on the same session wait
/// their turn inside the worker, where a cancellation still reaches them.
const MAX_CONCURRENCY: NonZeroU32 = NonZeroU32::new(4).expect("4 is not zero");

/// Serves the protocol on this process's stdin and stdout until stdin ends.
pub(crate) fn serve_stdio() -> Result<(), FrameError> {
    serve(BufReader::new(io::stdin()), stdio::protocol_stdout()?)
}

/// Serves the protocol on `input` and `output` until `input` ends. Returns
/// without waiting for requests still running.
fn serve<R: BufRead, W: Write + Send + 'static>(input: R, output: W) -> Result<(), FrameError> {
    let server = Arc::new(Server::new(Outgoing::new(output), Fault::from_env()));
    let mut reader = FrameReader::new(input);
    while let Some(frame) = reader.read_frame()? {
        match parse(frame) {
            Ok(Message::Request(request)) => server.request(request)?,
            Ok(Message::Notification(notification)) => {
                server.notification(&notification.method, notification.params)
            }
            // asc sends no requests the other way, so there is nothing to answer.
            Ok(Message::Response(_)) => {}
            // A malformed message has no id to answer to.
            Err(error) => server.out.send(Response::error(None, error))?,
        }
    }
    Ok(())
}

/// A frame as a message, or the JSON-RPC error that answers it.
fn parse(frame: &[u8]) -> Result<Message, ErrorObject> {
    let value: Value = serde_json::from_slice(frame)
        .map_err(|err| ErrorObject::new(ErrorCode::PARSE_ERROR, format!("not JSON: {err}")))?;
    serde_json::from_value(value).map_err(|err| {
        ErrorObject::new(
            ErrorCode::INVALID_REQUEST,
            format!("not a JSON-RPC message: {err}"),
        )
    })
}

/// Where the connection is in its lifecycle (§5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// Waiting for `initialize`.
    Starting,
    /// Serving requests.
    Ready,
    /// `shutdown` answered; only stdin's end is expected.
    ShuttingDown,
}

struct Server<W> {
    out: Outgoing<W>,
    phase: Mutex<Phase>,
    /// Options fixed by `initialize`.
    options: OnceLock<WorkerOptions>,
    programs: Mutex<HashMap<String, Arc<Program>>>,
    next_program: AtomicU64,
    /// Cancellation flags of the `decompile/classes` requests not answered yet.
    running: Mutex<HashMap<RequestId, CancelFlag>>,
    fault: Fault,
}

/// One `program/open` session. Its decompiler serves one request at a time;
/// the others wait for their turn without blocking cancellation.
struct Program {
    /// `None` while a request has the session.
    session: Mutex<Option<Session>>,
    returned: Condvar,
}

impl Program {
    fn new(session: Session) -> Self {
        Self {
            session: Mutex::new(Some(session)),
            returned: Condvar::new(),
        }
    }

    /// Waits for the session; `None` once `cancel` is raised first.
    fn take(&self, cancel: &CancelFlag) -> Option<Turn<'_>> {
        let mut slot = self.session.lock().expect("session mutex poisoned");
        loop {
            if cancel.is_cancelled() {
                return None;
            }
            if let Some(session) = slot.take() {
                return Some(Turn {
                    program: self,
                    session: Some(session),
                });
            }
            // Cancellation does not signal the condition variable; poll it.
            slot = self
                .returned
                .wait_timeout(slot, Duration::from_millis(20))
                .expect("session mutex poisoned")
                .0;
        }
    }
}

/// A request's hold on a session, given back when dropped.
struct Turn<'a> {
    program: &'a Program,
    session: Option<Session>,
}

impl Turn<'_> {
    fn session(&mut self) -> &mut Session {
        self.session
            .as_mut()
            .expect("a turn holds the session until dropped")
    }
}

impl Drop for Turn<'_> {
    fn drop(&mut self) {
        *self.program.session.lock().expect("session mutex poisoned") = self.session.take();
        self.program.returned.notify_one();
    }
}

impl<W: Write + Send + 'static> Server<W> {
    fn new(out: Outgoing<W>, fault: Fault) -> Self {
        Self {
            out,
            phase: Mutex::new(Phase::Starting),
            options: OnceLock::new(),
            programs: Mutex::new(HashMap::new()),
            next_program: AtomicU64::new(1),
            running: Mutex::new(HashMap::new()),
            fault,
        }
    }

    fn phase(&self) -> Phase {
        *self.phase.lock().expect("phase mutex poisoned")
    }

    fn set_phase(&self, phase: Phase) {
        *self.phase.lock().expect("phase mutex poisoned") = phase;
    }

    /// Answers `request`, now or from a thread of its own.
    fn request(self: &Arc<Self>, request: Request) -> Result<(), FrameError> {
        let id = request.id.clone();
        match (self.phase(), request.method.as_str()) {
            (Phase::Starting, Initialize::NAME) => {
                let result = request
                    .params::<InitializeParams>()
                    .and_then(|params| self.initialize(&params));
                if result.is_ok() {
                    self.set_phase(Phase::Ready);
                }
                self.out.respond(id, result)
            }
            (Phase::Starting, _) => self
                .out
                .respond::<Empty>(id, Err(invalid_request("`initialize` must come first"))),
            (Phase::Ready, Initialize::NAME) => self
                .out
                .respond::<Empty>(id, Err(invalid_request("already initialized"))),
            (Phase::Ready, ProgramOpen::NAME) => {
                let server = Arc::clone(self);
                self.spawn(id.clone(), move || {
                    let result = request
                        .params::<ProgramOpenParams>()
                        .and_then(|params| server.open(&params));
                    server.out.respond(id, result)
                })
            }
            (Phase::Ready, ProgramClose::NAME) => {
                let result = request
                    .params::<ProgramCloseParams>()
                    .and_then(|params| self.close(&params));
                self.out.respond(id, result)
            }
            (Phase::Ready, DecompileClasses::NAME) => {
                let params = match request.params::<DecompileClassesParams>() {
                    Ok(params) => params,
                    Err(error) => return self.out.respond::<Empty>(id, Err(error)),
                };
                let cancel = CancelFlag::default();
                self.running
                    .lock()
                    .expect("running mutex poisoned")
                    .insert(id.clone(), cancel.clone());
                let server = Arc::clone(self);
                self.spawn(id.clone(), move || {
                    let result = server.decompile(&params, &cancel);
                    server
                        .running
                        .lock()
                        .expect("running mutex poisoned")
                        .remove(&id);
                    server.out.respond(id, result)
                })
            }
            (Phase::Ready, Shutdown::NAME) => {
                self.set_phase(Phase::ShuttingDown);
                self.programs
                    .lock()
                    .expect("programs mutex poisoned")
                    .clear();
                self.out.respond(id, Ok(Empty {}))
            }
            (Phase::ShuttingDown, _) => self
                .out
                .respond::<Empty>(id, Err(invalid_request("shutting down"))),
            (Phase::Ready, method) => {
                let error =
                    ErrorObject::new(ErrorCode::METHOD_NOT_FOUND, format!("no method `{method}`"));
                self.out.respond::<Empty>(id, Err(error))
            }
        }
    }

    /// Runs `work` on a thread of its own; a panic in it, or a thread that
    /// cannot start, is answered with an internal error.
    fn spawn(
        self: &Arc<Self>,
        id: RequestId,
        work: impl FnOnce() -> Result<(), FrameError> + Send + 'static,
    ) -> Result<(), FrameError> {
        let server = Arc::clone(self);
        let thread_id = id.clone();
        let spawned = thread::Builder::new()
            .name(format!("request-{id}"))
            .spawn(move || match panic::catch_unwind(AssertUnwindSafe(work)) {
                Ok(Ok(())) => {}
                // stdout is gone; the reader sees stdin end soon after.
                Ok(Err(err)) => {
                    eprintln!("asc-dexdec-worker: cannot answer request {thread_id}: {err}")
                }
                Err(payload) => {
                    server
                        .running
                        .lock()
                        .expect("running mutex poisoned")
                        .remove(&thread_id);
                    let error = internal(format!(
                        "worker panicked: {}",
                        pool::panic_message(payload.as_ref())
                    ));
                    if let Err(err) = server.out.respond::<Empty>(thread_id.clone(), Err(error)) {
                        eprintln!("asc-dexdec-worker: cannot answer request {thread_id}: {err}");
                    }
                }
            });
        match spawned {
            Ok(_) => Ok(()),
            Err(err) => {
                self.running
                    .lock()
                    .expect("running mutex poisoned")
                    .remove(&id);
                self.out.respond::<Empty>(
                    id,
                    Err(internal(format!("cannot start a request thread: {err}"))),
                )
            }
        }
    }

    /// Handles a notification. `notifications/initialized` carries nothing
    /// the worker needs, and unknown notifications are ignored, as JSON-RPC
    /// requires.
    fn notification(&self, method: &str, params: Value) {
        if method != Cancelled::NAME {
            return;
        }
        match serde_json::from_value::<CancelledParams>(params) {
            // A finished or unknown request has nothing left to cancel.
            Ok(params) => {
                if let Some(cancel) = self
                    .running
                    .lock()
                    .expect("running mutex poisoned")
                    .get(&params.request_id)
                {
                    cancel.cancel();
                }
            }
            Err(err) => eprintln!("asc-dexdec-worker: ignoring malformed {method}: {err}"),
        }
    }

    fn initialize(&self, params: &InitializeParams) -> Result<InitializeResult, ErrorObject> {
        if params.protocol_version < 1 {
            return Err(invalid_params(format!(
                "protocol version {} is not supported",
                params.protocol_version
            )));
        }
        let options = WorkerOptions::from_table(&params.options)?;
        let hash = options.hash();
        // `initialize` is answered once (the phase guards it), so the cell is empty.
        let _ = self.options.set(options);
        eprintln!(
            "asc-dexdec-worker {VERSION} started for {} {}",
            params.client.name, params.client.version
        );
        Ok(InitializeResult::new(
            PROTOCOL_VERSION.min(params.protocol_version),
            Implementation::new(BACKEND_NAME, VERSION),
            hash,
        )
        .inputs(vec![InputKind::Archive])
        .languages(vec![Language::Java, Language::Kotlin])
        // Upstream dexdec has no way to rename a program's classes and members.
        .mappings(false)
        .method_status(true)
        .max_concurrency(MAX_CONCURRENCY))
    }

    fn open(&self, params: &ProgramOpenParams) -> Result<ProgramOpenResult, ErrorObject> {
        let files = match &params.input {
            ProgramInput::Archive { files } => files,
            input => {
                return Err(ErrorObject::new(
                    ErrorCode::UNSUPPORTED_INPUT,
                    format!("dexdec only reads archives, not {:?} input", input.kind()),
                ));
            }
        };
        if params
            .mappings
            .as_ref()
            .is_some_and(|mappings| !mappings.is_empty())
        {
            // asc only sends them when `initialize` said `mappings: true`.
            eprintln!("asc-dexdec-worker: ignoring name mappings; this dexdec cannot apply them");
        }
        let path = match files.as_slice() {
            [file] => file,
            [] => return Err(invalid_params("`input.files` is empty")),
            files => {
                return Err(invalid_params(format!(
                    "dexdec opens one APK, jar or DEX file as a program, not {}",
                    files.len()
                )));
            }
        };
        let started = Instant::now();
        let session = Session::open(path).map_err(|reason| {
            invalid_params(format!("cannot open {}: {reason}", path.display()))
        })?;
        let id = format!("p{}", self.next_program.fetch_add(1, Ordering::Relaxed));
        eprintln!(
            "asc-dexdec-worker: opened {} as {id} in {:.0?}",
            path.display(),
            started.elapsed()
        );
        self.programs
            .lock()
            .expect("programs mutex poisoned")
            .insert(id.clone(), Arc::new(Program::new(session)));
        Ok(ProgramOpenResult::new(ProgramId(id)))
    }

    fn close(&self, params: &ProgramCloseParams) -> Result<Empty, ErrorObject> {
        // A request still running on the session keeps it alive until it ends.
        match self
            .programs
            .lock()
            .expect("programs mutex poisoned")
            .remove(&params.program_id.0)
        {
            Some(_) => Ok(Empty {}),
            None => Err(program_not_found(&params.program_id)),
        }
    }

    /// Answers `decompile/classes`. A cancelled request gets `-32800` and no
    /// partial result.
    fn decompile(
        &self,
        params: &DecompileClassesParams,
        cancel: &CancelFlag,
    ) -> Result<DecompileClassesResult, ErrorObject> {
        let language = source_language(params.language)?;
        let program = self
            .programs
            .lock()
            .expect("programs mutex poisoned")
            .get(&params.program_id.0)
            .cloned()
            .ok_or_else(|| program_not_found(&params.program_id))?;
        let request = ClassRequest {
            options: self
                .options
                .get()
                .cloned()
                .unwrap_or_default()
                .to_decompile_options(),
            language,
            nested: params.nested,
            cancel,
            fault: &self.fault,
        };
        // Requests on one session take turns; a request cancelled while it
        // waits here gives up without decompiling anything.
        let mut turn = program.take(cancel).ok_or_else(cancelled)?;
        let total = params.classes.len();
        let units = turn
            .session()
            .units(&params.classes, &request, |done, class| {
                if let Some(token) = &params.progress_token {
                    #[expect(
                        clippy::cast_precision_loss,
                        reason = "class counts are far below 2^52"
                    )]
                    let progress = ProgressParams::new(token.clone(), done as f64)
                        .total(Some(total as f64))
                        .message(Some(class.to_owned()));
                    // Progress is advisory; a broken stdout fails the response anyway.
                    let _ = self.out.notify::<Progress>(&progress);
                }
            })
            .map_err(|_| cancelled())?;
        if cancel.is_cancelled() {
            return Err(cancelled());
        }
        Ok(DecompileClassesResult::new(units))
    }
}

fn source_language(language: Language) -> Result<SourceLanguage, ErrorObject> {
    match language {
        Language::Java => Ok(SourceLanguage::Java),
        Language::Kotlin => Ok(SourceLanguage::Kotlin),
        language => Err(invalid_params(format!(
            "language {language:?} is not supported"
        ))),
    }
}

fn cancelled() -> ErrorObject {
    ErrorObject::new(ErrorCode::REQUEST_CANCELLED, "request cancelled")
}

fn invalid_request(message: impl Into<String>) -> ErrorObject {
    ErrorObject::new(ErrorCode::INVALID_REQUEST, message)
}

fn internal(message: impl Into<String>) -> ErrorObject {
    ErrorObject::new(ErrorCode::INTERNAL_ERROR, message)
}

fn program_not_found(id: &ProgramId) -> ErrorObject {
    ErrorObject::new(
        ErrorCode::PROGRAM_NOT_FOUND,
        format!("no program session `{}`", id.0),
    )
}

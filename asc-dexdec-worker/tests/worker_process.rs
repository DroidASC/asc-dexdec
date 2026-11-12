//! `asc-dexdec-worker` driven over its stdio as asc's decompile pool drives it
//! (asc-rs `docs/decompile-backends.md` §5–§6).

use std::io::{BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{Receiver, channel};
use std::thread;
use std::time::{Duration, Instant};

use asc_decompile_protocol::{
    ErrorCode, FrameReader, FrameWriter, Message, Notification, PROTOCOL_VERSION, Request, Response,
};
use serde_json::{Value, json};

const WORKER: &str = env!("CARGO_BIN_EXE_asc-dexdec-worker");
const WAIT: Duration = Duration::from_secs(120);

/// Upstream dexdec's own test program.
fn classes_dex() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../dexdec/tests/testcases/classes.dex")
}

/// A worker process and the frames it writes.
struct Worker {
    child: Child,
    stdin: Option<FrameWriter<ChildStdin>>,
    frames: Receiver<Message>,
    next_id: i64,
}

impl Worker {
    fn spawn() -> Self {
        Self::spawn_logging(None, Stdio::null())
    }

    /// A worker with `fault` injected, writing its log to `stderr`.
    fn spawn_logging(fault: Option<&str>, stderr: Stdio) -> Self {
        let mut command = Command::new(WORKER);
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(stderr);
        command.env_remove("ASC_DEXDEC_WORKER_FAULT");
        if let Some(fault) = fault {
            command.env("ASC_DEXDEC_WORKER_FAULT", fault);
        }
        let mut child = command.spawn().expect("the worker runs");
        let stdin = FrameWriter::new(child.stdin.take().unwrap());
        let stdout = child.stdout.take().unwrap();
        let (tx, frames) = channel();
        thread::spawn(move || {
            let mut reader = FrameReader::new(BufReader::new(stdout));
            while let Ok(Some(message)) = reader.read_message() {
                if tx.send(message).is_err() {
                    break;
                }
            }
        });
        Self {
            child,
            stdin: Some(stdin),
            frames,
            next_id: 1,
        }
    }

    fn started() -> Self {
        Self::started_with(None)
    }

    fn started_with(fault: Option<&str>) -> Self {
        let mut worker = Self::spawn_logging(fault, Stdio::null());
        worker
            .call("initialize", initialize_params())
            .expect("initialize succeeds");
        worker.notify("notifications/initialized", json!({}));
        worker
    }

    fn send(&mut self, message: Message) {
        self.stdin
            .as_mut()
            .unwrap()
            .write_message(&message)
            .unwrap();
    }

    fn write_raw(&mut self, line: &[u8]) {
        let stdin = self.stdin.as_mut().unwrap().get_mut();
        stdin.write_all(line).unwrap();
        stdin.flush().unwrap();
    }

    /// Sends a request and returns its id without waiting.
    fn start(&mut self, method: &str, params: Value) -> i64 {
        let id = self.next_id;
        self.next_id += 1;
        self.send(Message::Request(Request {
            id: id.into(),
            method: method.into(),
            params,
        }));
        id
    }

    fn notify(&mut self, method: &str, params: Value) {
        self.send(Message::Notification(Notification {
            method: method.into(),
            params,
        }));
    }

    /// The next response, collecting the notifications before it.
    fn response(&mut self, notifications: &mut Vec<Notification>) -> Response {
        loop {
            match self.frames.recv_timeout(WAIT) {
                Ok(Message::Response(response)) => return response,
                Ok(Message::Notification(notification)) => notifications.push(notification),
                Ok(Message::Request(request)) => panic!("unexpected request {request:?}"),
                Err(err) => panic!("no response from the worker: {err}"),
            }
        }
    }

    fn call(&mut self, method: &str, params: Value) -> Result<Value, (ErrorCode, String)> {
        let id = self.start(method, params);
        let response = self.response(&mut Vec::new());
        assert_eq!(response.id, Some(id.into()));
        response.outcome.map_err(|err| (err.code, err.message))
    }

    fn open(&mut self, file: &Path) -> String {
        let result = self
            .call(
                "program/open",
                json!({"input": {"kind": "archive", "files": [file]}}),
            )
            .expect("program opens");
        result["programId"].as_str().unwrap().to_owned()
    }

    /// Closes stdin and waits for the process to exit.
    fn finish(mut self) -> std::process::ExitStatus {
        self.stdin = None;
        let deadline = Instant::now() + WAIT;
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                return status;
            }
            assert!(
                Instant::now() < deadline,
                "worker did not exit after stdin closed"
            );
            thread::sleep(Duration::from_millis(10));
        }
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        // The process may have exited already.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn initialize_params() -> Value {
    json!({"protocolVersion": PROTOCOL_VERSION, "client": {"name": "test", "version": "0"}, "options": {}})
}

fn decompile_params(program: &str, classes: &[&str]) -> Value {
    json!({"programId": program, "classes": classes, "language": "java"})
}

#[test]
fn initialize_open_decompile_close_shutdown() {
    let mut worker = Worker::spawn();
    let capabilities = worker.call("initialize", initialize_params()).unwrap();
    assert_eq!(capabilities["protocolVersion"], PROTOCOL_VERSION);
    assert_eq!(
        capabilities["backend"],
        json!({"name": "dexdec", "version": env!("CARGO_PKG_VERSION")})
    );
    assert_eq!(capabilities["inputs"], json!(["archive"]));
    assert_eq!(capabilities["languages"], json!(["java", "kotlin"]));
    assert_eq!(capabilities["mappings"], false);
    assert_eq!(capabilities["methodStatus"], true);
    assert_eq!(capabilities["maxConcurrency"], 4);
    let hash = capabilities["optionsHash"].as_str().unwrap();
    assert_eq!(hash.len(), 16, "{hash}");
    worker.notify("notifications/initialized", json!({}));

    let program = worker.open(&classes_dex());
    let id = worker.start(
        "decompile/classes",
        json!({"programId": program, "classes": ["LHelloWorld;", "LSimpleIf;"], "language": "java", "progressToken": "t"}),
    );
    let mut notifications = Vec::new();
    let response = worker.response(&mut notifications);
    assert_eq!(response.id, Some(id.into()));
    let result = response.outcome.unwrap();
    let units = result["units"].as_array().unwrap();
    assert_eq!(units.len(), 2);
    assert_eq!(units[0]["class"], "LHelloWorld;");
    assert_eq!(units[0]["status"], "ok", "{}", units[0]);
    assert_eq!(units[0]["path"], "HelloWorld.java");
    let text = units[0]["text"].as_str().unwrap();
    assert!(
        text.contains("public class HelloWorld {")
            && text.contains("System.out.println(\"Hello, world!\");"),
        "{text}"
    );
    assert!(
        units[0].get("methods").is_none(),
        "an ok unit lists no failed method"
    );
    assert_eq!(units[1]["class"], "LSimpleIf;");
    assert_eq!(units[1]["status"], "ok", "{}", units[1]);
    let progress = notifications
        .iter()
        .filter(|n| n.method == "notifications/progress")
        .map(|n| n.params.clone())
        .collect::<Vec<_>>();
    assert_eq!(progress.len(), 2, "{progress:?}");
    assert_eq!(
        progress[1],
        json!({"progressToken": "t", "progress": 2.0, "total": 2.0, "message": "LSimpleIf;"})
    );

    let params = json!({"programId": program, "classes": ["LHelloWorld;"], "language": "kotlin"});
    let kotlin = worker.call("decompile/classes", params).unwrap();
    assert_eq!(kotlin["units"][0]["status"], "ok");
    assert_eq!(kotlin["units"][0]["path"], "HelloWorld.kt");
    assert!(
        kotlin["units"][0]["text"]
            .as_str()
            .unwrap()
            .contains("class HelloWorld"),
        "{}",
        kotlin["units"][0]
    );

    let missing = worker
        .call(
            "decompile/classes",
            decompile_params(&program, &["LMissing;"]),
        )
        .unwrap();
    assert_eq!(
        missing["units"][0],
        json!({"class": "LMissing;", "status": "failed", "reason": "class not found: LMissing;"})
    );

    assert_eq!(
        worker
            .call("program/close", json!({"programId": program}))
            .unwrap(),
        json!({})
    );
    let (code, _) = worker
        .call(
            "decompile/classes",
            decompile_params(&program, &["LHelloWorld;"]),
        )
        .unwrap_err();
    assert_eq!(code, ErrorCode::PROGRAM_NOT_FOUND);
    let (code, _) = worker
        .call("program/close", json!({"programId": program}))
        .unwrap_err();
    assert_eq!(code, ErrorCode::PROGRAM_NOT_FOUND);

    assert_eq!(worker.call("shutdown", Value::Null).unwrap(), json!({}));
    let (code, _) = worker
        .call(
            "program/open",
            json!({"input": {"kind": "archive", "files": [classes_dex()]}}),
        )
        .unwrap_err();
    assert_eq!(
        code,
        ErrorCode::INVALID_REQUEST,
        "nothing is served after shutdown"
    );
    assert!(worker.finish().success());
}

#[test]
fn indent_option_reaches_the_source() {
    let mut worker = Worker::spawn();
    let params = json!({"protocolVersion": PROTOCOL_VERSION, "client": {"name": "test", "version": "0"}, "options": {"indent": "\t"}});
    let tabbed = worker.call("initialize", params).unwrap();
    let spaced = Worker::spawn()
        .call("initialize", initialize_params())
        .unwrap();
    assert_ne!(
        tabbed["optionsHash"], spaced["optionsHash"],
        "the options are part of the hash"
    );
    let program = worker.open(&classes_dex());
    let result = worker
        .call(
            "decompile/classes",
            decompile_params(&program, &["LHelloWorld;"]),
        )
        .unwrap();
    let text = result["units"][0]["text"].as_str().unwrap();
    assert!(text.contains("\n\tpublic static void main("), "{text}");
}

#[test]
fn cancels_running_and_queued_requests() {
    let mut worker = Worker::started_with(Some("stall:LHelloWorld;"));
    let program = worker.open(&classes_dex());
    let running = worker.start(
        "decompile/classes",
        decompile_params(&program, &["LHelloWorld;"]),
    );
    // Same session: waits for `running` inside the worker.
    let queued = worker.start(
        "decompile/classes",
        decompile_params(&program, &["LSimpleIf;"]),
    );
    thread::sleep(Duration::from_millis(200));

    worker.notify("notifications/cancelled", json!({"requestId": queued}));
    let response = worker.response(&mut Vec::new());
    assert_eq!(
        response.id,
        Some(queued.into()),
        "the queued request answers while the other still runs"
    );
    assert_eq!(
        response.outcome.unwrap_err().code,
        ErrorCode::REQUEST_CANCELLED
    );

    let started = Instant::now();
    worker.notify(
        "notifications/cancelled",
        json!({"requestId": running, "reason": "test"}),
    );
    let response = worker.response(&mut Vec::new());
    assert_eq!(response.id, Some(running.into()));
    assert_eq!(
        response.outcome.unwrap_err().code,
        ErrorCode::REQUEST_CANCELLED,
        "no result after cancellation"
    );
    assert!(started.elapsed() < Duration::from_secs(10));

    // Cancelling a finished or unknown request is harmless.
    worker.notify("notifications/cancelled", json!({"requestId": 999}));
    let result = worker
        .call(
            "decompile/classes",
            decompile_params(&program, &["LSimpleIf;"]),
        )
        .unwrap();
    assert_eq!(result["units"][0]["status"], "ok");
}

#[test]
fn a_panic_fails_only_its_class() {
    let mut worker = Worker::started_with(Some("panic:LHelloWorld;"));
    let program = worker.open(&classes_dex());
    let result = worker
        .call(
            "decompile/classes",
            decompile_params(&program, &["LSimpleIf;", "LHelloWorld;", "LSimpleIf;"]),
        )
        .unwrap();
    let units = result["units"].as_array().unwrap();
    assert_eq!(units[0]["status"], "ok");
    assert_eq!(units[1]["status"], "failed");
    assert_eq!(
        units[1]["reason"], "generation panicked: injected fault in LHelloWorld;",
        "{}",
        units[1]
    );
    assert_eq!(
        units[2]["status"], "ok",
        "the batch goes on after the panic"
    );
    assert_eq!(units[0]["text"], units[2]["text"]);

    let again = worker
        .call(
            "decompile/classes",
            decompile_params(&program, &["LSimpleIf;"]),
        )
        .unwrap();
    assert_eq!(
        again["units"][0]["status"], "ok",
        "the session survives the panic"
    );
}

#[cfg(unix)]
#[test]
fn stray_stdout_goes_to_stderr_not_into_the_frames() {
    let mut worker = Worker::spawn_logging(Some("print:LHelloWorld;"), Stdio::piped());
    let mut stderr = worker.child.stderr.take().unwrap();
    let log = thread::spawn(move || {
        let mut log = String::new();
        std::io::Read::read_to_string(&mut stderr, &mut log).unwrap();
        log
    });
    worker
        .call("initialize", initialize_params())
        .expect("initialize succeeds");
    let program = worker.open(&classes_dex());
    for _ in 0..2 {
        let result = worker
            .call(
                "decompile/classes",
                decompile_params(&program, &["LHelloWorld;", "LSimpleIf;"]),
            )
            .unwrap();
        let units = result["units"].as_array().unwrap();
        assert_eq!(units[0]["status"], "ok", "{}", units[0]);
        assert_eq!(units[1]["status"], "ok", "{}", units[1]);
    }
    assert!(worker.finish().success());
    let log = log.join().unwrap();
    assert_eq!(
        log.matches("stray stdout while decompiling LHelloWorld;")
            .count(),
        2,
        "{log}"
    );
}

#[test]
fn malformed_messages_get_errors_and_the_worker_goes_on() {
    let mut worker = Worker::spawn();
    let (code, _) = worker
        .call(
            "program/open",
            json!({"input": {"kind": "archive", "files": [classes_dex()]}}),
        )
        .unwrap_err();
    assert_eq!(
        code,
        ErrorCode::INVALID_REQUEST,
        "requests before initialize are refused"
    );

    let bad_options = json!({"protocolVersion": 1, "client": {"name": "t", "version": "0"}, "options": {"bogus": 1}});
    let (code, message) = worker.call("initialize", bad_options).unwrap_err();
    assert_eq!(code, ErrorCode::INVALID_PARAMS);
    assert!(message.contains("bogus"), "{message}");
    worker.call("initialize", initialize_params()).unwrap();

    for (line, code) in [
        (&b"this is not json\n"[..], ErrorCode::PARSE_ERROR),
        (b"[1, 2]\n", ErrorCode::INVALID_REQUEST),
    ] {
        worker.write_raw(line);
        let response = worker.response(&mut Vec::new());
        assert_eq!(response.id, None);
        assert_eq!(response.outcome.unwrap_err().code, code);
    }

    let (code, _) = worker.call("no/such/method", json!({})).unwrap_err();
    assert_eq!(code, ErrorCode::METHOD_NOT_FOUND);
    let (code, _) = worker
        .call("decompile/classes", json!({"classes": 3}))
        .unwrap_err();
    assert_eq!(code, ErrorCode::INVALID_PARAMS);
    let slice = json!({"input": {"kind": "slice", "files": [classes_dex()], "manifest": {}}});
    let (code, _) = worker.call("program/open", slice).unwrap_err();
    assert_eq!(code, ErrorCode::UNSUPPORTED_INPUT);
    let (code, _) = worker
        .call(
            "program/open",
            json!({"input": {"kind": "archive", "files": ["/no/such/file.apk"]}}),
        )
        .unwrap_err();
    assert_eq!(code, ErrorCode::INVALID_PARAMS);
    let two = json!({"input": {"kind": "archive", "files": [classes_dex(), classes_dex()]}});
    let (code, message) = worker.call("program/open", two).unwrap_err();
    assert_eq!(code, ErrorCode::INVALID_PARAMS);
    assert!(message.contains("one APK, jar or DEX file"), "{message}");

    let program = worker.open(&classes_dex());
    let result = worker
        .call(
            "decompile/classes",
            decompile_params(&program, &["LHelloWorld;"]),
        )
        .unwrap();
    assert_eq!(result["units"][0]["status"], "ok");
}

/// asc's own fixture of a class with an inner class, from the sibling asc-rs
/// checkout; upstream dexdec's test program has no nested classes.
#[test]
fn nested_classes_answer_with_the_unit_they_are_folded_into() {
    let dex = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../asc-rs/crates/asc-cli/tests/data/nest.dex");
    if !dex.exists() {
        eprintln!("skipped: {} is missing", dex.display());
        return;
    }
    let mut worker = Worker::started();
    let program = worker.open(&dex);
    let result = worker
        .call(
            "decompile/classes",
            decompile_params(&program, &["LNest;", "LNest$Inner;"]),
        )
        .unwrap();
    let units = result["units"].as_array().unwrap();
    assert_eq!(units[0]["class"], "LNest;");
    assert_eq!(units[0]["nestedClasses"], json!(["LNest$Inner;"]));
    assert!(
        units[0]["text"].as_str().unwrap().contains("class Inner {"),
        "{}",
        units[0]
    );
    assert_eq!(
        units[1], units[0],
        "the inner class comes as its outer class's unit"
    );

    // Without folding, the inner class is a unit of its own.
    let params = json!({"programId": program, "classes": ["LNest$Inner;"], "language": "java", "nested": false});
    let alone = worker.call("decompile/classes", params).unwrap();
    assert_eq!(alone["units"][0]["class"], "LNest$Inner;");
    assert_eq!(alone["units"][0]["path"], "Nest$Inner.java");
    assert!(
        alone["units"][0].get("nestedClasses").is_none(),
        "{}",
        alone["units"][0]
    );
}

#[test]
fn stdin_eof_ends_the_worker() {
    let worker = Worker::started();
    assert!(worker.finish().success());
}

#[test]
fn the_binary_says_its_versions_and_refuses_other_arguments() {
    let out = Command::new(WORKER).arg("--version").output().unwrap();
    assert!(out.status.success());
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        format!("asc-dexdec-worker {}\n", env!("CARGO_PKG_VERSION"))
    );
    let out = Command::new(WORKER)
        .arg("--protocol-version")
        .output()
        .unwrap();
    assert!(out.status.success());
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        format!("{PROTOCOL_VERSION}\n")
    );
    for args in [&["--stdio"][..], &["--version", "--protocol-version"]] {
        let out = Command::new(WORKER).args(args).output().unwrap();
        assert_eq!(out.status.code(), Some(2), "{args:?}");
        assert!(out.stdout.is_empty());
        assert!(
            String::from_utf8_lossy(&out.stderr)
                .starts_with("usage: asc-dexdec-worker [--version | --protocol-version]")
        );
    }
}

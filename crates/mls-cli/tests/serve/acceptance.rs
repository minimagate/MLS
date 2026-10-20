//! Exercise the production daemon over JSON-RPC stdio with replay-only model calls.
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, Command, Stdio},
    sync::mpsc::{self, Receiver},
    thread,
    time::{Duration, Instant},
};

const BLOCK: &str = "00000000-0000-4000-8000-000000000001";
const TIMEOUT: Duration = Duration::from_secs(45);

struct Daemon {
    child: Child,
    input: ChildStdin,
    messages: Receiver<String>,
    pending: VecDeque<Value>,
}

impl Daemon {
    fn start(document: &Path, replay: &Path) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_mls"))
            .arg("serve")
            .arg("--replay")
            .arg(replay)
            .env_remove("MLS_LIVE")
            .env_remove("OPENROUTER_API_KEY")
            .env("MLS_WORKSPACE_ROOT", env!("CARGO_MANIFEST_DIR"))
            .current_dir(document.parent().unwrap())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let stdout = child.stdout.take().unwrap();
        let (sender, messages) = mpsc::channel();
        thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                if sender
                    .send(line.unwrap_or_else(|error| format!("<stdout read error: {error}>")))
                    .is_err()
                {
                    break;
                }
            }
        });
        let stderr = child.stderr.take().unwrap();
        thread::spawn(move || {
            let _ = std::io::read_to_string(stderr);
        });
        Self {
            input: child.stdin.take().unwrap(),
            child,
            messages,
            pending: VecDeque::new(),
        }
    }

    fn send(&mut self, value: Value) {
        writeln!(self.input, "{value}").unwrap();
        self.input.flush().unwrap();
    }

    fn receive(&mut self) -> Value {
        if let Some(value) = self.pending.pop_front() {
            return value;
        }
        let line = self
            .messages
            .recv_timeout(TIMEOUT)
            .expect("daemon produced no JSON-RPC message before timeout");
        serde_json::from_str(&line)
            .unwrap_or_else(|error| panic!("invalid daemon output {line:?}: {error}"))
    }

    fn response(&mut self, id: u64) -> Value {
        loop {
            if let Some(position) = self
                .pending
                .iter()
                .position(|message| message.get("id") == Some(&json!(id)))
            {
                return self.pending.remove(position).unwrap();
            }
            let line = self
                .messages
                .recv_timeout(TIMEOUT)
                .expect("daemon produced no JSON-RPC response before timeout");
            let message: Value = serde_json::from_str(&line)
                .unwrap_or_else(|error| panic!("invalid daemon output {line:?}: {error}"));
            if message.get("id") == Some(&json!(id)) {
                return message;
            }
            self.pending.push_back(message);
        }
    }

    fn next(&mut self) -> Value {
        self.receive()
    }

    fn shutdown(mut self, id: u64) {
        self.send(json!({"jsonrpc":"2.0","id":id,"method":"shutdown","params":{}}));
        let response = self.response(id);
        assert!(
            response.get("result").is_some(),
            "shutdown failed: {response}"
        );
        let status = wait_for_exit(&mut self.child);
        assert!(status.success(), "daemon exited unsuccessfully");
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

fn wait_for_exit(child: &mut Child) -> std::process::ExitStatus {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            return status;
        }
        assert!(
            Instant::now() < deadline,
            "daemon did not exit within 10 seconds after shutdown"
        );
        thread::sleep(Duration::from_millis(20));
    }
}

fn replay() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/serve/fixtures/acceptance")
}

fn source(gap: &str) -> String {
    format!(
        "<TeXmacs|2.1.4>\n<\\body>\n{gap}<\\with|mls-id|{BLOCK}><\\theorem>Truth holds.<label|thm:first></theorem></with>\n</body>\n"
    )
}

fn request(id: u64, method: &str, params: Value) -> Value {
    json!({"jsonrpc":"2.0","id":id,"method":method,"params":params})
}

fn state(message: &Value) -> Option<&Value> {
    (message["method"] == "blockStatus").then(|| &message["params"]["data"]["view"])
}

fn has_block_state(message: &Value, status: &str) -> bool {
    state(message).is_some_and(|view| {
        view["blocks"].as_array().is_some_and(|blocks| {
            blocks
                .iter()
                .any(|block| block["block_id"] == BLOCK && block["status"] == status)
        })
    })
}

fn draft_notifications(daemon: &mut Daemon) -> Vec<Value> {
    let mut messages = Vec::new();
    let mut methods: Vec<String> = Vec::new();
    let mut drafted = false;
    while !(drafted
        && ["blockStatus", "reviewRequest", "diagnostics"]
            .iter()
            .all(|method| methods.iter().any(|seen| seen == method)))
    {
        let message = daemon.next();
        if let Some(method) = message["method"].as_str() {
            methods.push(method.to_owned());
        }
        drafted |= has_block_state(&message, "sir_drafted");
        messages.push(message);
    }
    messages
}

#[test]
fn real_daemon_drafts_confirms_proves_persists_and_reopens_a_saved_document() {
    if std::env::var("MLS_LEAN_TESTS").as_deref() != Ok("1") {
        eprintln!("skipped: set MLS_LEAN_TESTS=1 to run the production daemon with real Lean");
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let document = directory.path().join("notes.tm");
    std::fs::write(&document, source("  ")).unwrap();
    let document = document.canonicalize().unwrap();
    let document_text = document.to_string_lossy().into_owned();
    let replay = replay();

    let mut daemon = Daemon::start(&document, &replay);
    daemon.send(request(1, "initialize", json!({})));
    assert!(daemon.response(1)["result"]["protocol"] == 1);
    daemon.send(request(
        2,
        "didOpen",
        json!({"document":document_text,"revision":7,"source":source("  "),"active":BLOCK}),
    ));
    assert!(daemon.response(2).get("result").is_some());

    daemon.send(request(
        3,
        "cursor",
        json!({"document":document_text,"revision":7,"active":null}),
    ));
    assert!(daemon.response(3).get("result").is_some());
    let draft = draft_notifications(&mut daemon);
    assert!(
        draft
            .iter()
            .any(|message| message["method"] == "reviewRequest")
    );
    assert!(
        draft
            .iter()
            .any(|message| message["method"] == "diagnostics")
    );
    let draft_view = draft
        .iter()
        .find(|message| has_block_state(message, "sir_drafted"))
        .and_then(state)
        .unwrap();
    let block = draft_view["blocks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|block| block["block_id"] == BLOCK)
        .unwrap();
    assert!(
        block["lean_statement"]
            .as_str()
            .unwrap_or_else(|| panic!("draft has no accepted Lean statement: {block}"))
            .contains(":= by sorry"),
        "unexpected Lean statement in draft: {block}"
    );

    // A stale edit is refused without changing the current revision.
    daemon.send(request(
        4,
        "didChange",
        json!({"document":document_text,"revision":6,"source":source("  ")}),
    ));
    assert_eq!(daemon.response(4)["error"]["code"], -32001);

    daemon.send(request(
        5,
        "review",
        json!({"document":document_text,"revision":7,"block":BLOCK,"action":"confirm"}),
    ));
    assert!(daemon.response(5).get("result").is_some());
    let proof = loop {
        let message = daemon.next();
        if state(&message).is_some_and(|view| {
            view["blocks"].as_array().is_some_and(|blocks| {
                blocks
                    .iter()
                    .any(|block| block["block_id"] == BLOCK && block["proof_status"] == "verified")
            })
        }) {
            break message;
        }
    };
    assert!(has_block_state(&proof, "statement_confirmed"));

    // Formatting-only revision advances the protocol revision while preserving the saved review.
    daemon.send(request(
        6,
        "didChange",
        json!({"document":document_text,"revision":8,"source":source("\n  ")}),
    ));
    assert!(daemon.response(6).get("result").is_some());
    daemon.send(request(
        7,
        "cursor",
        json!({"document":document_text,"revision":8,"active":null}),
    ));
    assert!(daemon.response(7).get("result").is_some());
    daemon.send(request(
        8,
        "retry",
        json!({"document":document_text,"revision":7,"block":BLOCK}),
    ));
    assert_eq!(daemon.response(8)["error"]["code"], -32001);
    daemon.shutdown(9);

    let store_path = document
        .parent()
        .unwrap()
        .join(".mls/review/notes.tm.store.json");
    let stored: Value = serde_json::from_slice(&std::fs::read(&store_path).unwrap()).unwrap();
    assert!(stored.to_string().contains(BLOCK));
    assert!(stored.to_string().contains("confirmed"));

    // Reopen the same persisted-ID document in a second real process. Queue shutdown alongside
    // didOpen so the process exposes the saved decision before another model job can run.
    let mut reopened = Daemon::start(&document, &replay);
    reopened.send(request(10, "initialize", json!({})));
    assert!(reopened.response(10)["result"].is_object());
    reopened.send(request(
        11,
        "didOpen",
        json!({"document":document_text,"revision":12,"source":source("\n  "),"active":BLOCK}),
    ));
    reopened.send(request(12, "shutdown", json!({})));
    let mut confirmed = false;
    let mut opened = false;
    for _ in 0..8 {
        let message = reopened.next();
        if message["id"] == 11 {
            opened = message.get("result").is_some();
        }
        if state(&message).is_some_and(|view| {
            view["blocks"].as_array().is_some_and(|blocks| {
                blocks
                    .iter()
                    .any(|block| block["block_id"] == BLOCK && block["review"] == "confirmed")
            })
        }) {
            confirmed = true;
        }
        if message["id"] == 12 {
            assert!(
                message.get("result").is_some(),
                "reopened shutdown failed: {message}"
            );
            break;
        }
    }
    assert!(opened, "second daemon did not open the saved document");
    assert!(
        confirmed,
        "second daemon did not reload the persisted confirmation"
    );
    assert!(wait_for_exit(&mut reopened.child).success());
}

//! Prepared native transactions and conservative crash recovery.
use std::fs::File;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::fd::AsRawFd;
use std::process::{Child, ChildStdin, ChildStdout, Command, ExitStatus, Stdio};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::thread::JoinHandle;

use anyhow::{Context, Result, bail, ensure};
use archon_distro::transaction::{TransactionPlan, TransactionRequest};
use archon_distro::{Backend, Distro};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::transaction_state::{ProcessIdentity, Record, Status, Store, state_directory};

const MAX_MESSAGE: u64 = 16 * 1024 * 1024;
const MAX_LOG: u64 = 4 * 1024 * 1024;

#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct Failure {
    pub code: String,
    pub message: String,
}

fn failure(code: &str, message: impl Into<String>) -> anyhow::Error {
    Failure {
        code: code.into(),
        message: message.into(),
    }
    .into()
}

#[derive(Debug, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
enum Event {
    Ready {
        pid: u32,
    },
    Plan {
        plan: Box<TransactionPlan>,
    },
    Checkpoint {
        phase: String,
    },
    Complete,
    Cancelled,
    Error {
        code: String,
        message: String,
        uncertain: bool,
    },
}

struct Session {
    child: Child,
    input: Option<ChildStdin>,
    output: Option<BufReader<ChildStdout>>,
    stop_log: Arc<AtomicBool>,
    logger: Option<JoinHandle<()>>,
    finished: bool,
}

impl Session {
    fn start(
        mut command: Command,
        request: &TransactionRequest,
        apply: bool,
        mut log: File,
    ) -> Result<Self> {
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .context("cannot start native transaction bridge")?;
        let stop_log = Arc::new(AtomicBool::new(false));
        let stopped = stop_log.clone();
        let mut stderr = child.stderr.take().context("missing bridge log pipe")?;
        let logger = std::thread::spawn(move || {
            let fd = stderr.as_raw_fd();
            let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
            if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } != 0
            {
                return;
            }
            let mut buffer = [0u8; 8192];
            let mut written = log.metadata().map(|m| m.len()).unwrap_or(MAX_LOG);
            let mut final_reads = 0;
            loop {
                if stopped.load(Ordering::Acquire) {
                    final_reads += 1;
                    if final_reads > 64 {
                        break;
                    }
                }
                match stderr.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(count) => {
                        let remaining = MAX_LOG.saturating_sub(written).min(count as u64) as usize;
                        if remaining > 0 {
                            if log.write_all(&buffer[..remaining]).is_err() {
                                written = MAX_LOG;
                            } else {
                                written += remaining as u64;
                            }
                        }
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        if stopped.load(Ordering::Acquire) {
                            break;
                        }
                        std::thread::sleep(std::time::Duration::from_millis(20));
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(_) => break,
                }
            }
            let _ = log.sync_all();
        });
        let mut session = Self {
            input: child.stdin.take(),
            output: Some(BufReader::new(
                child.stdout.take().context("missing bridge pipe")?,
            )),
            child,
            stop_log,
            logger: Some(logger),
            finished: false,
        };
        session.send(&json!({"schema_version": 1, "mode": if apply { "apply" } else { "plan" }, "request": request}))?;
        Ok(session)
    }

    fn send(&mut self, message: &Value) -> Result<()> {
        let input = self.input.as_mut().context("bridge input is closed")?;
        serde_json::to_writer(&mut *input, message)?;
        input.write_all(b"\n")?;
        input.flush()?;
        Ok(())
    }

    fn next(&mut self) -> Result<Event> {
        let mut line = Vec::new();
        self.output
            .as_mut()
            .context("bridge output is closed")?
            .by_ref()
            .take(MAX_MESSAGE + 1)
            .read_until(b'\n', &mut line)?;
        ensure!(
            !line.is_empty() && line.len() as u64 <= MAX_MESSAGE,
            "native bridge ended early or sent an oversized event; inspect the transaction log"
        );
        let value: Value =
            serde_json::from_slice(&line).context("invalid native transaction protocol")?;
        ensure!(
            value.get("schema_version") == Some(&json!(1)),
            "native bridge protocol version mismatch"
        );
        Ok(serde_json::from_value(value)?)
    }

    fn finish(&mut self) -> Result<ExitStatus> {
        self.input.take();
        // Closing a rejected/oversized protocol stream prevents the helper
        // from blocking forever while trying to send a plan nobody will read.
        self.output.take();
        let result = self.child.wait();
        self.finished = true;
        self.stop_log.store(true, Ordering::Release);
        if let Some(logger) = self.logger.take() {
            let _ = logger.join();
        }
        Ok(result?)
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        if !self.finished {
            // EOF cancels any unacknowledged native checkpoint. Never kill an
            // RPM transaction that has already started: let native cleanup run.
            let _ = self.finish();
        }
    }
}

fn provider() -> Result<Box<dyn Backend>> {
    let distro = Distro::detect();
    let backend = distro.backend().context("unsupported distro")?;
    ensure!(
        backend.transaction_engine().is_some(),
        "prepared native transactions are not yet supported by {}; the existing adapter remains available",
        backend.name()
    );
    super::native::ensure_mutable_host(&distro)?;
    Ok(backend)
}

fn bridge_command(backend: &dyn Backend, apply: bool) -> Result<Command> {
    let script = backend
        .transaction_bridge()
        .context("native bridge is unavailable")?;
    let mut command = if apply && unsafe { libc::geteuid() } != 0 {
        let mut command = Command::new("sudo");
        command.args(["--", "/usr/bin/python3"]);
        command
    } else {
        Command::new("/usr/bin/python3")
    };
    // Isolated mode ignores PYTHONPATH, user site packages and the working
    // directory. The privileged source is embedded, never read from user state.
    command.args(["-I", "-c", script]);
    command.current_dir("/");
    Ok(command)
}

pub fn prepare(request: TransactionRequest, json_output: bool) -> Result<()> {
    request.validate().map_err(anyhow::Error::msg)?;
    let backend = provider()?;
    let store = Store::open(&state_directory()?, true)?;
    let log = tempfile::tempfile()?;
    emit(
        json_output,
        "progress",
        json!({"phase": "resolving", "backend": backend.transaction_engine()}),
    );
    let mut session = Session::start(
        bridge_command(backend.as_ref(), false)?,
        &request,
        false,
        log,
    )?;
    let plan = read_plan(&mut session, |_| Ok(()))?;
    plan.validate().map_err(anyhow::Error::msg)?;
    ensure!(
        plan.request == request,
        "native plan does not match the requested operation"
    );
    ensure!(
        matches!(session.next()?, Event::Complete),
        "native plan did not complete"
    );
    ensure!(session.finish()?.success(), "native plan helper failed");
    let record = Record::new(plan)?;
    store.save(&record)?;
    if json_output {
        emit(true, "plan", json!({"transaction": record}));
    } else {
        print_plan(&record.plan);
        println!(
            "Saved transaction {}\nApply: archon transaction apply {}",
            record.id, record.id
        );
    }
    Ok(())
}

fn read_plan(
    session: &mut Session,
    mut on_ready: impl FnMut(u32) -> Result<()>,
) -> Result<TransactionPlan> {
    match session.next()? {
        Event::Ready { pid } => {
            on_ready(pid)?;
            session.send(&json!({"continue": true}))?;
        }
        Event::Error { code, message, .. } => return Err(failure(&code, message)),
        _ => bail!("native bridge did not identify its session"),
    }
    match session.next()? {
        Event::Plan { plan } => Ok(*plan),
        Event::Error { code, message, .. } => Err(failure(&code, message)),
        _ => bail!("native bridge did not return a resolved plan"),
    }
}

pub fn apply(id: &str, yes: bool, json_output: bool) -> Result<()> {
    ensure!(
        !json_output || yes,
        "JSON execution requires --yes; review the saved plan first"
    );
    let backend = provider()?;
    let store = Store::open(&state_directory()?, true)?;
    let mut record = store.load(id)?;
    ensure!(
        record.status.can_apply(),
        "transaction {:?} cannot be replayed; run `archon transaction recover {id}`",
        record.status
    );
    ensure!(
        Some(record.plan.backend.as_str()) == backend.transaction_engine(),
        "transaction belongs to another native engine"
    );
    record.owner = ProcessIdentity::current()?;
    record.helper = None;
    record.error = None;
    record.set_status(Status::Resolving);
    store.save(&record)?;
    let outcome = apply_record(&store, &mut record, backend.as_ref(), yes, json_output);
    if let Err(error) = &outcome {
        if record.status != Status::Completed {
            record.set_status(if record.status == Status::Committing {
                Status::Uncertain
            } else {
                Status::Failed
            });
        }
        record.error = Some(format!("{error:#}"));
        // Failure to persist must remain an error. The last durable in-flight
        // checkpoint still lets recovery infer uncertainty after a crash.
        store
            .save(&record)
            .context("cannot persist failure; inspect native history before retrying")?;
    }
    outcome
}

fn apply_record(
    store: &Store,
    record: &mut Record,
    backend: &dyn Backend,
    yes: bool,
    json_output: bool,
) -> Result<()> {
    let log = store.log(&record.id)?;
    let mut session = Session::start(
        bridge_command(backend, true)?,
        &record.plan.request,
        true,
        log,
    )?;
    drive_apply(store, record, &mut session, yes, json_output)
}

fn drive_apply(
    store: &Store,
    record: &mut Record,
    session: &mut Session,
    yes: bool,
    json_output: bool,
) -> Result<()> {
    let actual = read_plan(session, |pid| {
        record.helper = Some(ProcessIdentity::read(pid)?.context("native helper disappeared")?);
        store.save(record)
    })?;
    actual.validate().map_err(anyhow::Error::msg)?;
    if actual != record.plan {
        session.send(&json!({"approve": false}))?;
        return Err(failure(
            "stale_plan",
            "Native packages, repository policy or installed state changed; prepare and review a new transaction",
        ));
    }
    record.set_status(Status::AwaitingConfirmation);
    store.save(record)?;
    if !json_output {
        print_plan(&actual);
    }
    if !yes && !crate::ui::confirm("Execute this native transaction?")? {
        session.send(&json!({"approve": false}))?;
        ensure!(
            matches!(session.next()?, Event::Cancelled),
            "native cancellation failed"
        );
        ensure!(session.finish()?.success(), "native cancellation failed");
        record.set_status(Status::Cancelled);
        store.save(record)?;
        emit(json_output, "cancelled", json!({"id": record.id}));
        return Ok(());
    }
    session.send(&json!({"approve": true}))?;
    loop {
        match session.next()? {
            Event::Checkpoint { phase } => {
                record.set_status(checkpoint(record.status, &phase)?);
                // Never authorize native work before its checkpoint is durable.
                store.save(record)?;
                emit(
                    json_output,
                    "progress",
                    json!({"id": record.id, "phase": phase}),
                );
                session.send(&json!({"continue": true}))?;
            }
            Event::Complete => {
                ensure!(
                    record.status == Status::Committing,
                    "native completion before commit checkpoint"
                );
                ensure!(
                    session.finish()?.success(),
                    "native helper failed after completion; inspect native history"
                );
                record.set_status(Status::Completed);
                store.save(record)?;
                emit(json_output, "complete", json!({"id": record.id}));
                return Ok(());
            }
            Event::Error {
                code,
                message,
                uncertain,
            } => {
                record.set_status(if uncertain {
                    Status::Committing
                } else {
                    Status::Failed
                });
                return Err(failure(&code, message));
            }
            _ => bail!("unexpected native transaction event"),
        }
    }
}

fn checkpoint(current: Status, phase: &str) -> Result<Status> {
    match (current, phase) {
        (Status::AwaitingConfirmation, "downloading") => Ok(Status::Downloading),
        (Status::Downloading, "verifying") => Ok(Status::Verifying),
        (Status::Verifying, "committing") => Ok(Status::Committing),
        _ => bail!("invalid native checkpoint transition: {current:?} -> {phase}"),
    }
}

pub fn list(json_output: bool) -> Result<()> {
    let store = Store::open(&state_directory()?, false)?;
    let records = store.list()?;
    if json_output {
        emit(true, "transactions", json!({"transactions": records}));
    } else {
        for record in records {
            println!(
                "{}  {:?}  {:?}",
                record.id, record.status, record.plan.request.operation
            );
        }
    }
    Ok(())
}

pub fn show(id: &str, json_output: bool) -> Result<()> {
    let store = Store::open(&state_directory()?, false)?;
    let record = store.load(id)?;
    if json_output {
        emit(true, "transaction", json!({"transaction": record}));
    } else {
        println!("Transaction {}: {:?}", record.id, record.status);
        print_plan(&record.plan);
        if let Some(error) = record.error {
            println!("Last error: {error}");
        }
    }
    Ok(())
}

pub fn recover(id: &str, json_output: bool) -> Result<()> {
    let store = Store::open(&state_directory()?, true)?;
    let mut record = store.load(id)?;
    record.recover()?;
    store.save(&record)?;
    let safe_to_retry = record.status.can_apply();
    let guidance = if safe_to_retry {
        format!(
            "Retry with `archon transaction apply {id}`; native state and the saved plan will be revalidated"
        )
    } else if record.status == Status::Completed {
        "Native transaction completed; it will not be replayed".into()
    } else {
        "Run `archon check` and `archon history`, inspect native errors and script state, then prepare a fresh plan. Do not delete native locks or replay this uncertain transaction".into()
    };
    emit(
        json_output,
        "recovery",
        json!({"id": id, "status": record.status, "safe_to_retry": safe_to_retry, "guidance": guidance}),
    );
    Ok(())
}

fn print_plan(plan: &TransactionPlan) {
    println!(
        "Native {} transaction: {:?}",
        plan.backend, plan.request.operation
    );
    for change in &plan.changes {
        println!(
            "  {:?} {} {}.{} [{}; {}] download={} bytes",
            change.action,
            change.package.name,
            change.package.version,
            change.package.architecture,
            change.repository,
            change.reason,
            change.download_bytes
        );
        for previous in &change.replaces {
            println!(
                "    replaces {} {}.{}",
                previous.name, previous.version, previous.architecture
            );
        }
        for next in &change.replaced_by {
            println!(
                "    replaced by {} {}.{}",
                next.name, next.version, next.architecture
            );
        }
    }
    println!(
        "Archive download total: {} bytes; conservative inbound installed size: {} bytes",
        plan.download_bytes, plan.install_bytes
    );
    for warning in &plan.warnings {
        println!("  {warning}");
    }
}

fn emit(json_output: bool, event: &str, fields: Value) {
    if json_output {
        let mut value = json!({"schema_version": 1, "event": event});
        value
            .as_object_mut()
            .unwrap()
            .extend(fields.as_object().unwrap().clone());
        println!("{value}");
    } else {
        match event {
            "progress" => println!(
                "Native transaction phase: {}",
                fields["phase"].as_str().unwrap_or("unknown")
            ),
            "complete" => println!(
                "Transaction {} completed.",
                fields["id"].as_str().unwrap_or("")
            ),
            "cancelled" => println!("Transaction cancelled before execution."),
            "recovery" => println!(
                "{}",
                fields["guidance"]
                    .as_str()
                    .unwrap_or("Inspect native state before continuing.")
            ),
            _ => println!("{event}: {fields}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FAKE_NATIVE: &str = r#"
import json,os,sys
from pathlib import Path
plan=json.loads(sys.argv[1]); marker=Path(sys.argv[2]); record=Path(sys.argv[3]); mode=sys.argv[4]
def emit(event, **fields): print(json.dumps(dict(schema_version=1,event=event,**fields)),flush=True)
def decision():
    line=sys.stdin.readline()
    return json.loads(line) if line else {}
decision()
emit('ready',pid=os.getpid())
if decision() != {'continue':True}: sys.exit(0)
emit('plan',plan=plan)
if decision() != {'approve':True}: sys.exit(0)
for phase in ['downloading','verifying','committing']:
    emit('checkpoint',phase=phase if mode != 'skip' else 'committing')
    if decision() != {'continue':True}: sys.exit(0)
assert json.loads(record.read_text())['status'] == 'committing'
marker.write_text('native commit executed after durable checkpoint')
if mode == 'crash': sys.exit(9)
emit('complete')
"#;

    fn run_fake(mode: &str, stale: bool) -> (Result<()>, Record, bool) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::for_test(dir.path()).unwrap();
        let plan: TransactionPlan =
            serde_json::from_str(include_str!("../../tests/fixtures/native-plan.json")).unwrap();
        let mut record = Record::new(plan.clone()).unwrap();
        record.set_status(Status::Resolving);
        store.save(&record).unwrap();
        let mut actual = plan;
        if stale {
            actual.system_fingerprint = "d".repeat(64);
        }
        let marker = dir.path().join("committed");
        let mut command = Command::new("/usr/bin/python3");
        command
            .args(["-I", "-c", FAKE_NATIVE])
            .arg(serde_json::to_string(&actual).unwrap())
            .arg(&marker)
            .arg(dir.path().join(format!("{}.json", record.id)))
            .arg(mode);
        let mut session = Session::start(
            command,
            &record.plan.request,
            true,
            store.log(&record.id).unwrap(),
        )
        .unwrap();
        let outcome = drive_apply(&store, &mut record, &mut session, true, true);
        drop(session);
        let committed = marker.exists();
        (outcome, record, committed)
    }

    #[test]
    fn changed_native_state_never_receives_execution_approval() {
        let (outcome, record, committed) = run_fake("success", true);
        assert_eq!(
            outcome.unwrap_err().downcast_ref::<Failure>().unwrap().code,
            "stale_plan"
        );
        assert_ne!(record.status, Status::Committing);
        assert!(!committed);
    }

    #[test]
    fn native_commit_requires_a_persisted_checkpoint_and_successful_exit() {
        let (outcome, record, committed) = run_fake("success", false);
        outcome.unwrap();
        assert!(committed);
        assert_eq!(record.status, Status::Completed);
    }

    #[test]
    fn missing_completion_after_commit_requires_uncertain_recovery() {
        let (outcome, mut record, committed) = run_fake("crash", false);
        assert!(outcome.is_err());
        assert!(committed);
        assert_eq!(record.status, Status::Committing);
        record.owner.boot_id = "previous-boot".into();
        record.recover().unwrap();
        assert_eq!(record.status, Status::Uncertain);
        assert!(!record.status.can_apply());
    }

    #[test]
    fn out_of_order_native_events_cannot_start_commit() {
        let (outcome, _, committed) = run_fake("skip", false);
        assert!(outcome.is_err());
        assert!(!committed);
    }

    #[test]
    fn checkpoints_cannot_skip_verification_or_reenter_commit() {
        assert!(checkpoint(Status::AwaitingConfirmation, "committing").is_err());
        assert!(checkpoint(Status::Downloading, "committing").is_err());
        assert!(checkpoint(Status::Committing, "committing").is_err());
        assert_eq!(
            checkpoint(Status::Verifying, "committing").unwrap(),
            Status::Committing
        );
    }
}

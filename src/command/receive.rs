use crate::net::{Event, Events, Node};
use crate::output::{Category, Device, FailedFile, ReceiveReport, ReceivedFile, Reporter};
use crate::util;
use anyhow::Context;
use localsend::http::server::common::save::FileUploadTarget;
use localsend::http::server::v2::{PrepareUploadDecisionV2, SessionEndReasonV2};
use localsend::model::transfer::FileDto;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, oneshot};

const RESULT_CHANNEL_SIZE: usize = 16;
const PROGRESS_CHANNEL_SIZE: usize = 16;

type FileResult = (String, Result<(), String>);

struct Request {
    session_id: String,
    device: Device,
    files: HashMap<String, FileDto>,
    decision_tx: oneshot::Sender<PrepareUploadDecisionV2>,
}

struct Session {
    id: String,
    files: HashMap<String, FileDto>,
    in_progress: HashSet<String>,
    paths: HashMap<String, PathBuf>,
    taken: HashSet<PathBuf>,
    received: Vec<ReceivedFile>,
    failed: Vec<FailedFile>,
    bytes: u64,
    ended: Option<SessionEndReasonV2>,
    results_tx: mpsc::Sender<FileResult>,
    progress_tx: mpsc::Sender<u64>,
}

impl Session {
    fn new(
        id: String,
        files: HashMap<String, FileDto>,
        results_tx: mpsc::Sender<FileResult>,
        progress_tx: mpsc::Sender<u64>,
    ) -> Self {
        Self {
            id,
            files,
            in_progress: HashSet::new(),
            paths: HashMap::new(),
            taken: HashSet::new(),
            received: Vec::new(),
            failed: Vec::new(),
            bytes: 0,
            ended: None,
            results_tx,
            progress_tx,
        }
    }

    fn is_done(&self) -> bool {
        self.ended.is_some() && self.in_progress.is_empty()
    }

    fn start_upload(
        &mut self,
        destination: &Path,
        file: &FileDto,
        file_id: String,
        target_tx: oneshot::Sender<FileUploadTarget>,
    ) -> Option<FailedFile> {
        let Some(path) = util::unique_path(destination, &file.file_name, &self.taken) else {
            return Some(self.record_failure(
                file.file_name.clone(),
                format!("No free file name in {}", destination.display()),
            ));
        };
        let (result_tx, result_rx) = oneshot::channel::<Result<(), String>>();
        let target = FileUploadTarget::Path {
            path: path.clone(),
            result_tx,
            progress_tx: Some(self.progress_tx.clone()),
        };
        if target_tx.send(target).is_err() {
            return None;
        }
        self.taken.insert(path.clone());
        self.paths.insert(file_id.clone(), path);
        self.in_progress.insert(file_id.clone());
        tokio::spawn({
            let results_tx = self.results_tx.clone();
            async move {
                let result = result_rx
                    .await
                    .unwrap_or_else(|_| Err("Upload aborted".to_string()));
                let _ = results_tx.send((file_id, result)).await;
            }
        });
        None
    }

    fn finish_upload(&mut self, file_id: &str, result: Result<(), String>) -> Option<FailedFile> {
        self.in_progress.remove(file_id);
        let file = self.files.get(file_id)?;
        match result {
            Ok(()) => {
                self.bytes += file.size;
                self.received.push(ReceivedFile {
                    name: file.file_name.clone(),
                    size: file.size,
                    path: self.paths.remove(file_id),
                });
                None
            }
            Err(error) => {
                let name = file.file_name.clone();
                Some(self.record_failure(name, error))
            }
        }
    }

    fn record_failure(&mut self, name: String, error: String) -> FailedFile {
        self.failed.push(FailedFile {
            name: name.clone(),
            error: error.clone(),
        });
        FailedFile { name, error }
    }

    fn into_report(mut self, device: Device, started: Instant) -> anyhow::Result<ReceiveReport> {
        let reason = self
            .ended
            .take()
            .with_context(|| format!("{}: The transfer ended early", device.alias()))?;
        anyhow::ensure!(
            matches!(reason, SessionEndReasonV2::Finished),
            "{}: Cancelled by sender ({} of {} received)",
            device.alias(),
            self.received.len(),
            util::count_files(self.files.len()),
        );

        self.received
            .sort_by(|left, right| left.name.cmp(&right.name));
        Ok(ReceiveReport {
            ok: self.failed.is_empty(),
            device,
            text: None,
            files: self.received,
            failed: self.failed,
            bytes: self.bytes,
            seconds: started.elapsed(),
        })
    }
}

pub async fn run(
    node: &Node,
    events: &mut Events,
    destination: &Path,
    reporter: &Reporter,
    timeout: Duration,
) -> anyhow::Result<ReceiveReport> {
    node.announce();
    reporter.log(
        Category::Receive,
        &format!("Waiting for a transfer ({}s)", timeout.as_secs()),
    );

    let request = tokio::time::timeout(timeout, wait(events))
        .await
        .map_err(|_| anyhow::anyhow!("No transfer within {}s", timeout.as_secs()))??;
    let started = Instant::now();

    match message_of(&request.files).map(str::to_string) {
        Some(text) => accept_message(request, text, started),
        None => accept_files(events, destination, reporter, request, started, timeout).await,
    }
}

async fn wait(events: &mut Events) -> anyhow::Result<Request> {
    while let Some(event) = events.next().await {
        match event {
            Event::PrepareUpload {
                session_id,
                host,
                info,
                files,
                decision_tx,
            } => {
                return Ok(Request {
                    session_id,
                    device: Device::from_register(info, host),
                    files,
                    decision_tx,
                });
            }
            Event::ListenerFailed(error) => {
                anyhow::bail!("The HTTP server stopped: {error}")
            }
            _ => {}
        }
    }
    anyhow::bail!("The HTTP server stopped")
}

fn accept_message(
    request: Request,
    text: String,
    started: Instant,
) -> anyhow::Result<ReceiveReport> {
    request
        .decision_tx
        .send(PrepareUploadDecisionV2::Accept(HashSet::new()))
        .map_err(|_| anyhow::anyhow!("{}: The request already ended", request.device.alias()))?;
    Ok(ReceiveReport {
        ok: true,
        device: request.device,
        text: Some(text),
        files: Vec::new(),
        failed: Vec::new(),
        bytes: 0,
        seconds: started.elapsed(),
    })
}

async fn accept_files(
    events: &mut Events,
    destination: &Path,
    reporter: &Reporter,
    request: Request,
    started: Instant,
    timeout: Duration,
) -> anyhow::Result<ReceiveReport> {
    let Request {
        session_id,
        device,
        files,
        decision_tx,
    } = request;

    let accepted: HashSet<String> = files.keys().cloned().collect();
    decision_tx
        .send(PrepareUploadDecisionV2::Accept(accepted))
        .map_err(|_| anyhow::anyhow!("{}: The request already ended", device.alias()))?;

    let offered: u64 = files.values().map(|file| file.size).sum();
    reporter.log(
        Category::Receive,
        &format!(
            "{}: Receiving {} ({})",
            device.alias(),
            util::count_files(files.len()),
            util::format_bytes(offered),
        ),
    );

    let (results_tx, mut results_rx) = mpsc::channel::<FileResult>(RESULT_CHANNEL_SIZE);
    let (progress_tx, mut progress_rx) = mpsc::channel::<u64>(PROGRESS_CHANNEL_SIZE);
    let mut session = Session::new(session_id, files, results_tx, progress_tx);

    let stall = tokio::time::sleep(timeout);
    tokio::pin!(stall);

    while !session.is_done() {
        tokio::select! {
            Some(event) = events.next() => match event {
                Event::FileUpload {
                    session_id: id,
                    file_id,
                    file,
                    target_tx,
                } if id == session.id => {
                    report(reporter, device.alias(), session.start_upload(destination, &file, file_id, target_tx));
                }
                Event::SessionEnd { session_id: id, reason } if id == session.id => {
                    session.ended = Some(reason);
                }
                _ => {}
            },
            Some((file_id, result)) = results_rx.recv() => {
                report(reporter, device.alias(), session.finish_upload(&file_id, result));
            }
            Some(_) = progress_rx.recv() => {}
            () = &mut stall => anyhow::bail!(
                "{}: The transfer stalled for {}s ({} of {} received)",
                device.alias(),
                timeout.as_secs(),
                session.received.len(),
                util::count_files(session.files.len()),
            ),
            else => break,
        }
        stall.as_mut().reset(tokio::time::Instant::now() + timeout);
    }

    session.into_report(device, started)
}

fn report(reporter: &Reporter, alias: &str, failed: Option<FailedFile>) {
    if let Some(failed) = failed {
        reporter.log(
            Category::Receive,
            &format!(
                "{alias}: Failed to receive {}: {}",
                failed.name, failed.error
            ),
        );
    }
}

fn message_of(files: &HashMap<String, FileDto>) -> Option<&str> {
    if files.len() != 1 {
        return None;
    }
    let file = files.values().next()?;
    if file.file_type != "text" && !file.file_type.starts_with("text/") {
        return None;
    }
    file.preview.as_deref()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(file_type: &str, preview: Option<&str>) -> FileDto {
        FileDto {
            id: "id".to_string(),
            file_name: "note.txt".to_string(),
            size: 5,
            file_type: file_type.to_string(),
            sha256: None,
            preview: preview.map(str::to_string),
            metadata: None,
        }
    }

    fn offer(files: Vec<FileDto>) -> HashMap<String, FileDto> {
        files
            .into_iter()
            .enumerate()
            .map(|(index, file)| (index.to_string(), file))
            .collect()
    }

    #[test]
    fn a_single_text_file_with_a_preview_is_a_message() {
        let files = offer(vec![file("text", Some("hello"))]);
        assert_eq!(message_of(&files), Some("hello"));

        let files = offer(vec![file("text/plain", Some("hello"))]);
        assert_eq!(message_of(&files), Some("hello"));
    }

    #[test]
    fn a_text_file_without_a_preview_is_saved_as_a_file() {
        let files = offer(vec![file("text/plain", None)]);
        assert_eq!(message_of(&files), None);
    }

    #[test]
    fn a_non_text_type_is_never_a_message() {
        let files = offer(vec![file("application/octet-stream", Some("hello"))]);
        assert_eq!(message_of(&files), None);

        let files = offer(vec![file("textual/thing", Some("hello"))]);
        assert_eq!(message_of(&files), None);
    }

    #[test]
    fn more_than_one_file_is_never_a_message() {
        let files = offer(vec![
            file("text/plain", Some("hello")),
            file("text/plain", Some("there")),
        ]);
        assert_eq!(message_of(&files), None);
        assert_eq!(message_of(&HashMap::new()), None);
    }
}

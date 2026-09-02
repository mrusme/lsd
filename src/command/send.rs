use crate::net::{Event, Events, Node, resolve};
use crate::output::{Category, Device, Reporter, SendReport, SentFile};
use crate::util;
use anyhow::Context;
use bytes::Bytes;
use futures_util::StreamExt;
use localsend::discovery::HttpChannel;
use localsend::http::client::ClientError;
use localsend::http::client::v2::LsHttpClientV2;
use localsend::http::dto_v2::{
    PrepareUploadRequestDtoV2, PrepareUploadResponseDtoV2, RegisterDtoV2,
};
use localsend::model::discovery::ProtocolType;
use localsend::model::transfer::{FileContent, FileDto, FileMetadata};
use localsend::reqwest;
use std::collections::HashMap;
use std::future::Future;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

const CANCEL_TIMEOUT: Duration = Duration::from_secs(2);
const TEXT_FILE_TYPE: &str = "text";

#[derive(Debug)]
enum Source {
    Path(PathBuf),
    Text(String),
}

#[derive(Debug)]
struct Offered {
    dto: FileDto,
    source: Source,
}

#[derive(Debug)]
struct Accepted<'a> {
    file: &'a Offered,
    token: &'a str,
}

struct Uploaded {
    files: Vec<SentFile>,
    declined: Vec<String>,
    bytes: u64,
    elapsed: Duration,
}

enum Outcome {
    Uploaded,
    Failed(ClientError),
    Interrupted,
    DeclinedByPeer,
    Stalled,
}

struct Peer<'a> {
    client: LsHttpClientV2,
    info: RegisterDtoV2,
    channel: &'a HttpChannel,
    cancel: &'a CancellationToken,
    timeout: Duration,
}

impl<'a> Peer<'a> {
    fn new(
        node: &Node,
        channel: &'a HttpChannel,
        cancel: &'a CancellationToken,
        timeout: Duration,
        fingerprint: &str,
    ) -> anyhow::Result<Self> {
        let expected_fingerprint = match channel.protocol {
            ProtocolType::Https => Some(fingerprint.to_string()),
            ProtocolType::Http => None,
        };
        let client = LsHttpClientV2::try_new(
            &node.identity().key_pem,
            &node.identity().cert_pem,
            expected_fingerprint,
            None,
        )
        .context("Could not create the HTTP client")?;
        Ok(Self {
            client,
            info: node.presence().register_dto(),
            channel,
            cancel,
            timeout,
        })
    }

    async fn abandon(&self, session_id: &str) {
        let _ = tokio::time::timeout(
            CANCEL_TIMEOUT,
            self.client.cancel(
                self.channel.protocol,
                &self.channel.host,
                self.channel.port,
                session_id,
            ),
        )
        .await;
    }
}

pub async fn run(
    node: &Node,
    events: &mut Events,
    selector: &str,
    paths: &[PathBuf],
    reporter: &Reporter,
    timeout: Duration,
) -> anyhow::Result<SendReport> {
    let offered = collect_files(paths)?;
    let cancel = on_interrupt();
    let target = tokio::select! {
        result = resolve::one(node, events, selector, timeout) => result?,
        () = cancel.cancelled() => anyhow::bail!("Interrupted"),
    };
    let device = Device::new(&target.device, &target.channel);
    let peer = Peer::new(
        node,
        &target.channel,
        &cancel,
        timeout,
        device.fingerprint(),
    )?;
    transfer(&peer, events, device, &offered, reporter).await
}

fn on_interrupt() -> CancellationToken {
    let token = CancellationToken::new();
    let trigger = token.clone();
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            trigger.cancel();
        }
    });
    token
}

async fn transfer(
    peer: &Peer<'_>,
    events: &mut Events,
    device: Device,
    offered: &[Offered],
    reporter: &Reporter,
) -> anyhow::Result<SendReport> {
    let started = Instant::now();
    let message = lone_message(offered);
    let payload = PrepareUploadRequestDtoV2 {
        info: peer.info.clone(),
        files: offered
            .iter()
            .map(|file| (file.dto.id.clone(), file.dto.clone()))
            .collect(),
    };
    let prepared = peer
        .client
        .prepare_upload(
            peer.channel.protocol,
            &peer.channel.host,
            peer.channel.port,
            None,
            payload,
            None,
            peer.cancel.clone(),
        )
        .await
        .map_err(|err| prepare_error(device.alias(), err))?;
    let Some(response) = prepared.response else {
        let text =
            message.with_context(|| format!("{}: All files were declined", device.alias()))?;
        return Ok(SendReport {
            ok: true,
            device,
            session_id: None,
            text: Some(text),
            files: Vec::new(),
            declined: Vec::new(),
            bytes: 0,
            seconds: started.elapsed(),
        });
    };

    match upload_all(peer, events, &device, offered, &response, reporter).await {
        Ok(uploaded) => Ok(SendReport {
            ok: uploaded.declined.is_empty(),
            device,
            session_id: Some(response.session_id),
            text: message,
            files: uploaded.files,
            declined: uploaded.declined,
            bytes: uploaded.bytes,
            seconds: uploaded.elapsed,
        }),
        Err(err) => {
            peer.abandon(&response.session_id).await;
            Err(err)
        }
    }
}

async fn upload_all(
    peer: &Peer<'_>,
    events: &mut Events,
    device: &Device,
    offered: &[Offered],
    response: &PrepareUploadResponseDtoV2,
    reporter: &Reporter,
) -> anyhow::Result<Uploaded> {
    let accepted = select_accepted(offered, response, device.alias())?;
    let declined: Vec<String> = offered
        .iter()
        .filter(|file| !response.files.contains_key(&file.dto.id))
        .map(|file| file.dto.file_name.clone())
        .collect();
    let total: u64 = accepted.iter().map(|file| file.file.dto.size).sum();
    reporter.log(
        Category::Send,
        &format!(
            "{}: Sending {} of {} ({})",
            device.alias(),
            accepted.len(),
            util::count_files(offered.len()),
            util::format_bytes(total),
        ),
    );

    let started = Instant::now();
    let mut sent = Vec::new();
    let mut bytes = 0u64;
    for file in &accepted {
        let progress = Arc::new(AtomicU64::new(0));
        let body = upload_body(content(&file.file.source), Arc::clone(&progress));
        let upload = peer.client.upload(
            peer.channel.protocol,
            &peer.channel.host,
            peer.channel.port,
            None,
            &response.session_id,
            &file.file.dto.id,
            file.token,
            body,
            peer.cancel.clone(),
        );
        let progressed = format!(
            "{} of {} sent",
            sent.len(),
            util::count_files(accepted.len())
        );
        match upload_one(upload, peer, events, &progress, &response.session_id).await {
            Outcome::Uploaded => {}
            Outcome::Failed(err) => {
                return Err(upload_error(device.alias(), &file.file.dto.file_name, err));
            }
            Outcome::Interrupted => {
                anyhow::bail!("{}: Interrupted ({progressed})", device.alias())
            }
            Outcome::DeclinedByPeer => {
                anyhow::bail!(
                    "{}: Cancelled by the receiver ({progressed})",
                    device.alias()
                )
            }
            Outcome::Stalled => anyhow::bail!(
                "{}: The transfer stalled for {}s ({progressed})",
                device.alias(),
                peer.timeout.as_secs(),
            ),
        }

        bytes += file.file.dto.size;
        sent.push(SentFile {
            name: file.file.dto.file_name.clone(),
            size: file.file.dto.size,
        });
    }
    Ok(Uploaded {
        files: sent,
        declined,
        bytes,
        elapsed: started.elapsed(),
    })
}

async fn upload_one<F>(
    upload: F,
    peer: &Peer<'_>,
    events: &mut Events,
    progress: &AtomicU64,
    session_id: &str,
) -> Outcome
where
    F: Future<Output = Result<(), ClientError>>,
{
    tokio::pin!(upload);
    let stall = tokio::time::sleep(peer.timeout);
    tokio::pin!(stall);
    let mut seen = 0u64;
    let mut listening = true;
    loop {
        tokio::select! {
            result = &mut upload => return match result {
                Ok(()) => Outcome::Uploaded,
                Err(ClientError::Cancelled) => Outcome::Interrupted,
                Err(err) => Outcome::Failed(err),
            },
            () = peer.cancel.cancelled() => return Outcome::Interrupted,
            event = events.next(), if listening => match event {
                None => listening = false,
                Some(Event::CancelReceived { session_id: id, host })
                    if id == session_id && host == peer.channel.host =>
                {
                    return Outcome::DeclinedByPeer;
                }
                Some(_) => {}
            },
            () = &mut stall => {
                let moved = progress.load(Ordering::Relaxed);
                if moved == seen {
                    return Outcome::Stalled;
                }
                seen = moved;
                stall.as_mut().reset(tokio::time::Instant::now() + peer.timeout);
            }
        }
    }
}

fn select_accepted<'a>(
    offered: &'a [Offered],
    response: &'a PrepareUploadResponseDtoV2,
    alias: &str,
) -> anyhow::Result<Vec<Accepted<'a>>> {
    let by_id: HashMap<&str, &Offered> = offered
        .iter()
        .map(|file| (file.dto.id.as_str(), file))
        .collect();
    let mut accepted = response
        .files
        .iter()
        .map(|(id, token)| {
            by_id
                .get(id.as_str())
                .map(|file| Accepted {
                    file,
                    token: token.as_str(),
                })
                .with_context(|| {
                    format!("{alias}: The receiver accepted a file that was not offered ({id})")
                })
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    accepted.sort_by(|left, right| left.file.dto.file_name.cmp(&right.file.dto.file_name));
    Ok(accepted)
}

fn collect_files(paths: &[PathBuf]) -> anyhow::Result<Vec<Offered>> {
    let mut offered = Vec::with_capacity(paths.len());
    for path in paths {
        offered.push(if util::is_stdin(path) {
            message(read_stdin()?)
        } else {
            offer_file(path)?
        });
    }
    embed_lone_message(&mut offered);
    Ok(offered)
}

fn offer_file(path: &Path) -> anyhow::Result<Offered> {
    let metadata = util::regular_file(path).map_err(|message| anyhow::anyhow!(message))?;
    let file_name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .with_context(|| format!("Path without a file name: {}", path.display()))?;
    Ok(Offered {
        dto: FileDto {
            id: Uuid::new_v4().to_string(),
            file_name,
            size: metadata.len(),
            file_type: mime_guess::from_path(path)
                .first_or_octet_stream()
                .to_string(),
            sha256: None,
            preview: None,
            metadata: FileMetadata::from_fs_metadata(&metadata),
        },
        source: Source::Path(path.to_path_buf()),
    })
}

fn read_stdin() -> anyhow::Result<String> {
    let mut text = String::new();
    std::io::stdin()
        .read_to_string(&mut text)
        .context("Could not read the message from stdin")?;
    let trimmed = text.strip_suffix('\n').unwrap_or(&text);
    let trimmed = trimmed.strip_suffix('\r').unwrap_or(trimmed);
    anyhow::ensure!(!trimmed.is_empty(), "No text on stdin");
    Ok(trimmed.to_string())
}

fn message(text: String) -> Offered {
    Offered {
        dto: FileDto {
            id: Uuid::new_v4().to_string(),
            file_name: format!("{}.txt", Uuid::new_v4()),
            size: text.len() as u64,
            file_type: TEXT_FILE_TYPE.to_string(),
            sha256: None,
            preview: None,
            metadata: None,
        },
        source: Source::Text(text),
    }
}

fn lone_message(offered: &[Offered]) -> Option<String> {
    match offered {
        [only] => match &only.source {
            Source::Text(text) => Some(text.clone()),
            Source::Path(_) => None,
        },
        _ => None,
    }
}

fn embed_lone_message(offered: &mut [Offered]) {
    if let [only] = offered
        && let Source::Text(text) = &only.source
    {
        only.dto.preview = Some(text.clone());
    }
}

fn content(source: &Source) -> FileContent {
    match source {
        Source::Path(path) => FileContent::Path(path.clone()),
        Source::Text(text) => {
            let (tx, rx) = mpsc::channel(1);
            let _ = tx.try_send(Bytes::copy_from_slice(text.as_bytes()));
            FileContent::Stream(rx)
        }
    }
}

fn upload_body(content: FileContent, progress: Arc<AtomicU64>) -> reqwest::Body {
    let stream = ReceiverStream::new(content.into_receiver()).map(move |chunk| {
        progress.fetch_add(chunk.len() as u64, Ordering::Relaxed);
        Ok::<Bytes, std::io::Error>(chunk)
    });
    reqwest::Body::wrap_stream(stream)
}

const PREPARE_REASONS: &[(u16, &str)] = &[
    (401, "PIN required (not supported by lsd)"),
    (403, "Declined"),
    (409, "Blocked by another session"),
    (429, "Too many requests"),
];

const UPLOAD_REASONS: &[(u16, &str)] = &[
    (400, "The receiver rejected the request as incomplete"),
    (403, "The receiver rejected the upload token"),
    (409, "Blocked by another session"),
    (422, "Checksum mismatch"),
    (500, "The receiver could not save it"),
];

fn reason(status: u16, message: Option<String>, reasons: &[(u16, &str)]) -> String {
    reasons
        .iter()
        .find(|(code, _)| *code == status)
        .map_or_else(
            || {
                format!(
                    "Request failed with status {status}{}",
                    message
                        .map(|message| format!(": {message}"))
                        .unwrap_or_default()
                )
            },
            |(_, reason)| (*reason).to_string(),
        )
}

fn prepare_error(alias: &str, error: ClientError) -> anyhow::Error {
    let ClientError::StatusCode(error) = error else {
        return anyhow::Error::new(error).context(format!("{alias}: Could not prepare the upload"));
    };
    anyhow::anyhow!(
        "{alias}: {}",
        reason(error.status, error.message, PREPARE_REASONS)
    )
}

fn upload_error(alias: &str, file_name: &str, error: ClientError) -> anyhow::Error {
    let ClientError::StatusCode(error) = error else {
        return anyhow::Error::new(error).context(format!("{alias}: Failed to upload {file_name}"));
    };
    anyhow::anyhow!(
        "{alias}: Failed to upload {file_name}: {}",
        reason(error.status, error.message, UPLOAD_REASONS)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use localsend::http::StatusCodeError;

    fn status(code: u16, message: Option<&str>) -> ClientError {
        ClientError::StatusCode(StatusCodeError {
            status: code,
            message: message.map(str::to_string),
        })
    }

    fn offered(id: &str, name: &str) -> Offered {
        Offered {
            dto: FileDto {
                id: id.to_string(),
                file_name: name.to_string(),
                size: 1,
                file_type: "application/octet-stream".to_string(),
                sha256: None,
                preview: None,
                metadata: None,
            },
            source: Source::Path(PathBuf::from(name)),
        }
    }

    fn response(files: &[(&str, &str)]) -> PrepareUploadResponseDtoV2 {
        PrepareUploadResponseDtoV2 {
            session_id: "SESSION".to_string(),
            files: files
                .iter()
                .map(|(id, token)| ((*id).to_string(), (*token).to_string()))
                .collect(),
        }
    }

    #[test]
    fn known_prepare_failures_get_a_readable_reason() {
        for (code, expected) in [
            (401, "RX: PIN required (not supported by lsd)"),
            (403, "RX: Declined"),
            (409, "RX: Blocked by another session"),
            (429, "RX: Too many requests"),
        ] {
            assert_eq!(
                format!("{:#}", prepare_error("RX", status(code, None))),
                expected
            );
        }
    }

    #[test]
    fn an_unmapped_status_reports_the_code_and_any_message() {
        assert_eq!(
            format!("{:#}", prepare_error("RX", status(418, None))),
            "RX: Request failed with status 418"
        );
        assert_eq!(
            format!("{:#}", prepare_error("RX", status(500, Some("boom")))),
            "RX: Request failed with status 500: boom"
        );
    }

    #[test]
    fn a_non_status_failure_keeps_its_own_error() {
        let message = format!("{:#}", prepare_error("RX", ClientError::Cancelled));
        assert!(message.starts_with("RX: Could not prepare the upload"));
        assert!(message.contains("Upload cancelled"));
    }

    #[test]
    fn a_message_is_offered_as_a_text_file_with_the_body_in_the_preview() {
        let mut offered = vec![message("Hello World".to_string())];
        embed_lone_message(&mut offered);

        let dto = &offered[0].dto;
        assert_eq!(dto.file_type, "text");
        let (stem, extension) = dto.file_name.rsplit_once('.').unwrap();
        assert_eq!(extension, "txt");
        assert!(Uuid::parse_str(stem).is_ok());
        assert_eq!(dto.size, 11);
        assert_eq!(dto.preview.as_deref(), Some("Hello World"));
        assert!(dto.metadata.is_none());
        assert!(matches!(&offered[0].source, Source::Text(text) if text == "Hello World"));
    }

    #[test]
    fn a_message_alongside_files_is_a_plain_text_file() {
        let mut offered = vec![offered("id-a", "a.txt"), message("Hello World".to_string())];
        embed_lone_message(&mut offered);
        assert!(offered[1].dto.preview.is_none());
        assert_eq!(offered[1].dto.file_type, "text");
    }

    #[tokio::test]
    async fn a_message_body_streams_its_own_bytes() {
        let offered = message("Hello World".to_string());
        let mut rx = match content(&offered.source) {
            FileContent::Stream(rx) => rx,
            FileContent::Path(_) => panic!("expected a stream"),
        };
        let chunk = rx.recv().await.unwrap();
        assert_eq!(&chunk[..], b"Hello World");
        assert!(rx.recv().await.is_none());
    }

    #[test]
    fn upload_failures_name_the_file_and_the_reason() {
        for (code, expected) in [
            (
                400,
                "RX: Failed to upload a.txt: The receiver rejected the request as incomplete",
            ),
            (
                403,
                "RX: Failed to upload a.txt: The receiver rejected the upload token",
            ),
            (
                409,
                "RX: Failed to upload a.txt: Blocked by another session",
            ),
            (422, "RX: Failed to upload a.txt: Checksum mismatch"),
            (
                500,
                "RX: Failed to upload a.txt: The receiver could not save it",
            ),
        ] {
            assert_eq!(
                format!("{:#}", upload_error("RX", "a.txt", status(code, None))),
                expected
            );
        }
        assert_eq!(
            format!(
                "{:#}",
                upload_error("RX", "a.txt", status(418, Some("teapot")))
            ),
            "RX: Failed to upload a.txt: Request failed with status 418: teapot"
        );
        assert!(
            format!("{:#}", upload_error("RX", "a.txt", ClientError::Cancelled))
                .starts_with("RX: Failed to upload a.txt")
        );
    }

    #[test]
    fn a_missing_path_is_named_as_missing() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("nope.txt");
        let error = collect_files(std::slice::from_ref(&missing)).unwrap_err();
        assert_eq!(
            format!("{error:#}"),
            format!("No such file: {}", missing.display())
        );
    }

    #[test]
    fn accepted_files_are_paired_with_their_tokens_and_sorted_by_name() {
        let files = vec![offered("id-b", "b.bin"), offered("id-a", "a.txt")];
        let response = response(&[("id-a", "tok-a"), ("id-b", "tok-b")]);
        let accepted = select_accepted(&files, &response, "RX").unwrap();
        let pairs: Vec<_> = accepted
            .iter()
            .map(|file| (file.file.dto.file_name.as_str(), file.token))
            .collect();
        assert_eq!(pairs, vec![("a.txt", "tok-a"), ("b.bin", "tok-b")]);
    }

    #[test]
    fn a_partial_acceptance_only_yields_the_accepted_files() {
        let files = vec![offered("id-a", "a.txt"), offered("id-b", "b.bin")];
        let response = response(&[("id-b", "tok-b")]);
        let accepted = select_accepted(&files, &response, "RX").unwrap();
        assert_eq!(accepted.len(), 1);
        assert_eq!(accepted[0].file.dto.file_name, "b.bin");
    }

    #[test]
    fn an_unoffered_file_id_is_rejected_rather_than_panicking() {
        let files = vec![offered("id-a", "a.txt")];
        let response = response(&[("id-ghost", "tok")]);
        let error = select_accepted(&files, &response, "RX").unwrap_err();
        assert_eq!(
            format!("{error:#}"),
            "RX: The receiver accepted a file that was not offered (id-ghost)"
        );
    }

    #[test]
    fn declining_everything_yields_no_files() {
        let files = vec![offered("id-a", "a.txt")];
        let response = response(&[]);
        assert!(select_accepted(&files, &response, "RX").unwrap().is_empty());
    }

    #[test]
    fn a_directory_is_not_offered() {
        let dir = tempfile::tempdir().unwrap();
        let error = collect_files(&[dir.path().to_path_buf()]).unwrap_err();
        assert!(format!("{error:#}").starts_with("Not a file: "));
    }

    #[test]
    fn a_regular_file_is_offered_with_its_size_and_type() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("notes.txt");
        std::fs::write(&path, b"hello").unwrap();
        let offered = collect_files(&[path]).unwrap();
        assert_eq!(offered.len(), 1);
        assert_eq!(offered[0].dto.file_name, "notes.txt");
        assert_eq!(offered[0].dto.size, 5);
        assert_eq!(offered[0].dto.file_type, "text/plain");
        assert!(offered[0].dto.sha256.is_none());
    }
}

mod discover;
mod receive;
mod send;

use crate::cli::Command;
use crate::net::Node;
use crate::output::{Report, Reporter};
use crate::settings::Settings;
use std::time::Duration;

pub async fn dispatch(
    command: &Command,
    settings: Settings,
    reporter: &Reporter,
    timeout: Duration,
) -> anyhow::Result<Report> {
    let Settings {
        destination,
        multicast,
        identity,
        presence,
    } = settings;
    let (node, mut events) = Node::start(identity, presence, multicast).await?;
    let report = match command {
        Command::Discover => discover::run(&node, &mut events, timeout)
            .await
            .map(Report::Discover),
        Command::Send { device, files } => {
            send::run(&node, &mut events, device, files, reporter, timeout)
                .await
                .map(Report::Send)
        }
        Command::Receive => receive::run(&node, &mut events, &destination, reporter, timeout)
            .await
            .map(Report::Receive),
    };
    node.shutdown().await;
    report
}

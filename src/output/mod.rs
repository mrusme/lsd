mod device;
mod report;

pub use device::Device;
pub use report::{
    DiscoverReport, FailedFile, ReceiveReport, ReceivedFile, Report, SendReport, SentFile,
};

use serde::Serialize;
use std::process::ExitCode;

#[derive(Clone, Copy)]
pub enum Category {
    Discovery,
    Receive,
    Send,
}

impl Category {
    const fn tag(self) -> &'static str {
        match self {
            Self::Discovery => "D",
            Self::Receive => "R",
            Self::Send => "S",
        }
    }
}

fn block(category: Category, text: &str) -> String {
    text.lines()
        .enumerate()
        .map(|(index, line)| match index {
            0 => format!("{} {line}", category.tag()),
            _ => format!("  {line}"),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[derive(Serialize)]
struct Failure {
    ok: bool,
    error: String,
}

pub struct Reporter {
    json: bool,
}

impl Reporter {
    pub const fn new(json: bool) -> Self {
        Self { json }
    }

    pub fn log(&self, category: Category, text: &str) {
        if !self.json {
            println!("{}", block(category, text));
        }
    }

    pub fn finish(&self, report: &Report) -> anyhow::Result<ExitCode> {
        if self.json {
            println!("{}", serde_json::to_string_pretty(report)?);
        } else {
            println!("{}", report.text());
        }
        Ok(if report.ok() {
            ExitCode::SUCCESS
        } else {
            ExitCode::FAILURE
        })
    }

    pub fn fail(&self, error: &anyhow::Error) {
        if self.json {
            let failure = Failure {
                ok: false,
                error: format!("{error:#}"),
            };
            if let Ok(json) = serde_json::to_string_pretty(&failure) {
                println!("{json}");
            }
        } else {
            eprintln!("{error:#}");
        }
    }
}

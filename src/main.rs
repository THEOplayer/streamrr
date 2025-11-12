use std::path::PathBuf;

use clap::{Parser, Subcommand};
use tokio::spawn;
use tokio_util::sync::CancellationToken;
use url::Url;

use streamrr::record::{RecordError, RecordOptions};
use streamrr::replay::ReplayError;
use streamrr::shared::{MediaSelect, VariantSelect, VariantSelectOptions, abort_on_ctrlc};

/// Record and replay HLS streams.
#[derive(Parser)]
#[command(author, version, about, long_about = None)]
#[command(propagate_version = true)]
#[command(args_conflicts_with_subcommands = true)]
struct Cli {
    #[command(subcommand)]
    command: Option<CliCommand>,
    /// Print license information of this software.
    #[arg(long)]
    license: bool,
}

#[derive(Parser)]
#[command(subcommand_required = true)]
struct CliRequired {
    #[command(flatten)]
    cli: Cli,
}

#[derive(Subcommand)]
enum CliCommand {
    /// Record a HLS VOD or live stream.
    Record {
        /// The URL of the HLS stream. Can be a master or media playlist.
        #[arg(value_name = "URL", value_parser = Url::parse)]
        manifest_url: Url,
        /// The directory path to store the recording of the HLS stream.
        #[arg(value_name = "PATH")]
        recording_path: PathBuf,
        /// The variant stream(s) to record.
        #[arg(short = 'v', long, default_value = "first")]
        variant: VariantSelect,
        /// The audio renditions(s) to record.
        #[arg(long, default_value = "default")]
        audio: MediaSelect,
        /// The video renditions(s) to record.
        #[arg(long, default_value = "default")]
        video: MediaSelect,
        /// The subtitle renditions(s) to record.
        #[arg(long, default_value = "default")]
        subtitle: MediaSelect,
        /// The maximum bandwidth of the variant stream to record.
        ///
        /// Cannot be used when --variant is set.
        #[arg(short = 'b', long, conflicts_with = "variant")]
        bandwidth: Option<u64>,
        /// The start time of the first segment to record, in seconds.
        ///
        /// - If positive, the start time counts from the start of the first media playlist.
        /// - If negative, the start time counts from the end of the first media playlist.
        /// - If unset, the recording starts at the first segment of the first media playlist.
        #[arg(long, allow_hyphen_values = true, verbatim_doc_comment)]
        start: Option<f32>,
        /// The end time of the first segment to record, in seconds.
        ///
        /// - If positive, the end time counts from the start of the first media playlist.
        /// - If negative, the end time counts from the end of the first media playlist.
        /// - If unset, the recording stops at the last segment of the last media playlist.
        #[arg(long, allow_hyphen_values = true, verbatim_doc_comment)]
        end: Option<f32>,
    },
    /// Replay a HLS VOD or live stream.
    Replay {
        /// The directory path of the recording of an HLS stream created by record.
        #[arg(value_name = "PATH")]
        recording_path: PathBuf,
        /// The port on which to run the server.
        #[arg(short = 'p', long, value_name = "PORT", default_value_t = 8080)]
        port: u16,
    },
}

#[tokio::main]
async fn main() {
    let args = Cli::parse();
    let Some(command) = args.command else {
        if args.license {
            println!(include_str!("../LICENSE.md"));
            println!(include_str!("../NOTICE.md"));
            return;
        } else {
            // If --license is not set, then a subcommand is required.
            let Err(e) = CliRequired::try_parse() else {
                unreachable!("expected missing subcommand")
            };
            e.exit()
        }
    };
    match command {
        CliCommand::Record {
            manifest_url,
            recording_path,
            variant,
            audio,
            video,
            subtitle,
            bandwidth,
            start,
            end,
        } => {
            let variant_select = if let Some(bandwidth) = bandwidth {
                VariantSelectOptions::Bandwidth(bandwidth)
            } else {
                VariantSelectOptions::Named(variant)
            };
            let options = RecordOptions {
                start,
                end,
                variant_select,
                audio,
                video,
                subtitle,
            };
            let token = CancellationToken::new();
            let record_task = {
                let token = token.clone();
                spawn(async move {
                    streamrr::record::record(&manifest_url, &recording_path, options, token).await
                })
            };
            match abort_on_ctrlc(record_task, token, RecordError::Cancelled).await {
                Ok(()) => {}
                Err(RecordError::Cancelled) => println!("Stopped recording."),
                Err(e) => {
                    eprintln!("{e}");
                    std::process::exit(1);
                }
            };
        }
        CliCommand::Replay {
            recording_path,
            port,
        } => {
            let token = CancellationToken::new();
            let replay_task = {
                let token = token.clone();
                spawn(async move { streamrr::replay::replay(&recording_path, port, token).await })
            };
            match abort_on_ctrlc(replay_task, token, ReplayError::Cancelled).await {
                Ok(()) => {}
                Err(ReplayError::Cancelled) => println!("Stopped replaying."),
                Err(e) => {
                    eprintln!("{e}");
                    std::process::exit(1);
                }
            };
        }
    }
}

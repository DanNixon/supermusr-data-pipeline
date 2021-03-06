mod file;

use crate::file::{EventFile, TraceFile};
use anyhow::{anyhow, Result};
use clap::Parser;
use flatbuffers::{FileIdentifier, Follow, SkipRootOffset};
use rdkafka::{
    config::ClientConfig,
    consumer::{stream_consumer::StreamConsumer, CommitMode, Consumer},
    message::Message,
};
use std::path::PathBuf;
use streaming_types::{
    aev1_frame_assembled_event_v1_generated::{
        frame_assembled_event_list_message_buffer_has_identifier,
        root_as_frame_assembled_event_list_message,
    },
    dat1_digitizer_analog_trace_v1_generated::{
        digitizer_analog_trace_message_buffer_has_identifier,
        root_as_digitizer_analog_trace_message,
    },
};

#[derive(Debug, Parser)]
#[clap(author, version, about)]
struct Cli {
    #[clap(long)]
    broker: String,

    #[clap(long)]
    username: String,

    #[clap(long)]
    password: String,

    #[clap(long = "group")]
    consumer_group: String,

    #[clap(long)]
    event_topic: Option<String>,

    #[clap(long)]
    event_file: Option<PathBuf>,

    #[clap(long)]
    trace_topic: Option<String>,

    #[clap(long)]
    trace_file: Option<PathBuf>,

    #[clap(long)]
    trace_channels: Option<usize>,
}

#[tokio::main]
async fn main() -> Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let args = Cli::parse();
    log::debug!("Args: {:?}", args);

    let consumer: StreamConsumer = ClientConfig::new()
        .set("bootstrap.servers", &args.broker)
        .set("security.protocol", "sasl_plaintext")
        .set("sasl.mechanisms", "SCRAM-SHA-256")
        .set("sasl.username", &args.username)
        .set("sasl.password", &args.password)
        .set("group.id", &args.consumer_group)
        .set("enable.partition.eof", "false")
        .set("session.timeout.ms", "6000")
        .set("enable.auto.commit", "false")
        .create()?;

    let topics_to_subscribe: Vec<String> = vec![args.event_topic, args.trace_topic]
        .into_iter()
        .flatten()
        .collect();
    let topics_to_subscribe: Vec<&str> = topics_to_subscribe.iter().map(|i| i.as_ref()).collect();
    if topics_to_subscribe.is_empty() {
        return Err(anyhow!(
            "Nothing to do (no message type requested to be saved)"
        ));
    }
    consumer.subscribe(&topics_to_subscribe)?;

    let event_file = match args.event_file {
        Some(filename) => Some(EventFile::create(&filename)?),
        None => None,
    };

    let trace_file = match args.trace_file {
        Some(filename) => Some(TraceFile::create(&filename, args.trace_channels.unwrap())?),
        None => None,
    };

    loop {
        match consumer.recv().await {
            Err(e) => log::warn!("Kafka error: {}", e),
            Ok(msg) => {
                log::debug!(
                    "key: '{:?}', topic: {}, partition: {}, offset: {}, timestamp: {:?}",
                    msg.key(),
                    msg.topic(),
                    msg.partition(),
                    msg.offset(),
                    msg.timestamp()
                );
                if let Some(payload) = msg.payload() {
                    if event_file.is_some()
                        && frame_assembled_event_list_message_buffer_has_identifier(payload)
                    {
                        if let Ok(data) = root_as_frame_assembled_event_list_message(payload) {
                            log::info!("Event packet: status: {:?}", data.status());
                            if let Err(e) = event_file.as_ref().unwrap().push(&data) {
                                log::warn!("Failed to save events to file: {}", e);
                            }
                        }
                        consumer.commit_message(&msg, CommitMode::Async).unwrap();
                    } else if trace_file.is_some()
                        && digitizer_analog_trace_message_buffer_has_identifier(payload)
                    {
                        if let Ok(data) = root_as_digitizer_analog_trace_message(payload) {
                            log::info!(
                                "Trace packet: dig. ID: {}, status: {:?}",
                                data.digitizer_id(),
                                data.status()
                            );
                            if let Err(e) = trace_file.as_ref().unwrap().push(&data) {
                                log::warn!("Failed to save traces to file: {}", e);
                            }
                        }
                        consumer.commit_message(&msg, CommitMode::Async).unwrap();
                    } else {
                        let file_identifier = <SkipRootOffset<FileIdentifier>>::follow(payload, 0);
                        log::warn!(
                            "Unexpected message type \"{:?}\" on topic \"{}\"",
                            file_identifier,
                            msg.topic()
                        );
                    }
                }
            }
        };
    }
}

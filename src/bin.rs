use async_std::task;
use launch::*;
use std::sync::mpsc::channel;
use structopt::StructOpt;

use futures::channel::mpsc;

use async_tungstenite::tungstenite;
use tungstenite::Message as WsMessage;

mod launch;
mod utils;
mod webrtc;

use crate::utils::BsVideoConfig;

/// Example config:
/// ```
/// static CONFIG: &str = r#"{
///     "server": "wss://localhost:8443",
///     "peer_id": 1000,
///     "agent_id": 9000,
///     "pipeline": ["video. ! videoconvert  ! vp8enc deadline=1 ! rtpvp8pay pt=96 ! webrtc. audiotestsrc is-live=true ! opusenc ! rtpopuspay pt=97 ! webrtc."]
/// }"#;
/// let args = serde_json::from_str(CONFIG)?;
/// ```

fn main() -> Result<(), anyhow::Error> {
    let args = Args::from_args();

    // Initialize GStreamer first
    gst::init()?;

    check_plugins()?;

    // Data channel
    let (recv_data_tx, _recv_data_rx) = channel::<String>();
    let (_send_data_tx, send_data_rx) = mpsc::unbounded::<String>();

    // Websocket channel
    let (recv_msg_tx, _recv_msg_rx) = channel::<String>();
    let (_send_msg_tx, send_msg_rx) = mpsc::unbounded::<WsMessage>();

    let video_config = BsVideoConfig {
        format: utils::BsVideoFormat::RGBA8,
        framerate: 10,
        width: 256,
        height: 256,
    };

    task::block_on(async_main(
        args.clone(),
        video_config,
        0,
        None,
        recv_data_tx,
        recv_msg_tx,
        send_data_rx,
        send_msg_rx,
    ))
}

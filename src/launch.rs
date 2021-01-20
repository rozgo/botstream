use mpsc::{UnboundedReceiver, UnboundedSender};
use rand::prelude::*;


use structopt::StructOpt;

use async_std::prelude::*;
use futures::channel::mpsc;
use futures::sink::{Sink, SinkExt};
use futures::stream::StreamExt;

use async_tungstenite::tungstenite;
use tungstenite::Error as WsError;
use tungstenite::Message as WsMessage;

use serde_derive::Deserialize;

use anyhow::{anyhow, bail};

use crate::webrtc;

const DEFAULT_PIPELINE: &str =
    "videotestsrc pattern=ball is-live=true ! vp8enc deadline=1 ! rtpvp8pay pt=96 ! webrtcbin. \
        audiotestsrc is-live=true ! opusenc ! rtpopuspay pt=97 ! webrtcbin. \
        webrtcbin name=webrtcbin";

#[derive(Debug, StructOpt, Deserialize, Clone)]
pub struct Args {
    #[structopt(short, long, default_value = "wss://localhost:8443")]
    pub server: String,
    #[structopt(short, long)]
    pub peer_id: Option<u32>,
    #[structopt(short, long)]
    pub agent_id: Option<u32>,
    #[structopt(default_value = DEFAULT_PIPELINE)]
    pub pipeline: Vec<String>,
}

enum Message {
    Ws(WsMessage),
    Data(String),
    None
}

async fn run(
    app: &webrtc::App,
    send_gst_msg_rx: gst::bus::BusStream,
    send_ws_msg_rx: mpsc::UnboundedReceiver<WsMessage>,
    send_data_rx: mpsc::UnboundedReceiver<String>,
    send_msg_rx: mpsc::UnboundedReceiver<WsMessage>,
    ws: impl Sink<WsMessage, Error = WsError> + Stream<Item = Result<WsMessage, WsError>>,
) -> Result<(), anyhow::Error> {
    // Split the websocket into the Sink and Stream
    let (mut ws_sink, ws_stream) = ws.split();
    // Fuse the Stream, required for the select macro
    let mut ws_stream = ws_stream.fuse();

    let mut send_gst_msg_rx = send_gst_msg_rx.fuse();
    let mut send_ws_msg_rx = send_ws_msg_rx.fuse();

    let mut send_data_rx = send_data_rx.fuse();
    let mut send_msg_rx = send_msg_rx.fuse();

    let (nop_data_tx, nop_data_rx) = mpsc::unbounded::<String>();

    let mut data_setup = false;
    let mut send_data_rx_internal = nop_data_rx.fuse();

    // let s = futures::stream::iter(vec!['a', 'b', 'c']);
    // let (stx, srx) = s.split();

    // And now let's start our message loop
    loop {
        let msg: Message = futures::select! {
            // Handle the WebSocket messages here
            ws_msg = ws_stream.select_next_some() => {
                match ws_msg? {
                    WsMessage::Close(_) => {
                        println!("peer disconnected");
                        break
                    },
                    WsMessage::Ping(data) => Message::Ws(WsMessage::Pong(data)),
                    WsMessage::Pong(_) => Message::None,
                    WsMessage::Binary(_) => Message::None,
                    WsMessage::Text(text) => {
                        app.handle_websocket_message(&text)?;
                        Message::None
                    },
                }
            },
            // Pass the GStreamer messages to the application control logic
            gst_msg = send_gst_msg_rx.select_next_some() => {
                app.handle_pipeline_message(&gst_msg)?;
                Message::None
            },
            // Handle WebSocket messages we created asynchronously
            // to send them out now
            ws_msg = send_ws_msg_rx.select_next_some() => Message::Ws(ws_msg),
            // Handle messages
            ws_msg = send_msg_rx.select_next_some() => Message::Ws(ws_msg),
            // Handle data messages
            dt_msg = send_data_rx.select_next_some() => Message::Data(dt_msg),
            _dt_msg = send_data_rx_internal.select_next_some() => Message::None,            
            // Once we're done, break the loop and return
            complete => break,
        };

        // if !data_setup {
        //     let rx = app.send_data_rx.into_inner().unwrap().into_iter();
        //     send_data_rx_internal = rx.fuse();
        //     // send_data_rx_internal = rx.into();
        //     // if let Some(rx) = app.send_data_rx.lock().unwrap() {
        //     //     send_data_rx_internal.try_next();
        //     // }
        //     // if let Some(tx) = app.send_data_tx.lock().unwrap().as_mut().and_then(|tx| Some(tx)) {
        //     //     tx.unbounded_send(msg);
        //     // }
        // }

        // match msg {
        //     Message::Data(msg) => {

        //     }
        //     Message::Ws(msg) => {

        //     }
        //     _ => ()
        // };

        // If there's a data message to send out, do so now
        // if let Message::Data(msg) = msg {
        //     if let Some(send_data_tx) = send_data_tx {
        //         // send_data_tx.send(msg).await?;
        //     }
        //     else if let Some(tx) = app.send_data_tx.lock().unwrap().as_mut().and_then(|tx| Some(tx)) {
        //         tx.start_send(msg)
        //     }
        // }
        // else}

        // stx.send(2).await?;

        // If there's a message to send out, do so now
        if let Message::Ws(ws_msg) = msg {
            ws_sink.send(ws_msg).await?;
        }

    }

    Ok(())
}

// Check if all GStreamer plugins we require are available
pub fn check_plugins() -> Result<(), anyhow::Error> {
    let needed = [
        "videotestsrc",
        "audiotestsrc",
        "videoconvert",
        "audioconvert",
        "autodetect",
        "opus",
        "vpx",
        "webrtc",
        "nice",
        "dtls",
        "srtp",
        "rtpmanager",
        "rtp",
        "playback",
        "videoscale",
        "audioresample",
    ];

    let registry = gst::Registry::get();
    let missing = needed
        .iter()
        .filter(|n| registry.find_plugin(n).is_none())
        .cloned()
        .collect::<Vec<_>>();

    if !missing.is_empty() {
        bail!("Missing plugins: {:?}", missing);
    } else {
        Ok(())
    }
}

pub async fn async_main(
    args: Args,
    app: webrtc::App,
    send_gst_msg_rx: gst::bus::BusStream,
    send_ws_msg_rx: mpsc::UnboundedReceiver<WsMessage>,
    send_data_rx: mpsc::UnboundedReceiver<String>,
    send_msg_rx: mpsc::UnboundedReceiver<WsMessage>,
) -> Result<(), anyhow::Error> {
    let connector = async_native_tls::TlsConnector::new().danger_accept_invalid_certs(true);

    let (mut ws, _) = async_tungstenite::async_std::connect_async_with_tls_connector(
        &args.server,
        Some(connector),
    )
    .await?;

    println!("connected");

    // Say HELLO to the server and see if it replies with HELLO
    let our_id = if let Some(our_id) = args.agent_id {
        our_id
    } else {
        rand::thread_rng().gen_range(10, 10_000)
    };
    println!("Registering id {} with server", our_id);
    ws.send(WsMessage::Text(format!("HELLO {}", our_id)))
        .await?;

    let msg = ws
        .next()
        .await
        .ok_or_else(|| anyhow!("didn't receive anything"))??;

    if msg != WsMessage::Text("HELLO".into()) {
        bail!("server didn't say HELLO");
    }

    if let Some(peer_id) = args.peer_id {
        // Join the given session
        ws.send(WsMessage::Text(format!("SESSION {}", peer_id)))
            .await?;

        let msg = ws
            .next()
            .await
            .ok_or_else(|| anyhow!("didn't receive anything"))??;

        if msg != WsMessage::Text("SESSION_OK".into()) {
            bail!("server error: {:?}", msg);
        }
    }

    // All good, let's run our message loop
    run(
        &app,
        send_gst_msg_rx,
        send_ws_msg_rx,
        send_data_rx,
        send_msg_rx,
        ws,
    )
    .await
}

pub fn prime_pipeline(pipeline: &mut String) {
    if pipeline.contains("webrtc.") {
        pipeline.push_str(" webrtcbin name=webrtc")
    }
    if pipeline.contains("video.") {
        pipeline.push_str(" appsrc name=video")
    }
}

use mpsc::{UnboundedReceiver, UnboundedSender};
use rand::prelude::*;

use glib::prelude::*;
use gst::prelude::*;

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

use crate::{
    utils::{BsVideoConfig, BsVideoFormat},
    webrtc,
};

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
    None,
}

async fn run(
    args: Args,
    pipeline: gst::Pipeline,
    recv_data_tx: std::sync::mpsc::Sender<String>,
    recv_msg_tx: std::sync::mpsc::Sender<String>,
    send_data_rx: mpsc::UnboundedReceiver<String>,
    send_msg_rx: mpsc::UnboundedReceiver<WsMessage>,
    ws: impl Sink<WsMessage, Error = WsError> + Stream<Item = Result<WsMessage, WsError>>,
) -> Result<(), anyhow::Error> {
    // Split the websocket into the Sink and Stream
    let (mut ws_sink, ws_stream) = ws.split();
    // Fuse the Stream, required for the select macro
    let mut ws_stream = ws_stream.fuse();

    // Channel for outgoing WebSocket messages from other threads
    let (send_ws_msg_tx, send_ws_msg_rx) = mpsc::unbounded::<WsMessage>();

    // Create a stream for handling the GStreamer message asynchronously
    let bus = pipeline.get_bus().unwrap();
    let send_gst_msg_rx = bus.stream();

    let mut send_gst_msg_rx = send_gst_msg_rx.fuse();
    let mut send_ws_msg_rx = send_ws_msg_rx.fuse();

    let mut send_data_rx = send_data_rx.fuse();
    let mut send_msg_rx = send_msg_rx.fuse();

    let (app, (mut data_tx, data_rx)) = webrtc::App::new(
        args.clone(),
        pipeline,
        send_ws_msg_tx,
        recv_data_tx,
        recv_msg_tx,
    )
    .unwrap();
    let mut data_rx = data_rx.fuse();

    // let (nop_data_tx, nop_data_rx) = mpsc::unbounded::<String>();

    // let mut data_setup = false;
    // let mut send_data_rx_internal = nop_data_rx.fuse();

    // let s = futures::stream::iter(vec!['a', 'b', 'c']);
    // let (stx, srx) = s.split();

    // let mut send_data_rx_internal: futures::stream::Fuse<futures::stream::Empty<()>> = futures::stream::empty::<()>().fuse();

    // let mut send_data_rx_internal = Box::pin(futures::stream::empty::<()>().fuse());

    // And now let's start our message loop
    loop {
        // let mut mtx = app.send_data_rx.lock().unwrap();
        // if mtx.is_some() {
        //     let rx = std::mem::replace(&mut *mtx, None).unwrap();
        // }

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
            _dt_msg = data_rx.select_next_some() => Message::None,
            // Once we're done, break the loop and return
            complete => break,
        };

        // If there's a data message to send out, do so now
        if let Message::Data(msg) = msg {
            data_tx.send(msg).await?;
            // if let Some(tx) = app
            //     .send_data_tx
            //     .lock()
            //     .unwrap()
            //     .as_mut()
            //     .and_then(|tx| Some(tx))
            // {
            //     tx.start_send(msg).unwrap();

            //     // app.send_data_rx.lock().unwrap().as_mut().unwrap().
            //     println!("!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!");
            // }
        }
        // If there's a message to send out, do so now
        else if let Message::Ws(ws_msg) = msg {
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
    video_config: BsVideoConfig,
    obj: usize,
    on_need_data: Option<extern "C" fn(obj: usize, data: *mut u8, data_length: u32)>,
    recv_data_tx: std::sync::mpsc::Sender<String>,
    recv_msg_tx: std::sync::mpsc::Sender<String>,
    send_data_rx: mpsc::UnboundedReceiver<String>,
    send_msg_rx: mpsc::UnboundedReceiver<WsMessage>,
) -> Result<(), anyhow::Error> {
    // Specify the format we want to provide as application into the pipeline
    // by creating a video info with the given format and creating caps from it for the appsrc element.
    let format = match video_config.format {
        BsVideoFormat::RGBA8 => gst_video::VideoFormat::Rgba,
    };
    let video_info = gst_video::VideoInfo::builder(format, video_config.width, video_config.height)
        .fps(gst::Fraction::new(video_config.framerate as i32, 1))
        .build()
        .expect("Failed to create video info");

    // Create the GStreamer pipeline
    let mut pipeline = args.pipeline.join(" ");
    prime_pipeline(&mut pipeline);
    let pipeline = gst::parse_launch(pipeline.as_str()).unwrap();

    // Downcast from gst::Element to gst::Pipeline
    let pipeline = pipeline
        .downcast::<gst::Pipeline>()
        .expect("not a pipeline");

    let videosrc = pipeline
        .get_by_name("video")
        .expect("can't find video appsrc");
    let mut videosrc = videosrc
        .dynamic_cast::<gst_app::AppSrc>()
        .expect("Source element is expected to be an appsrc!");
    prepare_appsrc(obj, &mut videosrc, &video_info, on_need_data);

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
        args,
        pipeline,
        recv_data_tx,
        recv_msg_tx,
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

fn prepare_appsrc(
    obj: usize,
    appsrc: &mut gst_app::AppSrc,
    video_info: &gst_video::VideoInfo,
    on_need_data: Option<extern "C" fn(obj: usize, data: *mut u8, data_length: u32)>,
) {
    let video_size = video_info.size();

    appsrc.set_caps(video_info.to_caps().as_ref().ok());
    appsrc.set_property_format(gst::Format::Time);
    appsrc.set_callbacks(
        // Since our appsrc element operates in pull mode (it asks us to provide data),
        // we add a handler for the need-data callback and provide new data from there.
        gst_app::AppSrcCallbacks::builder()
            .need_data(move |appsrc, _| {
                // Create the buffer that can hold exactly one RGBx frame.
                let mut buffer = gst::Buffer::with_size(video_size).unwrap();
                {
                    let buffer = buffer.get_mut().unwrap();
                    // For each frame we produce, we set the timestamp when it should be displayed
                    // (pts = presentation time stamp)
                    // The autovideosink will use this information to display the frame at the right time.
                    let running_time = appsrc.get_current_running_time();
                    buffer.set_pts(running_time);

                    // At this point, buffer is only a reference to an existing memory region somewhere.
                    // When we want to access its content, we have to map it while requesting the required
                    // mode of access (read, read/write).
                    // See: https://gstreamer.freedesktop.org/documentation/plugin-development/advanced/allocation.html
                    let mut data = buffer.map_writable().unwrap();

                    if let Some(on_need_data) = on_need_data {
                        on_need_data(obj, data.as_mut_ptr(), data.get_size() as u32);
                    }
                }

                // appsrc already handles the error here
                let _ = appsrc.push_buffer(buffer);
            })
            .build(),
    );
}

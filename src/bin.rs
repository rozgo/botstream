use async_std::task;
use gst::prelude::*;
use launch::*;
use std::sync::mpsc::channel;
use structopt::StructOpt;

use futures::channel::mpsc;

use async_tungstenite::tungstenite;
use tungstenite::Message as WsMessage;

mod launch;
mod webrtc;

// static CONFIG: &str = r#"{
//     "server": "wss://localhost:8443",
//     "peer_id": 1000,
//     "agent_id": 9000,
//     "pipeline": ["videotestsrc pattern=snow is-live=true ! vp8enc deadline=1 ! rtpvp8pay pt=96 ! webrtc. audiotestsrc is-live=true ! opusenc ! rtpopuspay pt=97 ! webrtc."]
// }"#;

fn main() -> Result<(), anyhow::Error> {
    let args = Args::from_args();
    // let _args = serde_json::from_str(CONFIG)?;

    // Initialize GStreamer first
    gst::init()?;

    check_plugins()?;

    // Create the GStreamer pipeline
    let mut pipeline = args.pipeline.join(" ");
    prime_pipeline(&mut pipeline);
    let pipeline = gst::parse_launch(pipeline.as_str()).unwrap();

    // Downcast from gst::Element to gst::Pipeline
    let pipeline = pipeline
        .downcast::<gst::Pipeline>()
        .expect("not a pipeline");

    if let Some(videosrc) = pipeline.get_by_name("video") {
        let mut videosrc = videosrc
            .dynamic_cast::<gst_app::AppSrc>()
            .expect("Source element is expected to be an appsrc!");
        prepare_appsrc(&mut videosrc);
    }

    // Data channel
    let (recv_data_tx, recv_data_rx) = channel::<String>();
    let (send_data_tx, send_data_rx) = mpsc::unbounded::<String>();

    // Websocket channel
    let (recv_msg_tx, recv_msg_rx) = channel::<String>();
    let (send_msg_tx, send_msg_rx) = mpsc::unbounded::<WsMessage>();

    // Create a stream for handling the GStreamer message asynchronously
    let bus = pipeline.get_bus().unwrap();
    let send_gst_msg_rx = bus.stream();

    // Channel for outgoing WebSocket messages from other threads
    let (send_ws_msg_tx, send_ws_msg_rx) = mpsc::unbounded::<WsMessage>();

    let app = webrtc::App::new(
        args.clone(),
        pipeline,
        send_ws_msg_tx,
        recv_data_tx,
        recv_msg_tx,
    )
    .unwrap();

    task::block_on(async_main(
        args.clone(),
        app,
        send_gst_msg_rx,
        send_ws_msg_rx,
        send_data_rx,
        send_msg_rx,
    ))
}

const WIDTH: usize = 320;
const HEIGHT: usize = 240;

pub fn prepare_appsrc(appsrc: &mut gst_app::AppSrc) {
    // Specify the format we want to provide as application into the pipeline
    // by creating a video info with the given format and creating caps from it for the appsrc element.
    let video_info =
        gst_video::VideoInfo::builder(gst_video::VideoFormat::Bgrx, WIDTH as u32, HEIGHT as u32)
            .fps(gst::Fraction::new(30, 1))
            .build()
            .expect("Failed to create video info");

    appsrc.set_caps(video_info.to_caps().as_ref().ok());
    appsrc.set_property_format(gst::Format::Time);

    // Our frame counter, that is stored in the mutable environment
    // of the closure of the need-data callback
    //
    // Alternatively we could also simply start a new thread that
    // pushes a buffer to the appsrc whenever it wants to, but this
    // is not really needed here. It is *not required* to use the
    // need-data callback.
    let mut i = 0;
    appsrc.set_callbacks(
        // Since our appsrc element operates in pull mode (it asks us to provide data),
        // we add a handler for the need-data callback and provide new data from there.
        // In our case, we told gstreamer that we do 2 frames per second. While the
        // buffers of all elements of the pipeline are still empty, this will be called
        // a couple of times until all of them are filled. After this initial period,
        // this handler will be called (on average) twice per second.
        gst_app::AppSrcCallbacks::builder()
            .need_data(move |appsrc, _| {
                // We only produce 100 frames
                if i == 100 {
                    let _ = appsrc.end_of_stream();
                    return;
                }

                println!("Producing frame {}", i);

                let r = if i % 2 == 0 { 0 } else { 255 };
                let g = if i % 3 == 0 { 0 } else { 255 };
                let b = if i % 5 == 0 { 0 } else { 255 };

                // Create the buffer that can hold exactly one BGRx frame.
                let mut buffer = gst::Buffer::with_size(video_info.size()).unwrap();
                {
                    let buffer = buffer.get_mut().unwrap();
                    // For each frame we produce, we set the timestamp when it should be displayed
                    // (pts = presentation time stamp)
                    // The autovideosink will use this information to display the frame at the right time.
                    buffer.set_pts(i * 500 * gst::MSECOND);

                    // At this point, buffer is only a reference to an existing memory region somewhere.
                    // When we want to access its content, we have to map it while requesting the required
                    // mode of access (read, read/write).
                    // See: https://gstreamer.freedesktop.org/documentation/plugin-development/advanced/allocation.html
                    let mut data = buffer.map_writable().unwrap();

                    for p in data.as_mut_slice().chunks_mut(4) {
                        assert_eq!(p.len(), 4);
                        p[0] = b;
                        p[1] = g;
                        p[2] = r;
                        p[3] = 0;
                    }
                }

                i += 1;

                // appsrc already handles the error here
                let _ = appsrc.push_buffer(buffer);
            })
            .build(),
    );
}

use async_std::task;
use derive_more::{Display, Error};
use glib::prelude::*;
use gst::prelude::*;
use launch::*;
use libc::c_char;
use std::ffi::CStr;
use std::ffi::CString;
use std::sync::mpsc::channel;

use futures::channel::mpsc;

use async_tungstenite::tungstenite;
use tungstenite::Message as WsMessage;

mod launch;
mod webrtc;

#[derive(Debug, Display, Error)]
#[display(fmt = "Missing element {}", _0)]
struct MissingElement(#[error(not(source))] &'static str);

pub struct BsStream {
    obj: usize,
    recv_data_rx: std::sync::mpsc::Receiver<String>,
    send_data_tx: mpsc::UnboundedSender<String>,
    recv_msg_rx: std::sync::mpsc::Receiver<String>,
    send_msg_tx: mpsc::UnboundedSender<WsMessage>,
    on_data_recv: Option<extern "C" fn(obj: usize, config: *const c_char)>,
    on_msg_recv: Option<extern "C" fn(obj: usize, data: *const c_char)>,
}

#[repr(C)]
pub enum BsVideoFormat {
    RGBA8,
}

#[repr(C)]
pub struct VideoConfig {
    format: BsVideoFormat,
    width: u32,
    height: u32,
    framerate: u32,
}

#[no_mangle]
pub extern "C" fn bs_init() {
    gst::init().unwrap()
}

// Example config:
// static CONFIG: &str = r#"{
//     "server": "wss://localhost:8443",
//     "peer_id": 1000,
//     "agent_id": 9000,
//     "pipeline": ["video. ! videoconvert  ! vp8enc deadline=1 ! rtpvp8pay pt=96 ! webrtc. audiotestsrc is-live=true ! opusenc ! rtpopuspay pt=97 ! webrtc."]
// }"#;

#[no_mangle]
pub extern "C" fn bs_start(
    obj: usize,
    config: *const c_char,
    video_config: VideoConfig,
    on_need_data: Option<extern "C" fn(obj: usize, data: *mut u8, data_length: u32)>,
    on_data_recv: Option<extern "C" fn(obj: usize, data: *const c_char)>,
    on_msg_recv: Option<extern "C" fn(obj: usize, data: *const c_char)>,
) -> *mut BsStream {
    let config = unsafe {
        assert!(!config.is_null());
        CStr::from_ptr(config)
    };
    let config = config.to_str().unwrap();

    let args: Args = serde_json::from_str(config).unwrap();

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

    task::spawn(async_main(
        args.clone(),
        app,
        send_gst_msg_rx,
        send_ws_msg_rx,
        send_data_rx,
        send_msg_rx,
    ));

    let stream = BsStream {
        obj,
        recv_data_rx,
        send_data_tx,
        recv_msg_rx,
        send_msg_tx,
        on_data_recv,
        on_msg_recv,
    };
    let stream = Box::new(stream);
    let stream = Box::into_raw(stream);

    return stream;
}

#[no_mangle]
pub extern "C" fn bs_update(stream: *mut BsStream) {
    let stream = unsafe { &mut (*stream) };
    if let Ok(data) = stream.recv_data_rx.try_recv() {
        if let Some(on_data_recv) = stream.on_data_recv {
            let data_cstr = CString::new(data).unwrap();
            on_data_recv(stream.obj, data_cstr.as_ptr());
        }
    }
    if let Ok(data) = stream.recv_msg_rx.try_recv() {
        if let Some(on_msg_recv) = stream.on_msg_recv {
            let data_cstr = CString::new(data).unwrap();
            on_msg_recv(stream.obj, data_cstr.as_ptr());
        }
    }
}

#[no_mangle]
pub extern "C" fn bs_send_data(stream: *mut BsStream, data: *const c_char) {
    let data = unsafe { CStr::from_ptr(data) };
    if let Ok(data) = data.to_str() {
        let stream = unsafe { &mut (*stream) };
        let _res = stream.send_data_tx.unbounded_send(data.to_string());
    }
}

#[no_mangle]
pub extern "C" fn bs_send_msg(stream: *mut BsStream, data: *const c_char) {
    let data = unsafe { CStr::from_ptr(data) };
    if let Ok(data) = data.to_str() {
        let stream = unsafe { &mut (*stream) };
        let _res = stream
            .send_msg_tx
            .unbounded_send(WsMessage::Text(data.to_string()));
    }
}

#[no_mangle]
pub extern "C" fn bs_drop(stream: *mut BsStream) {
    let stream = unsafe { Box::from_raw(stream) };
    drop(stream);
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

use async_std::task;
use derive_more::{Display, Error};
use launch::*;
use libc::c_char;
use std::ffi::CStr;
use std::ffi::CString;
use std::sync::mpsc::channel;

use futures::channel::mpsc;

use async_tungstenite::tungstenite;
use tungstenite::Message as WsMessage;

mod utils;
mod launch;
mod webrtc;

use crate::{utils::BsVideoConfig};

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

#[no_mangle]
pub extern "C" fn bs_init() {
    gst::init().unwrap()
}

/// Example config:
/// ```
/// static CONFIG: &str = r#"{
///     "server": "wss://localhost:8443",
///     "peer_id": 1000,
///     "agent_id": 9000,
///     "pipeline": ["video. ! videoconvert  ! vp8enc deadline=1 ! rtpvp8pay pt=96 ! webrtc. audiotestsrc is-live=true ! opusenc ! rtpopuspay pt=97 ! webrtc."]
/// }"#;
/// let _args = serde_json::from_str(CONFIG)?;
/// ```

#[no_mangle]
pub extern "C" fn bs_start(
    obj: usize,
    config: *const c_char,
    video_config: BsVideoConfig,
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

    // Data channel
    let (recv_data_tx, recv_data_rx) = channel::<String>();
    let (send_data_tx, send_data_rx) = mpsc::unbounded::<String>();

    // Websocket channel
    let (recv_msg_tx, recv_msg_rx) = channel::<String>();
    let (send_msg_tx, send_msg_rx) = mpsc::unbounded::<WsMessage>();

    let main_loop = async_main(
        args.clone(),
        video_config,
        obj,
        on_need_data,
        recv_data_tx,
        recv_msg_tx,
        send_data_rx,
        send_msg_rx,
    );

    task::spawn(main_loop);

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

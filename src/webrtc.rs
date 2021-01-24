use std::pin::Pin;
use std::sync::{Arc, Mutex, Weak};

use futures::channel::mpsc;
use futures::sink::{Sink, SinkExt};
use futures::stream::StreamExt;

use async_tungstenite::tungstenite;
use glib::Object;
use tungstenite::Message as WsMessage;

use gst::gst_element_error;
use gst::prelude::*;

use serde_derive::{Deserialize, Serialize};

use anyhow::{anyhow, bail, Context};
use derive_more::{Display, Error};

use crate::launch::Args;

#[derive(Debug, Display, Error)]
#[display(fmt = "Missing element {}", _0)]
pub struct MissingElement(#[error(not(source))] &'static str);

const STUN_SERVER: &str = "stun://stun.l.google.com:19302";

// upgrade weak reference or return
#[macro_export]
macro_rules! upgrade_weak {
    ($x:ident, $r:expr) => {{
        match $x.upgrade() {
            Some(o) => o,
            None => return $r,
        }
    }};
    ($x:ident) => {
        upgrade_weak!($x, ())
    };
}

// JSON messages we communicate with
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum JsonMsg {
    Ice {
        candidate: String,
        #[serde(rename = "sdpMLineIndex")]
        sdp_mline_index: u32,
    },
    Sdp {
        #[serde(rename = "type")]
        type_: String,
        sdp: String,
    },
    Msg {
        msg: String,
    },
}

// Strong reference to our application state
#[derive(Clone)]
pub struct App(Arc<AppInner>);

// Weak reference to our application state
#[derive(Clone)]
struct AppWeak(Weak<AppInner>);

// Actual application state
// #[derive(Debug)]
pub struct AppInner {
    args: Args,
    pipeline: gst::Pipeline,
    webrtcbin: gst::Element,
    send_ws_msg_tx: Mutex<mpsc::UnboundedSender<WsMessage>>,
    pub send_data_rx: Mutex<Option<mpsc::UnboundedReceiver<String>>>,
    pub send_data_tx: Mutex<Option<mpsc::UnboundedSender<String>>>,
    pub callback_data_tx: Mutex<Pin<Box<dyn Fn(String) + Send>>>,
    recv_data_tx: Mutex<std::sync::mpsc::Sender<String>>,
    recv_msg_tx: Mutex<std::sync::mpsc::Sender<String>>,
}

// To be able to access the App's fields directly
impl std::ops::Deref for App {
    type Target = AppInner;

    fn deref(&self) -> &AppInner {
        &self.0
    }
}

impl AppWeak {
    // Try upgrading a weak reference to a strong one
    fn upgrade(&self) -> Option<App> {
        self.0.upgrade().map(App)
    }
}

pub struct DataChannel(glib::Object);

impl App {
    // Downgrade the strong reference to a weak reference
    fn downgrade(&self) -> AppWeak {
        AppWeak(Arc::downgrade(&self.0))
    }

    pub fn new(
        args: Args,
        pipeline: gst::Pipeline,
        send_ws_msg_tx: mpsc::UnboundedSender<WsMessage>,
        recv_data_tx: std::sync::mpsc::Sender<String>,
        recv_msg_tx: std::sync::mpsc::Sender<String>,
    ) -> Result<
        (
            Self,
            (
                mpsc::UnboundedSender<String>,
                impl futures::stream::Stream<Item = String> + Send,
            ),
        ),
        anyhow::Error,
    > {
        // Get access to the webrtcbin by name
        let webrtcbin = pipeline
            .get_by_name("webrtc")
            .expect("can't find webrtcbin with name: webrtc");

        // Set some properties on webrtcbin
        webrtcbin.set_property_from_str("stun-server", STUN_SERVER);
        webrtcbin.set_property_from_str("bundle-policy", "max-bundle");

        let app = App(Arc::new(AppInner {
            args,
            pipeline,
            webrtcbin,
            send_ws_msg_tx: Mutex::new(send_ws_msg_tx),
            send_data_rx: Mutex::new(None),
            send_data_tx: Mutex::new(None),
            callback_data_tx: Mutex::new(Box::pin(|_| {})),
            recv_data_tx: Mutex::new(recv_data_tx),
            recv_msg_tx: Mutex::new(recv_msg_tx),
        }));

        // Connect to on-negotiation-needed to handle sending an Offer
        if app.args.peer_id.is_some() {
            let app_clone = app.downgrade();
            app.webrtcbin
                .connect("on-negotiation-needed", false, move |values| {
                    let _webrtc = values[0].get::<gst::Element>().unwrap();

                    let app = upgrade_weak!(app_clone, None);
                    if let Err(err) = app.on_negotiation_needed() {
                        gst_element_error!(
                            app.pipeline,
                            gst::LibraryError::Failed,
                            ("Failed to negotiate: {:?}", err)
                        );
                    }

                    None
                })
                .unwrap();
        }

        // Whenever there is a new ICE candidate, send it to the peer
        let app_clone = app.downgrade();
        app.webrtcbin
            .connect("on-ice-candidate", false, move |values| {
                let _webrtc = values[0].get::<gst::Element>().expect("Invalid argument");
                let mlineindex = values[1].get_some::<u32>().expect("Invalid argument");
                let candidate = values[2]
                    .get::<String>()
                    .expect("Invalid argument")
                    .unwrap();

                let app = upgrade_weak!(app_clone, None);

                if let Err(err) = app.on_ice_candidate(mlineindex, candidate) {
                    gst_element_error!(
                        app.pipeline,
                        gst::LibraryError::Failed,
                        ("Failed to send ICE candidate: {:?}", err)
                    );
                }

                None
            })
            .unwrap();

        // Ready state is required for connecitng data channels
        app.pipeline.set_state(gst::State::Ready).unwrap();

        // Create data channels
        let data_channel = app
            .webrtcbin
            .emit(
                "create-data-channel",
                &[&"channel", &None::<gst::Structure>],
            )
            .unwrap()
            .expect("Failed to create data-channel");
        println!("data_channel.type_() {:?}", data_channel.type_());
        let data_channel = data_channel.get::<glib::Object>().expect("Why");
        app.connect_data_channel_signals(&data_channel.clone())
            .unwrap();

        let (data_tx, data_rx) = mpsc::unbounded::<String>();
        let data_rx = if let Some(data_channel) = data_channel {
            let data_channel = Arc::new(Mutex::new(data_channel));
            let data_channel = unsafe { force_send_sync::Send::new(data_channel) };
            data_rx
                .map(move |msg| {
                    data_channel
                        .lock()
                        .unwrap()
                        .emit("send-string", &[&msg])
                        .unwrap();
                    msg
                })
                .boxed()
        } else {
            data_rx.boxed()
        };

        // Handle data channel events
        let app_clone = app.downgrade();
        app.webrtcbin
            .connect("on-data-channel", false, move |values| {
                let channel = values[1].get::<glib::Object>().unwrap();
                let app = upgrade_weak!(app_clone, None);
                app.connect_data_channel_signals(&channel.clone()).unwrap();
                None
            })?;

        // Whenever there is a new stream incoming from the peer, handle it
        let app_clone = app.downgrade();
        app.webrtcbin.connect_pad_added(move |_webrtc, pad| {
            let app = upgrade_weak!(app_clone);

            if let Err(err) = app.on_incoming_stream(pad) {
                gst_element_error!(
                    app.pipeline,
                    gst::LibraryError::Failed,
                    ("Failed to handle incoming stream: {:?}", err)
                );
            }
        });

        // Asynchronously set the pipeline to Playing
        app.pipeline.call_async(|pipeline| {
            // If this fails, post an error on the bus so we exit
            if pipeline.set_state(gst::State::Playing).is_err() {
                gst_element_error!(
                    pipeline,
                    gst::LibraryError::Failed,
                    ("Failed to set pipeline to Playing")
                );
            }
        });

        // Asynchronously set the pipeline to Playing
        app.pipeline.call_async(|pipeline| {
            pipeline
                .set_state(gst::State::Playing)
                .expect("Couldn't set pipeline to Playing");
        });

        Ok((app, (data_tx, data_rx)))
    }

    // Handle WebSocket messages, both our own as well as WebSocket protocol messages
    pub fn handle_websocket_message(&self, msg: &str) -> Result<(), anyhow::Error> {
        if msg.starts_with("ERROR") {
            bail!("Got error message: {}", msg);
        }

        if let Ok(json_msg) = serde_json::from_str::<JsonMsg>(msg) {
            match json_msg {
                JsonMsg::Sdp { type_, sdp } => self.handle_sdp(&type_, &sdp),
                JsonMsg::Ice {
                    sdp_mline_index,
                    candidate,
                } => self.handle_ice(sdp_mline_index, &candidate),
                JsonMsg::Msg { msg } => self.handle_msg(&msg),
            }
        } else {
            self.handle_msg(&msg)
        }

        // let json_msg = serde_json::from_str::<JsonMsg>(msg).unwrap();

        // match json_msg {
        //     JsonMsg::Sdp { type_, sdp } => self.handle_sdp(&type_, &sdp),
        //     JsonMsg::Ice {
        //         sdp_mline_index,
        //         candidate,
        //     } => self.handle_ice(sdp_mline_index, &candidate),
        //     JsonMsg::Msg { msg } => self.handle_msg(&msg),
        // }
    }

    // Handle GStreamer messages coming from the pipeline
    pub fn handle_pipeline_message(&self, message: &gst::Message) -> Result<(), anyhow::Error> {
        use gst::message::MessageView;

        match message.view() {
            MessageView::Error(err) => bail!(
                "Error from element {}: {} ({})",
                err.get_src()
                    .map(|s| String::from(s.get_path_string()))
                    .unwrap_or_else(|| String::from("None")),
                err.get_error(),
                err.get_debug().unwrap_or_else(|| String::from("None")),
            ),
            MessageView::Warning(warning) => {
                println!("Warning: \"{}\"", warning.get_debug().unwrap());
            }
            _ => (),
        }

        Ok(())
    }

    // Whenever webrtcbin tells us that (re-)negotiation is needed, simply ask
    // for a new offer SDP from webrtcbin without any customization and then
    // asynchronously send it to the peer via the WebSocket connection
    fn on_negotiation_needed(&self) -> Result<(), anyhow::Error> {
        println!("starting negotiation");

        let app_clone = self.downgrade();
        let promise = gst::Promise::with_change_func(move |reply| {
            let app = upgrade_weak!(app_clone);

            if let Err(err) = app.on_offer_created(reply) {
                gst_element_error!(
                    app.pipeline,
                    gst::LibraryError::Failed,
                    ("Failed to send SDP offer: {:?}", err)
                );
            }
        });

        self.webrtcbin
            .emit("create-offer", &[&None::<gst::Structure>, &promise])
            .unwrap();

        Ok(())
    }

    // Once webrtcbin has create the offer SDP for us, handle it by sending it to the peer via the
    // WebSocket connection
    fn on_offer_created(
        &self,
        reply: Result<Option<&gst::StructureRef>, gst::PromiseError>,
    ) -> Result<(), anyhow::Error> {
        let reply = match reply {
            Ok(Some(reply)) => reply,
            Ok(None) => {
                bail!("Offer creation future got no reponse");
            }
            Err(err) => {
                bail!("Offer creation future got error reponse: {:?}", err);
            }
        };

        let offer = reply
            .get_value("offer")
            .unwrap()
            .get::<gst_webrtc::WebRTCSessionDescription>()
            .expect("Invalid argument")
            .unwrap();
        self.webrtcbin
            .emit("set-local-description", &[&offer, &None::<gst::Promise>])
            .unwrap();

        println!(
            "sending SDP offer to peer: {}",
            offer.get_sdp().as_text().unwrap()
        );

        let message = serde_json::to_string(&JsonMsg::Sdp {
            type_: "offer".to_string(),
            sdp: offer.get_sdp().as_text().unwrap(),
        })
        .unwrap();

        self.send_ws_msg_tx
            .lock()
            .unwrap()
            .unbounded_send(WsMessage::Text(message))
            .with_context(|| format!("Failed to send SDP offer"))?;

        Ok(())
    }

    // Once webrtcbin has create the answer SDP for us, handle it by sending it to the peer via the
    // WebSocket connection
    fn on_answer_created(
        &self,
        reply: Result<Option<&gst::StructureRef>, gst::PromiseError>,
    ) -> Result<(), anyhow::Error> {
        let reply = match reply {
            Ok(Some(reply)) => reply,
            Ok(None) => {
                bail!("Answer creation future got no reponse");
            }
            Err(err) => {
                bail!("Answer creation future got error reponse: {:?}", err);
            }
        };

        let answer = reply
            .get_value("answer")
            .unwrap()
            .get::<gst_webrtc::WebRTCSessionDescription>()
            .expect("Invalid argument")
            .unwrap();
        self.webrtcbin
            .emit("set-local-description", &[&answer, &None::<gst::Promise>])
            .unwrap();

        println!(
            "sending SDP answer to peer: {}",
            answer.get_sdp().as_text().unwrap()
        );

        let message = serde_json::to_string(&JsonMsg::Sdp {
            type_: "answer".to_string(),
            sdp: answer.get_sdp().as_text().unwrap(),
        })
        .unwrap();

        self.send_ws_msg_tx
            .lock()
            .unwrap()
            .unbounded_send(WsMessage::Text(message))
            .with_context(|| format!("Failed to send SDP answer"))?;

        Ok(())
    }

    // Handle out of signal message
    fn handle_msg(&self, msg: &str) -> Result<(), anyhow::Error> {
        self.recv_msg_tx.lock().unwrap().send(msg.to_string())?;

        Ok(())
    }

    // Handle incoming SDP answers from the peer
    fn handle_sdp(&self, type_: &str, sdp: &str) -> Result<(), anyhow::Error> {
        if type_ == "answer" {
            print!("Received answer:\n{}\n", sdp);

            let ret = gst_sdp::SDPMessage::parse_buffer(sdp.as_bytes())
                .map_err(|_| anyhow!("Failed to parse SDP answer"))?;
            let answer =
                gst_webrtc::WebRTCSessionDescription::new(gst_webrtc::WebRTCSDPType::Answer, ret);

            self.webrtcbin
                .emit("set-remote-description", &[&answer, &None::<gst::Promise>])
                .unwrap();

            Ok(())
        } else if type_ == "offer" {
            print!("Received offer:\n{}\n", sdp);

            let ret = gst_sdp::SDPMessage::parse_buffer(sdp.as_bytes())
                .map_err(|_| anyhow!("Failed to parse SDP offer"))?;

            // And then asynchronously start our pipeline and do the next steps. The
            // pipeline needs to be started before we can create an answer
            let app_clone = self.downgrade();
            self.pipeline.call_async(move |_pipeline| {
                let app = upgrade_weak!(app_clone);

                let offer = gst_webrtc::WebRTCSessionDescription::new(
                    gst_webrtc::WebRTCSDPType::Offer,
                    ret,
                );

                app.0
                    .webrtcbin
                    .emit("set-remote-description", &[&offer, &None::<gst::Promise>])
                    .unwrap();

                let app_clone = app.downgrade();
                let promise = gst::Promise::with_change_func(move |reply| {
                    let app = upgrade_weak!(app_clone);

                    if let Err(err) = app.on_answer_created(reply) {
                        gst_element_error!(
                            app.pipeline,
                            gst::LibraryError::Failed,
                            ("Failed to send SDP answer: {:?}", err)
                        );
                    }
                });

                app.0
                    .webrtcbin
                    .emit("create-answer", &[&None::<gst::Structure>, &promise])
                    .unwrap();
            });

            Ok(())
        } else {
            bail!("Sdp type is not \"answer\" but \"{}\"", type_)
        }
    }

    // Handle incoming ICE candidates from the peer by passing them to webrtcbin
    fn handle_ice(&self, sdp_mline_index: u32, candidate: &str) -> Result<(), anyhow::Error> {
        self.webrtcbin
            .emit("add-ice-candidate", &[&sdp_mline_index, &candidate])
            .unwrap();

        Ok(())
    }

    // Asynchronously send ICE candidates to the peer via the WebSocket connection as a JSON
    // message
    fn on_ice_candidate(&self, mlineindex: u32, candidate: String) -> Result<(), anyhow::Error> {
        let message = serde_json::to_string(&JsonMsg::Ice {
            candidate,
            sdp_mline_index: mlineindex,
        })
        .unwrap();

        self.send_ws_msg_tx
            .lock()
            .unwrap()
            .unbounded_send(WsMessage::Text(message))
            .with_context(|| format!("Failed to send ICE candidate"))?;

        Ok(())
    }

    // Whenever there's a new incoming, encoded stream from the peer create a new decodebin
    fn on_incoming_stream(&self, pad: &gst::Pad) -> Result<(), anyhow::Error> {
        // Early return for the source pads we're adding ourselves
        if pad.get_direction() != gst::PadDirection::Src {
            return Ok(());
        }

        let decodebin = gst::ElementFactory::make("decodebin", None).unwrap();
        let app_clone = self.downgrade();
        decodebin.connect_pad_added(move |_decodebin, pad| {
            let app = upgrade_weak!(app_clone);

            if let Err(err) = app.on_incoming_decodebin_stream(pad) {
                gst_element_error!(
                    app.pipeline,
                    gst::LibraryError::Failed,
                    ("Failed to handle decoded stream: {:?}", err)
                );
            }
        });

        self.pipeline.add(&decodebin).unwrap();
        decodebin.sync_state_with_parent().unwrap();

        let sinkpad = decodebin.get_static_pad("sink").unwrap();
        pad.link(&sinkpad).unwrap();

        Ok(())
    }

    // Handle a newly decoded decodebin stream and depending on its type, create the relevant
    // elements or simply ignore it
    fn on_incoming_decodebin_stream(&self, pad: &gst::Pad) -> Result<(), anyhow::Error> {
        let caps = pad.get_current_caps().unwrap();
        let name = caps.get_structure(0).unwrap().get_name();

        let sink = if name.starts_with("video/") {
            gst::parse_bin_from_description(
                "queue ! videoconvert ! videoscale ! autovideosink",
                true,
            )?
        } else if name.starts_with("audio/") {
            gst::parse_bin_from_description(
                "queue ! audioconvert ! audioresample ! autoaudiosink",
                true,
            )?
        } else {
            println!("Unknown pad {:?}, ignoring", pad);
            return Ok(());
        };

        self.pipeline.add(&sink).unwrap();
        sink.sync_state_with_parent()
            .with_context(|| format!("can't start sink for stream {:?}", caps))?;

        let sinkpad = sink.get_static_pad("sink").unwrap();
        pad.link(&sinkpad)
            .with_context(|| format!("can't link sink for stream {:?}", caps))?;

        Ok(())
    }

    fn connect_data_channel_signals(
        &self,
        data_channel: &Option<glib::Object>,
    ) -> Result<(), anyhow::Error> {
        let data_channel = data_channel
            .as_ref()
            .expect("Failed to extract data channel.");

        let app_clone = self.downgrade();
        data_channel
            .connect("on-open", false, move |values| {
                // let data_channel = values[0].get::<glib::Object>().unwrap().unwrap();
                // data_channel
                //     .emit("send-string", &[&"Hi from BotStream!"])
                //     .unwrap();
                // let app = upgrade_weak!(app_clone, None);

                // let (tx, rx) = mpsc::unbounded::<String>();
                // let p = rx.map(|msg| {
                //     data_channel
                //     .emit("send-string", &[&msg])
                //     .unwrap();
                //     msg
                // });
                // let pp: mpsc::UnboundedReceiver<String> = p.into_inner();

                // let mut mtx = app.send_data_tx.lock().unwrap();
                // *mtx = Some(tx);

                // let mut mrx = app.send_data_rx.lock().unwrap();
                // *mrx = Some(pp);

                // let callback = move |msg: String| {
                //     data_channel.clone()
                //     .emit("send-string", &[&msg])
                //     .unwrap();
                // };

                // let mut mtx = app.callback_data_tx.lock().unwrap();
                // *mtx = Box::pin(callback.clone());

                None
            })
            .unwrap();

        let app_clone = self.downgrade();
        data_channel
            .connect("on-message-string", false, move |values| {
                let message = values[1].get::<String>().unwrap().unwrap();
                let app = upgrade_weak!(app_clone, None);
                app.recv_data_tx
                    .lock()
                    .unwrap()
                    .send(message.clone())
                    .with_context(|| format!("Failed to send data message"))
                    .unwrap_or_default();
                None
            })
            .unwrap();

        Ok(())
    }
}

// Make sure to shut down the pipeline when it goes out of scope
// to release any system resources
impl Drop for AppInner {
    fn drop(&mut self) {
        let _ = self.pipeline.set_state(gst::State::Null);
    }
}

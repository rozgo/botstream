#include <cstdarg>
#include <cstdint>
#include <cstdlib>
#include <ostream>
#include <new>

enum class BsVideoFormat {
  RGBA8,
};

struct BsStream;

struct BsVideoConfig {
  BsVideoFormat format;
  uint32_t width;
  uint32_t height;
  uint32_t framerate;
};

extern "C" {

void bs_init();

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
BsStream *bs_start(uintptr_t obj,
                   const char *config,
                   BsVideoConfig video_config,
                   void (*on_need_data)(uintptr_t obj, uint8_t *data, uint32_t data_length),
                   void (*on_data_recv)(uintptr_t obj, const char *data),
                   void (*on_msg_recv)(uintptr_t obj, const char *data));

void bs_update(BsStream *stream);

void bs_send_data(BsStream *stream, const char *data);

void bs_send_msg(BsStream *stream, const char *data);

void bs_drop(BsStream *stream);

} // extern "C"

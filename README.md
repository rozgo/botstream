# WebRTC Distributed Agents

## Dependencies

### GStreamer
```
sudo apt-get install -y gstreamer1.0-tools gstreamer1.0-nice gstreamer1.0-plugins-bad gstreamer1.0-plugins-ugly gstreamer1.0-plugins-good libgstreamer1.0-dev git libglib2.0-dev libgstreamer-plugins-bad1.0-dev libsoup2.4-dev libjson-glib-dev
```

### Rust
```
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

```

### cbindgen
```
cargo install --force cbindgen
```

## Quick test
```
./launch_servers.sh
./launch_agents.sh
```

## Launch agent with custom pipeline
```
cargo run -- --agent-id 9000 --peer-id 1000 \
    "videotestsrc pattern=snow is-live=true ! vp8enc deadline=1 ! rtpvp8pay pt=96 ! webrtc." \
    "audiotestsrc is-live=true ! opusenc ! rtpopuspay pt=97 ! webrtc."
```

```
cargo run -- --agent-id 9000 --peer-id 1000  \
    "video. ! videoconvert  ! vp8enc deadline=1 ! rtpvp8pay pt=96 ! webrtc."  \
    "audiotestsrc is-live=true ! opusenc ! rtpopuspay pt=97 ! webrtc."
```

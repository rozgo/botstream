# WebRTC Distributed Agents

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

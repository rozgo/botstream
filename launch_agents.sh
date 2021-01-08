#!/usr/bin/env bash

cargo build && \
for i in {9000..9000}; do ./target/debug/botstream --agent-id $i --peer-id 1000 & done

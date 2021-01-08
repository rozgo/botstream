#!/usr/bin/env bash

pushd servers
http-server -p4443 --ssl . &
python3 signalling_server.py
popd

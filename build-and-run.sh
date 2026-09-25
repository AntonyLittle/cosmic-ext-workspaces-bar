#!/bin/bash

killall cosmic-ext-workspaces-bar

cargo build --release

nohup ./target/release/cosmic-ext-workspaces-bar &
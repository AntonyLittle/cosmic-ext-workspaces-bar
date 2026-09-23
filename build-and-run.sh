#!/bin/bash

killall cosmic-ext-workspaces-bar

cargo build --release

./target/release/cosmic-ext-workspaces-bar &
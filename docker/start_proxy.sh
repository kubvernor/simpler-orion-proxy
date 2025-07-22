#!/bin/bash
#./replace_control_plane.sh
#./orion --config orion-config.yaml

echo "$@"
#echo "Config file "
export RUST_BACKTRACE=1
#more /orion-config/orion-bootstrap.yaml
./orion --config /orion-config/orion-bootstrap.yaml

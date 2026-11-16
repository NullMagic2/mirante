#!/bin/sh
# Module: mirante.clean_build
# Purpose: Removes all build output, then runs a full build.
# Created: 2026-11-16
# Architecture: Wraps build.sh; see it for requirements.
set -eu
cd "$(dirname "$0")"
rm -rf target dist
exec ./build.sh

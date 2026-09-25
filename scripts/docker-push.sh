#!/usr/bin/env bash
# Build the backend images and push them to the private registry.
#
# Config via env (override any):
#   REGISTRY   registry host/namespace            (default: registry.noddeboe.dk)
#   TAG        image tag                          (required, e.g. v0.15.1-forums.1)
#   APPS       space separated apps to build      (default: api events crond pushd voice-ingress)
#   PUSH       "1" push, "0" build only           (default: 1)
#
# Images are named stoat-<app>, e.g. registry.noddeboe.dk/stoat-api:v0.15.1-forums.1
#
# Usage:
#   TAG=v0.15.1-forums.1 ./scripts/docker-push.sh
#   TAG=dev PUSH=0 APPS="api events" ./scripts/docker-push.sh
set -euo pipefail

cd "$(dirname "$0")/.."

REGISTRY="${REGISTRY:-registry.noddeboe.dk}"
TAG="${TAG:?set TAG, e.g. TAG=v0.15.1-forums.1}"
APPS="${APPS:-api events crond pushd voice-ingress}"
PUSH="${PUSH:-1}"

BASE="stoat-base-local:${TAG}"

# Compile every binary once for the host architecture (amd64)
echo ">> building ${BASE}"
docker build -t "$BASE" -f Dockerfile.useCurrentArch .

for app in $APPS; do
  case "$app" in
    api)           dockerfile=crates/delta/Dockerfile ;;
    events)        dockerfile=crates/bonfire/Dockerfile ;;
    crond)         dockerfile=crates/daemons/crond/Dockerfile ;;
    pushd)         dockerfile=crates/daemons/pushd/Dockerfile ;;
    voice-ingress) dockerfile=crates/daemons/voice-ingress/Dockerfile ;;
    file-server)   dockerfile=crates/services/autumn/Dockerfile ;;
    proxy)         dockerfile=crates/services/january/Dockerfile ;;
    gifbox)        dockerfile=crates/services/gifbox/Dockerfile ;;
    *) echo "unknown app: $app"; exit 1 ;;
  esac

  ref="${REGISTRY%/}/stoat-${app}:${TAG}"
  echo ">> building ${ref}"
  # The app Dockerfiles copy from the upstream base image, use ours instead
  sed "s#ghcr.io/stoatchat/base:latest#${BASE}#" "$dockerfile" |
    docker build -t "$ref" -

  if [ "$PUSH" = "1" ]; then
    docker push "$ref"
  fi
done

echo ">> done: ${APPS} @ ${TAG}"

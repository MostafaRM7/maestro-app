#!/usr/bin/env bash

set -euo pipefail

if [[ "$(uname -s)" != "Linux" ]]; then
  echo "run-without-network.sh requires Linux network namespaces" >&2
  exit 64
fi

if [[ $# -eq 0 ]]; then
  echo "usage: scripts/run-without-network.sh <command> [arguments...]" >&2
  exit 64
fi

self_namespace="$(readlink /proc/self/ns/net)"
init_namespace="$(readlink /proc/1/ns/net)"
if [[ "$self_namespace" == "$init_namespace" ]]; then
  echo "refusing to run: process is not inside an isolated network namespace" >&2
  exit 65
fi

shopt -s nullglob
network_interfaces=(/sys/class/net/*)
if [[ ${#network_interfaces[@]} -ne 1 || "${network_interfaces[0]##*/}" != "lo" ]]; then
  echo "refusing to run: isolated namespace contains a non-loopback interface" >&2
  exit 65
fi

if [[ ! -r /proc/net/route ]]; then
  echo "refusing to run: isolated namespace route table is unavailable" >&2
  exit 65
fi

if awk 'NR > 1 && $2 == "00000000" { found = 1 } END { exit(found ? 0 : 1) }' /proc/net/route; then
  echo "refusing to run: isolated namespace still has a default IPv4 route" >&2
  exit 65
fi

echo "Network isolation verified (${self_namespace}); running: $1"
exec "$@"

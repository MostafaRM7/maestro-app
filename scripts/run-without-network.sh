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

# Current namespace identity. This must be readable: without it the script
# cannot prove it is inside a distinct network namespace.
if ! self_namespace="$(readlink /proc/self/ns/net 2>/dev/null)"; then
  echo "refusing to run: cannot read /proc/self/ns/net for the current network namespace" >&2
  exit 65
fi

# Parent/host namespace identity. The caller must capture it before entering
# the isolated namespace and pass it as MAESTRO_HOST_NET_NAMESPACE; this
# avoids depending on /proc/1/ns/net, which unprivileged processes cannot
# read on common systems (EACCES under the ptrace/yama restrictions).
# /proc/1/ns/net is used only as a fallback when it is readable. If no
# identity source is available the script fails closed: an empty comparison
# would otherwise falsely treat an unisolated process as isolated.
if [[ -n "${MAESTRO_HOST_NET_NAMESPACE:-}" ]]; then
  host_namespace="$MAESTRO_HOST_NET_NAMESPACE"
elif ! host_namespace="$(readlink /proc/1/ns/net 2>/dev/null)"; then
  echo "refusing to run: no host network namespace identity is available (MAESTRO_HOST_NET_NAMESPACE unset and /proc/1/ns/net unreadable)" >&2
  exit 65
fi

# A distinct namespace is the NET-002 requirement: only loopback plus a
# namespace identity different from the host proves isolation. The interface
# check below alone is not sufficient.
if [[ "$self_namespace" == "$host_namespace" ]]; then
  echo "refusing to run: process is not in a distinct network namespace (still in ${host_namespace})" >&2
  exit 65
fi

if [[ ! -r /proc/net/dev ]]; then
  echo "refusing to run: isolated namespace device table is unavailable" >&2
  exit 65
fi

# /proc/net/dev is per-network-namespace, unlike /sys/class/net, which stays
# bound to the network namespace of its sysfs mount and can list host
# interfaces from inside a fresh namespace. A fresh network namespace must
# expose exactly one interface: loopback. This proves the interface set only;
# namespace distinctness is proven by the namespace-identity check above.
if ! interfaces="$(awk 'NR > 2 { print $1 }' /proc/net/dev | sed 's/:$//')"; then
  echo "refusing to run: could not read the namespace interface table" >&2
  exit 65
fi
if [[ "$interfaces" != "lo" ]]; then
  echo "refusing to run: isolated namespace contains non-loopback interfaces: ${interfaces//$'\n'/ }" >&2
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

echo "Network isolation verified (host ${host_namespace} -> isolated ${self_namespace}); running: $1"
exec "$@"

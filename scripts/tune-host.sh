#!/usr/bin/env bash
# Optional kernel-level tuning for hosts running bifrost-server or
# bifrost-client. Run once on each host (server + client). Settings are
# RUNTIME ONLY — they reset on reboot. Re-run after each boot, or add
# the same writes to /etc/rc.local / a systemd-tmpfiles drop-in / a
# udev rule, depending on your distro.
#
# What it does and why:
#
# * RPS (Receive Packet Steering) — distributes the work of processing
#   incoming packets across CPUs in software, even on a NIC that only
#   has one hardware RX queue. Bifrost on a USB-Ethernet adapter or a
#   single-queue embedded NIC is bottlenecked by NET_RX softirq pinned
#   to one core; RPS spreads that work across the others.
#
# * XPS (Transmit Packet Steering) — symmetric, for the TX side.
#
# * RFS (Receive Flow Steering) — keeps packets of one flow on the
#   same CPU as the userspace socket reading them, for cache locality.
#
# Concrete win: ~+25 % single-stream upload through bifrost on a
# Cortex-A55 4-core ARM box with a single-queue gigabit NIC
# (361 Mbps → 455 Mbps in the LAN testbed).
#
# Usage:
#   sudo scripts/tune-host.sh            # auto-detect default-route NIC
#   sudo scripts/tune-host.sh eth0       # specify NIC explicitly

set -euo pipefail

NIC=${1:-$(ip route show default | awk 'NR==1{print $5}')}
if [[ -z "$NIC" || ! -d /sys/class/net/$NIC ]]; then
    echo "no NIC found (looked for default-route device)" >&2
    exit 1
fi

# All-CPUs mask, e.g. "ff" on 8-core, "f" on 4-core.
NCPU=$(nproc)
MASK=$(printf '%x' $(( (1 << NCPU) - 1 )))

echo "==> tuning $NIC ($NCPU cores, mask=$MASK)"

# RPS — every RX queue can dispatch to any CPU.
for q in /sys/class/net/$NIC/queues/rx-*; do
    [[ -e $q/rps_cpus ]] && echo $MASK > $q/rps_cpus
done

# RFS — flow cache size. Power of two; ~4096 entries per RX queue is a
# reasonable default for a single-flow heavy workload like bifrost.
echo 32768 > /proc/sys/net/core/rps_sock_flow_entries
for q in /sys/class/net/$NIC/queues/rx-*; do
    [[ -e $q/rps_flow_cnt ]] && echo 4096 > $q/rps_flow_cnt
done

# XPS — symmetric for TX. Some drivers don't expose tx-N/xps_cpus;
# silently skip those.
for q in /sys/class/net/$NIC/queues/tx-*; do
    [[ -e $q/xps_cpus ]] && echo $MASK > $q/xps_cpus
done

echo "==> done. Verify with:"
echo "     cat /sys/class/net/$NIC/queues/rx-0/rps_cpus    # expect $MASK"
echo "     cat /sys/class/net/$NIC/queues/tx-0/xps_cpus    # expect $MASK"
echo "     cat /proc/softirqs | grep -E 'CPU|NET_(TX|RX)'  # should distribute across CPUs under load"

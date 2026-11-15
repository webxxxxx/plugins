#!/bin/sh /etc/rc.common
START=99
STOP=10
USE_PROCD=1

PORT=8443

start_service() {
    procd_open_instance
    procd_set_param command /plugin/msa/run.sh
    procd_set_param respawn 3600 10 0
    procd_set_param stdout 1
    procd_set_param stderr 1
    procd_set_param limits nofile="4096 4096"
    procd_set_param env GOMAXPROCS=1
    procd_close_instance
    iptables -I INPUT -p udp --dport $PORT -j ACCEPT 2>/dev/null
    ip6tables -I INPUT -p udp --dport $PORT -j ACCEPT 2>/dev/null
}

stop_service() {
    killall mini-sb-agent 2>/dev/null
    iptables -D INPUT -p udp --dport $PORT -j ACCEPT 2>/dev/null
    ip6tables -D INPUT -p udp --dport $PORT -j ACCEPT 2>/dev/null
    rm -f /tmp/mini-sb-agent
}

#!/bin/sh /etc/rc.common

START=99
STOP=10
USE_PROCD=1

PROG=/plugin/meow/meow
CONFIG=/plugin/meow/config.yaml
DIR=/plugin/meow

start_service() {
    /plugin/meow/fix-rules.sh
    procd_open_instance
    procd_set_param command "$PROG" -d "$DIR" -f "$CONFIG"
    procd_set_param respawn 3600 10 0
    procd_set_param stdout 1
    procd_set_param stderr 1
    procd_set_param limits nofile="4096 4096"
    procd_set_param env TOKIO_WORKER_THREADS=1
    procd_close_instance
}

stop_service() {
    :
}

restart() {
    stop
    sleep 2
    start
}

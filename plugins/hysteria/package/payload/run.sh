#!/bin/sh
cd /plugin/msa
export GOMAXPROCS=1
if [ ! -f /tmp/mini-sb-agent ]; then
    zstd -d ./mini-sb-agent.zst -o /tmp/mini-sb-agent
    chmod +x /tmp/mini-sb-agent
fi
/tmp/mini-sb-agent run -c config.json > /dev/null 2>&1

#!/bin/sh
CONFIG=/plugin/meow/config.yaml

if grep -q "SUB-RULE,cn-direct" "$CONFIG"; then
    exit 0
fi

sed -i '/^rules:/a\- SUB-RULE,cn-direct' "$CONFIG"

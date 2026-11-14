#!/system/bin/sh
DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
cd "$DIR"
su -c "\"$DIR/theraohe\" --show"

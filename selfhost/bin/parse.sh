#!/bin/sh
set -eu
if [ "$#" -lt 1 ]; then
  echo "Uso: selfhost/bin/parse.sh <arquivo.nova> [binario-parser]" >&2
  exit 2
fi
source_file=$1
binary=${2:-./selfhost/target/nova-selfhost}
NOVA_SOURCE="$source_file" exec "$binary"

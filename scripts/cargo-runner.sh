#!/usr/bin/env bash
#
# Cargo `runner` for the ESP32-C6 target. Cargo invokes it as
#
#   cargo-runner.sh <path-to-the-linked-elf>
#
# and it flashes that image over serial.
#
# Three of the flags below are load-bearing and none of them is espflash's
# default:
#
#   --partition-table  otherwise espflash writes its own single-app table, with
#                      no second slot and nowhere for an over-the-air update
#                      to go.
#   --bootloader       otherwise espflash writes its own bootloader, built
#                      without rollback. Rollback is a bootloader state machine,
#                      so it would silently do nothing.
#   --erase-parts      after an over-the-air update the boot pointer may name
#                      ota_1, but a serial flash writes ota_0, and the board
#                      would come back up on the image just replaced.
#
# Settings, from the environment:
#
#   ESPFLASH         the espflash to run (default: espflash). Under WSL, a
#                    Windows build such as espflash.exe works and is often the
#                    only thing that can reach the port; paths are translated.
#   ESPFLASH_PORT    the serial port, e.g. /dev/ttyUSB0 or COM10.
#   ESPFLASH_BEFORE  how to get the chip into its bootloader (default:
#                    default-reset). Use no-reset when the board has been put
#                    into download mode by hand.

set -euo pipefail

here="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
root="$(dirname -- "$here")"

elf="${1:?internal error: cargo did not pass a binary path}"
shift
out="$(dirname -- "$elf")"

espflash="${ESPFLASH:-espflash}"
command -v "$espflash" >/dev/null 2>&1 ||
    { echo "error: $espflash not found (cargo install espflash --locked)" >&2; exit 1; }

# A Windows espflash reached through WSL wants Windows paths.
path() {
    case "$espflash" in
    *.exe) wslpath -w "$1" ;;
    *) printf '%s' "$1" ;;
    esac
}

[ -f "$out/bootloader.bin" ] ||
    { echo "error: no bootloader.bin next to $elf; was this built for the esp32c6?" >&2; exit 1; }

args=(flash
    --chip esp32c6
    --flash-size 4mb
    --partition-table "$(path "$root/partitions.csv")"
    --bootloader "$(path "$out/bootloader.bin")"
    --erase-parts otadata
    --before "${ESPFLASH_BEFORE:-default-reset}")
[ -z "${ESPFLASH_PORT:-}" ] || args+=(--port "$ESPFLASH_PORT")

exec "$espflash" "${args[@]}" "$@" "$(path "$elf")"

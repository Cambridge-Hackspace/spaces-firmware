#!/usr/bin/env bash
#
# Cargo `runner` for the ESP32-C6 target. Cargo invokes it as
#
#   cargo-runner.sh <path-to-the-linked-elf> [whatever followed `--`]
#
# and dispatches on what followed:
#
#   cargo run --release                    flash over USB or serial
#   cargo run --release -- 192.168.1.50    push over the network, through
#                                          esp-ota-push's push.sh (see
#                                          push.sh --help; anything after the
#                                          host goes to it)
#   cargo run --release -- ota             push to $ESP_OTA_HOST or .ota-host
#
# For the USB route, anything starting with a dash goes on to espflash.
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
#   ESPFLASH_PORT    the serial port, e.g. /dev/ttyACM0 or COM13 for the
#                    board's own USB, /dev/ttyUSB0 or COM10 for an adapter.
#   ESPFLASH_BEFORE  how to get the chip into its bootloader (default:
#                    default-reset). Use usb-reset over the board's own USB,
#                    and no-reset when it has been put into download mode by
#                    hand.

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

# ------------------------------------------------------- over the network
# `cargo run --release -- 192.168.1.50` (or `-- ota` for the remembered host)
# pushes to a running module instead of flashing over USB. Anything starting
# with a dash still goes to espflash, as before.
if [ "$#" -gt 0 ] && [ "${1#-}" = "$1" ]; then
    # push.sh ships inside the esp-ota-push crate, so it is always the version
    # that matches the firmware's own copy of the library. Cargo knows where
    # that is, whether it came from git or a local path.
    lib="$(cargo metadata --format-version 1 --manifest-path "$root/Cargo.toml" 2>/dev/null |
        grep -o '"manifest_path":"[^"]*/esp-ota-push/Cargo.toml"' | head -n 1 |
        sed 's/^"manifest_path":"//; s/Cargo.toml"$//')"
    push="${lib}scripts/push.sh"
    [ -n "$lib" ] && [ -f "$push" ] ||
        { echo "error: cannot find esp-ota-push's push.sh through cargo metadata" >&2; exit 1; }

    # Converted here rather than by push.sh, because only this script knows
    # espflash may be a Windows build that wants Windows paths.
    image="$elf.ota.bin"
    "$espflash" save-image --chip esp32c6 --flash-size 4mb "$(path "$elf")" "$(path "$image")" \
        >/dev/null || { echo "error: espflash save-image failed" >&2; exit 2; }

    version="$(sed -n 's/^version[[:space:]]*=[[:space:]]*"\(.*\)"/\1/p' "$root/Cargo.toml" | head -n 1)"
    exec bash "$push" --image "$image" \
        --partitions "$root/partitions.csv" \
        --expect-name "$(basename -- "$elf")" \
        --new-version "$version" \
        "$@"
fi

# ------------------------------------------------------------- over USB
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

# spaces-firmware

Firmware for devices that take part in [Cooperative Systems: Spaces][css], the
access-control platform at the [Cambridge Hackspace][hackspace].

It is two things at once:

- **A library.** `spaces-device` is the module protocol with no hardware in it:
  the rules every module has to get exactly right, such as energizing only on an
  explicit yes and turning off by itself when the coordinator goes quiet. It is
  tested on an ordinary computer. `spaces-device-espidf` runs it on an ESP32.
- **A demo**, in `examples/buttons`: an ESP32-C6 with buttons standing in for
  people swiping cards and LEDs standing in for a machine, built as a starting
  point for writing a new module.

The protocol itself is described in the platform's `FIRMWARE.md`.

> Work in progress. The demo runs the protocol end to end against a stand-in
> edge, and takes firmware updates over the network; a real edge, and notes on
> what the protocol document leaves open, are next.

## Layout

| Path | What it is |
|---|---|
| `spaces-device/` | the protocol core: no hardware, no I/O, host-tested |
| `spaces-device-espidf/` | running it on ESP-IDF |
| `examples/buttons/` | the demo firmware |
| `partitions.csv` | two app slots, for updates over the air |
| `scripts/cargo-runner.sh` | what `cargo run` uses to flash, or to push over the network |

## Building

Needs Rust (the pinned nightly in `rust-toolchain.toml` is installed
automatically), `cargo install ldproxy --locked`, and
`cargo install espflash --locked`. The first build downloads ESP-IDF into
`.embuild/` and takes a while.

```sh
cargo build --release
cargo run --release      # build and flash
cargo test -p spaces-device --target x86_64-unknown-linux-gnu
```

Flashing is configured from the environment; see the top of
`scripts/cargo-runner.sh`. For example, over the board's own USB port, from WSL
through a Windows espflash:

```sh
ESPFLASH=/path/to/espflash.exe ESPFLASH_PORT=COM13 ESPFLASH_BEFORE=usb-reset \
  cargo run --release
```

### Updating over the network

Once a module is running, new firmware can be pushed to it, through
[esp-ota-push]:

```sh
cargo run --release -- 192.168.1.50
```

That builds, converts, uploads, and waits for the verdict. The new image is on
probation until it gets back to what the old one had when the update arrived
(on the network, on the edge's broker, or with the setup access point up); if
it cannot within three minutes, the board goes back to the old image by itself,
and the push reports it. While an update is written, the status light blinks
cyan.

Updates are refused until a **firmware update password** has been set on the
setup page, and while a tool session is open. The username is `admin`; the
password is read from `~/.netrc` (or the file in `ESP_OTA_NETRC`), or asked
for, and never goes on a command line:

```
machine 192.168.1.50 login admin password <the password>
```

A browser works too: `http://<the module>/update`.

[esp-ota-push]: https://forge.axonibyte.com/axonibyte/esp-ota-push

### The ESP32-C6 Super Mini

The demo is built on this board, and flashing it the first time may not be
straightforward: it arrives blank, its own USB did not answer us until it had
firmware on it, and the fallback, its serial pins, has its own traps.
**Read [docs/SUPER-MINI.md](docs/SUPER-MINI.md) before you start.** It is the
condensed version of the time spent finding that out, including two wrong
conclusions, and every problem in it looked like a different problem at first.

Once firmware with over-the-air updates is on the board, none of this is needed
again.

## License

[AGPL-3.0-or-later](LICENSE), matching the platform.

[css]: https://github.com/Cambridge-Hackspace/cooperative-systems-spaces
[hackspace]: https://cambridgehackspace.com

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

> Work in progress. What is here so far boots on the board and proves the
> flashing setup; the protocol is next.

## Layout

| Path | What it is |
|---|---|
| `spaces-device/` | the protocol core: no hardware, no I/O, host-tested |
| `spaces-device-espidf/` | running it on ESP-IDF |
| `examples/buttons/` | the demo firmware |
| `partitions.csv` | two app slots, for updates over the air |
| `scripts/cargo-runner.sh` | what `cargo run` uses to flash |

## Building

Needs Rust (the pinned nightly in `rust-toolchain.toml` is installed
automatically), `cargo install ldproxy --locked`, and
`cargo install espflash --locked`. The first build downloads ESP-IDF into
`.embuild/` and takes a while.

```sh
cargo build --release
cargo run --release      # build and flash over serial
cargo test -p spaces-device --target x86_64-unknown-linux-gnu
```

Flashing is configured from the environment; see the top of
`scripts/cargo-runner.sh`. For example, from WSL through a Windows espflash, to
a board already put into download mode by hand:

```sh
ESPFLASH=/path/to/espflash.exe ESPFLASH_PORT=COM10 ESPFLASH_BEFORE=no-reset \
  cargo run --release
```

### The ESP32-C6 Super Mini

The demo is built on this board, and flashing it the first time is not
straightforward: it arrives blank, its own USB download mode did not work for
us, and it has to be flashed over its serial pins while powered from a charger.
**Read [docs/SUPER-MINI.md](docs/SUPER-MINI.md) before you start.** It is the
condensed version of an evening spent finding that out, and every problem in it
looked like a different problem at first.

Once firmware with over-the-air updates is on the board, none of this is needed
again.

## License

[AGPL-3.0-or-later](LICENSE), matching the platform.

[css]: https://github.com/Cambridge-Hackspace/cooperative-systems-spaces
[hackspace]: https://cambridgehackspace.com

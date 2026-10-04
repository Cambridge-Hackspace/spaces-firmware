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
> edge, and takes firmware updates over the network; a real edge is next. What
> the protocol document gets wrong or leaves open, found while writing this, is
> in [docs/FINDINGS.md](docs/FINDINGS.md).

## Layout

| Path | What it is |
|---|---|
| `spaces-device/` | the protocol core: no hardware, no I/O, host-tested |
| `spaces-device-espidf/` | running it on ESP-IDF |
| `examples/buttons/` | the demo firmware |
| `tools/fake-edge/` | a stand-in edge, for testing a module and breaking things on cue |
| `docs/FINDINGS.md` | what FIRMWARE.md did not say |
| `docs/SUPER-MINI.md` | getting firmware onto an ESP32-C6 Super Mini |
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

## Running the demo

A script for showing the protocol to someone, with each step's point.

### The board

An ESP32-C6 Super Mini on a breadboard, and:

| Qty | Part | For |
|---|---|---|
| 3 | momentary push buttons (6×6 mm tactile) | Alice, Bob, Tool off |
| 1 | on/off switch (a breadboard slide switch) | Running |
| 2 | LEDs, two colours (say amber and green) | Tool, Running |
| 2 | 330 Ω resistors (100 Ω for blue or white LEDs) | one per LED |
| | jumper wires | |

| GPIO | Connect | Meaning |
|---|---|---|
| 18 | button to GND | Alice presents her card |
| 19 | button to GND | Bob presents his |
| 20 | button to GND | Tool off |
| 21 | switch to GND | the machine is running (a laser firing, say) |
| 22 | resistor, then LED, to GND | **Tool**: powered, authorized *and* leased |
| 23 | resistor, then LED, to GND | **Running**: powered *and* the switch is on |

The inputs use the chip's own pull-ups, so the buttons need no resistors. The
board's RGB LED is the status light:

| Light | Means |
|---|---|
| blue, pulsing | setup mode: join its access point |
| amber, blinking | joining Wi-Fi, or reaching the edge's broker |
| green flash every 3 s | all well |
| amber, steady | lost the edge's broker (so the tool is off) |
| cyan, blinking fast | taking new firmware: leave it powered |
| red | failed; restarting |

### Setting up

1. Flash it once over USB (see [Building](#building)); later updates go over
   the network.
2. With nothing configured it starts in setup mode. Join its access point,
   `spaces-setup-…`, open `http://192.168.71.1/`, and fill in the Wi-Fi, the
   Spaces server, a device invite from `/admin/devices` (paste it), the edge's
   broker and this module's login on it, the tool, a firmware update
   password, and two cards: one authorized on the tool for Alice, one that
   should be refused for Bob. Hold BOOT for three seconds to come back here.
3. An administrator binds the device to the tool in the **`power`** role. Any
   other role can start a session, but only a `power` module is leased, so
   nothing would keep the tool on.
4. Run an edge. For the demo, the stand-in in `tools/fake-edge` will do:

   ```sh
   pip install -r tools/fake-edge/requirements.txt
   python3 tools/fake-edge/fake_edge.py --broker <the broker> \
       --username edge --password-file <file> --allow <Alice's card>
   ```

   It answers like the real edge, leases every second for three, and takes
   commands typed into it to break things on purpose (listed at the top of the
   file).

Keep the module's serial log open (`espflash monitor` over its USB): it
narrates every message in and out.

### The script

1. **Alice presses.** The module asks the edge, which says yes, and starts
   leasing. Tool LED on. *A yes alone does not energize: it needs the yes and
   a lease.*
2. **Bob presses.** Refused, "Unknown card". The Tool LED never flickers.
   *Only an explicit yes counts; anything else, including silence, is no.*
3. **Flip Running on and off a few times, then press Tool off.** The module
   reports `tool-log` with the running time, then `tool-off`. *A metered tool
   bills for the time the machine worked, not the time the session was open.*
4. **Alice again, then type `pause` into the fake edge.** No message is sent;
   the leases just stop. Within three seconds the Tool LED goes out, and the
   module reports the session over. *Silence is the instruction to stop. This
   is what happens when the edge dies or the network goes.* Type `resume`:
   the LED stays off. *A lapsed session does not come back by itself; it
   needs a new swipe.*
5. **Alice again, then `revoke`.** The edge withdraws the lease outright, and
   the LED goes out at once. *A courtesy: the mechanism is still the timer.*
6. **`ok`, then Alice.** The edge answers `{"status": "ok"}` and nothing else.
   Tool stays off. *"OK" is not "yes".* (`ignore` and `late` show a reply that
   never comes, and one that comes after the module has given up.)
7. **Stop the broker,** or unplug the edge's network. Status light: steady
   amber, and the tool, if it was on, is off within three seconds. Start it
   again: green flashes, no restart needed.
8. **With Alice's session open, push new firmware** (`cargo run --release --
   <address>`). Refused: not while the tool is in use. Press Tool off and
   push again: the light blinks cyan, the module restarts into the new
   firmware, and keeps it once it is back on the broker. *An update that cannot
   get back to the broker is undone by itself, three minutes later.*

One module per broker, for now: replies to `tool-on` do not say whose they
are, so two modules on one broker can take each other's answers. See
[docs/FINDINGS.md](docs/FINDINGS.md), finding 7.

## License

[AGPL-3.0-or-later](LICENSE), matching the platform.

[css]: https://github.com/Cambridge-Hackspace/cooperative-systems-spaces
[hackspace]: https://cambridgehackspace.com

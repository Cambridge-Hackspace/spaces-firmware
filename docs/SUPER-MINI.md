# Flashing an ESP32-C6 Super Mini

The demo is built on an ESP32-C6 "Super Mini", a cheap thumb-sized board with a
single USB-C port. Getting firmware onto one took us most of an evening and a
morning, and none of the problems were where they first appeared to be. This is
that time, condensed, so it does not have to happen twice. Two things we
believed for a while turned out to be wrong; they are marked as such below,
because they are the conclusions you are most likely to jump to as well.

Once firmware with over-the-air updates is on the board, none of this is needed
again: later updates go over Wi-Fi.

## In short

1. **Try the board's own USB first**, with espflash resetting the chip into
   download mode itself (`ESPFLASH_BEFORE=usb-reset`). Once the board had any
   firmware on it, even a half-written image, this worked every time and took
   seven seconds.
2. It **did not work while the board was blank**. If it will not answer, flash
   it over its **serial pins** through a 3.3 V USB-to-serial adapter; an ESP32
   DevKit can be that adapter.
3. On the serial route, **BOOT held while tapping RESET** picks USB or serial
   download apparently at random. Repeat until the boot report says
   `UART0_BOOT`; Espressif says a pull-up on GPIO 8 should settle it.
4. **Keep a serial monitor open** throughout. The boot report answers in one
   line what everything else makes you guess at.

## What a blank board looks like

Plugged in, the board blinks, and its USB serial device appears and disappears
roughly every three seconds. From WSL it looks like `usbipd attach` working for
a moment and then dropping; from Windows, like a COM port that will not stay.

That is not a fault. With nothing in its flash, the chip's built-in bootloader
looks for a program, finds `0xffffffff`, and its watchdog restarts it:

```
ESP-ROM:esp32c6-20220919
rst:0x1 (POWERON),boot:0x8 (SPI_FAST_FLASH_BOOT)
invalid header: 0xffffffff
invalid header: 0xffffffff
...
rst:0x7 (TG0_WDT_HPSYS),boot:0x8 (SPI_FAST_FLASH_BOOT)
```

The USB port is part of the chip, and a watchdog restart takes it off the bus.

## Flash over its own USB

Plug the C6 into the computer. It appears as a serial port: "USB JTAG/serial
debug unit" on Windows, `/dev/ttyACM0` or similar on Linux. Then:

```sh
ESPFLASH_PORT=/dev/ttyACM0 ESPFLASH_BEFORE=usb-reset cargo run --release
```

`usb-reset` has espflash restart the chip into download mode through the USB
port itself, so no buttons are involved, and the board starts the new firmware
by itself afterwards.

**What worked and what did not.** On our board this route failed for an
evening, then worked first time the next morning and every time since. The
difference we can point to is the board's state: the evening it was blank, and
we were putting it into download mode with the BOOT button; the morning it had
a half-written image on it and espflash did the reset. A board that is merely
crashing in a loop restarts its processor but not, apparently, its USB port,
which stays up for espflash to use; a blank board's watchdog restarts the whole
chip. We have not narrowed it down further than that. Try this route first
regardless: it costs a minute.

When it failed, the port *enumerated* but nothing got through. The very first
write timed out (`The semaphore timeout period has expired` on Windows, a
stream of `urb->status -104` in the Linux kernel log under WSL). The USB
hardware on a C6 keeps answering the host even when nothing on the chip is
reading, so a port that appears is not a chip that is listening.

> **Wrong turn: "its USB download mode does not work."** We concluded this
> after the evening, and wrote it here. It was wrong, or at least far too
> broad.

## Or over the serial pins

The C6's UART0 is on **GPIO 16 (TX)** and **GPIO 17 (RX)**. The bootloader prints
its startup report there on every boot, and in download mode it can listen
there.

You need a **3.3 V TTL** USB-to-serial adapter: a small board with pin headers,
often labelled CP2102, CH340 or FT232.

> **Not an adapter with a DB9 plug.** Those speak RS-232, which swings to about
> ±12 V, and would very likely destroy the chip.

### An ESP32 DevKit makes a good adapter

If you have a classic ESP32 DevKit, its on-board USB-to-serial chip will do,
provided its own ESP32 is held in reset so it stays out of the way:

| DevKit pin | C6 Super Mini pin | Why |
|---|---|---|
| **EN** | DevKit **GND** | holds the DevKit's own ESP32 in reset |
| **RX0** | **RX** (GPIO 17) | the DevKit's serial chip transmits on this pin |
| **TX0** | **TX** (GPIO 16) | the DevKit's serial chip listens on this pin |
| **GND** | **GND** | common ground |

RX to RX and TX to TX looks wrong and is right: the DevKit's labels are named
from its own ESP32's point of view, and that chip is being bypassed.

Rules:

- **Ground EN before the DevKit is powered.** Otherwise whatever firmware is on
  it runs, drives its TX0 pin, and drowns out the C6 on that wire. With EN low,
  the firmware on the DevKit never runs and does not matter.
- **Power the C6 one way only.** Either from its own USB socket (a charger is
  fine), or from the DevKit with a wire from **DevKit 3V3 to C6 3V3**, but
  never both at once. Not DevKit 5V to C6 3V3: that is 5 V on a 3.6 V supply.
  DevKit 3V3 to the C6's 5V pin does no harm but starves the C6's regulator.
  The Super Mini's power LED runs from the USB side, so it stays dark when the
  board is powered through 3V3, even while the chip is running.
- **Mind the cable.** A breadboard holds jumpers loosely, and the weight of a
  USB cable is enough to pull one out. Tape the cable down. When the serial
  port suddenly goes silent, check the wires before anything else.

### If the adapter hears nothing

Split the problem in half. Take the two signal wires off the adapter's TX and
RX pins, join those two pins with one jumper, and type into a terminal on the
adapter's port: what you type should come back. If it does, the adapter is
fine and the fault is on the C6 side, most often a loose wire or a board with
no power. If it does not, the adapter is the problem.

## Getting it into serial download mode

**Hold BOOT, tap RESET, keep holding BOOT for a second, then release.** Then
read the boot report. In download mode the C6 waits on either USB or its
serial pins, and says which:

```
rst:0x1 (POWERON),boot:0x0 (USB_BOOT)
wait usb download
```

```
rst:0x1 (POWERON),boot:0x72 (UART0_BOOT)
wait uart0 download
```

Only the second is any use on this route. On our board the same button press
gave one or the other with no pattern we could find: the same choice came out
both ways on a charger, on the computer, and on the DevKit's 3V3. The number in
`boot:0x..` is the strapping pins as the chip read them, one bit per pin, and
even on ordinary boots from flash it wandered (`0x8`, `0xa`, `0xc`, `0x18`,
`0x29`), which points at a pin nobody is driving.

Espressif's documentation names the pin. It says download mode needs GPIO 9
low **and GPIO 8 high**: "The strapping combination of GPIO8 = 0 and GPIO9 = 0
is invalid and will trigger unexpected behavior," and GPIO 8 "needs to be
pulled up (default is float)". `boot:0x0` is every strapping pin low, GPIO 8
included. On the Super Mini, GPIO 8 also carries the data line of the RGB LED.

So: **a resistor of 1 to 10 kΩ from GPIO 8 to 3V3** should make the choice
reliable. It will probably light the RGB LED at full brightness while fitted,
which is harmless. Without it, just repeat the buttons until the report says
`UART0_BOOT`.

> **Wrong turn: "the charger decides it."** For a while it looked as if the C6
> chose USB whenever it could see a computer, and the serial pins on a charger.
> It then chose USB with no USB connected at all.
>
> **Wrong turn: "GPIO 8 is not it."** We fitted a pull-up once, saw the LED
> light, and gave up on the idea. That one attempt never reached download mode
> at all, so it showed nothing either way. We have not since tried it properly,
> because the USB route started working; if you are on the serial route, try
> it.

Unplugging the C6 while holding BOOT ought to work too, but with the adapter
wired up it is unreliable: the adapter holds the C6's RX line at 3.3 V when
idle, and enough leaks through it to keep the chip from powering down fully.
The RESET button always gives a clean start.

## Watching it

Keep a terminal open on the adapter's serial port at 115200 baud while you
press the buttons. Every boot prints its report there, which settles in one line
whether the board is blank, in download mode, on which interface, or running
your firmware. Without it, everything above was guesswork.

An adapter's serial port stays put while the C6 restarts. The C6's own USB port
does not, and a terminal open on it is silently left talking to nothing after
a full reset.

Over the C6's own USB, `espflash monitor` works, but only in its default mode,
which resets the board and then reads. With `--no-reset` it still connects to
the chip's bootloader first, which stops the running firmware.

## Then flash over serial

With the board in download mode on the serial pins:

```sh
ESPFLASH=espflash ESPFLASH_PORT=/dev/ttyUSB0 ESPFLASH_BEFORE=no-reset \
  cargo run --release
```

`no-reset` because the adapter's reset lines are not connected to the C6; you
have already put it into download mode by hand. Afterwards, tap RESET without
BOOT to start the firmware.

From WSL, the Windows build of espflash pointed at the adapter's COM port is the
reliable route (`ESPFLASH=/path/to/espflash.exe ESPFLASH_PORT=COM10`); the
runner translates the paths. The same goes for the C6's own USB port.

A successful first boot looks like this:

```
I (88) boot: No factory image, trying OTA 0
I (171) boot: Loaded app from partition at offset 0x20000
I (486) buttons: spaces-firmware buttons demo 0.1.0, running from ota_0
```

## References

- [esptool: Boot Mode Selection, ESP32-C6](https://docs.espressif.com/projects/esptool/en/latest/esp32c6/advanced-topics/boot-mode-selection.html)
- [ESP32-C6 Hardware Design Guidelines: Download Guidelines](https://docs.espressif.com/projects/esp-hardware-design-guidelines/en/latest/esp32c6/download-guidelines.html)

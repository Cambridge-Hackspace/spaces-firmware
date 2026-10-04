# Flashing an ESP32-C6 Super Mini

The demo is built on an ESP32-C6 "Super Mini", a cheap thumb-sized board with a
single USB-C port. Getting the first firmware onto one took most of an evening,
and none of the problems were where they first appeared to be. This is that
evening, condensed, so it does not have to happen twice.

Once firmware with over-the-air updates is on the board, none of this is needed
again: later updates go over Wi-Fi.

## In short

1. The board arrives **blank**. A blinking light and a USB port that vanishes
   every three seconds are normal.
2. Its **download mode over its own USB port did not work** for us at all.
3. So flash it over its **serial pins**, through any 3.3 V USB-to-serial adapter.
   An ESP32 DevKit can be that adapter.
4. **Power the C6 from a charger, not the computer**, or it waits for USB and
   ignores the serial pins.
5. Enter download mode with **BOOT held while tapping RESET**, not by unplugging.

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

The USB port is part of the chip, so every restart drops it off the bus.

## Its USB download mode did not work

The usual way to flash a C6 is through that same USB port, in download mode. On
this board it never answered: not from Linux, not through WSL, and not from
Windows directly, with two different cables and two different ports.

The tell is that the port *enumerates* but nothing gets through. The very first
write times out (`The semaphore timeout period has expired` on Windows, a stream
of `urb->status -104` in the Linux kernel log under WSL). The USB hardware on a
C6 keeps answering the host even when nothing on the chip is reading, so a port
that appears is not a chip that is listening.

We did not get to the bottom of why. It does not matter, because the serial
pins work.

## Flash it over the serial pins instead

The C6's UART0 is on **GPIO 16 (TX)** and **GPIO 17 (RX)**. The bootloader prints
its startup report there on every boot, and in download mode it listens there.

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

Two rules:

- **Ground EN before the DevKit is powered.** Otherwise whatever firmware is on
  it runs, may drive its TX0 pin, and fights the C6 over that wire. With EN low,
  the firmware on the DevKit never runs and does not matter.
- **Join only the grounds.** Do not connect the two boards' 3V3 or 5V pins; each
  is powered from its own USB.

## Make it choose the serial pins: power it from a charger

In download mode the C6 can wait on USB or on its serial pins, and the boot
report says which:

```
rst:0x1 (POWERON),boot:0x0 (USB_BOOT)
wait usb download
```

```
rst:0x1 (POWERON),boot:0x72 (UART0_BOOT)
wait uart0 download
```

It chose USB every time it was plugged into the laptop, and the serial pins
every time it was plugged into a charger. As far as we can tell, if it can see a
USB host it waits for USB, whatever is connected to its serial pins. And since
its USB download does not work, that is a dead end.

So: **DevKit on the computer, C6 on a phone charger or power bank.**

We spent a while trying to steer this with a pull-up on GPIO 8 instead, on the
theory that it was a floating strapping pin. That theory was wrong, and GPIO 8
also drives the board's RGB LED, which a pull-up lights at full brightness.
Leave it alone.

## Enter download mode with the buttons, not by unplugging

**Hold BOOT, tap RESET, release BOOT.**

Unplugging the C6 while holding BOOT ought to do the same, but with the adapter
wired up it does not reliably: the adapter holds the C6's RX line at 3.3 V when
idle, enough current leaks through it to keep the chip from powering down
fully, and it then does not do a clean start. The RESET button always does.

In download mode the board's blue LED goes out, because nothing is running to
drive it. That is expected.

## Watching it

Keep a terminal open on the adapter's serial port at 115200 baud while you
press the buttons. Every boot prints its report there, which settles in one line
whether the board is blank, in download mode, on which interface, or running
your firmware. Without it, everything above was guesswork.

An adapter's serial port stays put while the C6 restarts. The C6's own USB port
does not, and a terminal open on it is silently left talking to nothing after
the first reset.

## Then flash

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
runner translates the paths.

A successful first boot looks like this:

```
I (88) boot: No factory image, trying OTA 0
I (171) boot: Loaded app from partition at offset 0x20000
I (307) buttons: spaces-firmware buttons demo 0.1.0
I (308) buttons: running from ota_0
```

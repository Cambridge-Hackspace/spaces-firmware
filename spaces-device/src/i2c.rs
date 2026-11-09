//! Finding what is fitted on an I²C bus, and knowing what it is (#19).
//!
//! A box increasingly hangs its parts off one I²C bus: the LCD's backpack, a
//! card reader converted to I²C, a joystick on an expander, a current monitor.
//! Two wires instead of a pin each. This finds them at boot, the same way on
//! every device, generically over any [`embedded_hal::i2c::I2c`] bus so the
//! rules are tested here with a fake one.
//!
//! **I²C cannot enumerate, so this finds what it is told to look for.** An
//! address answering says only that *something* is there, and addresses
//! collide: a PCF8574 LCD backpack and a PCF8574 used as an expander both live
//! at 0x20-0x27. So a device looks for each part it expects at the addresses
//! that part can have ([`find`]), and a part is reported either as
//! **confirmed**, when the chip has an identity register and it read right, or
//! **assumed**, when the chip has none and only answered at its address.
//!
//! **Probing is careful, because some parts act on any write.** A PCF8574's
//! port is whatever byte was last written to it; on an expander driving a
//! relay, a stray byte switches it. So:
//!
//! - an address is first probed with a zero-length write, which a device only
//!   acknowledges and cannot act on;
//! - an identity register is read (a one-byte register-pointer write, then a
//!   read) only at an address that answered, and only for a part that has one;
//! - addresses another part has already claimed are never touched (`avoid`).
//!
//! What this does **not** do is put anything that must fail safe on the bus.
//! The stop button, relays and strikes stay on their own GPIO: a held-low SDA
//! hangs the bus, a stop press would be lost, and an output behind an expander
//! would stay where it was.

use core::fmt;

use embedded_hal::i2c::I2c;

/// How a part shows that it is what it claims.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Check {
    /// No identity register: it answering at one of its addresses is all
    /// there is. Reported as [`Identity::Assumed`].
    Answers,
    /// An 8-bit register that reads as one of these values.
    Register { reg: u8, any_of: &'static [u8] },
    /// A 16-bit big-endian register that reads as this value.
    Register16 { reg: u8, value: u16 },
}

/// A kind of part a device may look for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Part {
    /// What it is called in the log and on the screen.
    pub name: &'static str,
    /// The addresses it can have, in the order to try them.
    pub addresses: &'static [u8],
    pub check: Check,
}

/// An NXP MFRC522 card reader converted to I²C. With `EA` high (as the common
/// boards are) its address comes from six pins, two of them on the header and
/// four usually floating, and converted boards are reported at 0x28 and 0x3C;
/// a device should look at the address its board file names. `VersionReg`
/// (0x37) reads 0x91 or 0x92.
pub const MFRC522: Part = Part {
    name: "MFRC522",
    addresses: &[0x28, 0x29, 0x2A, 0x2B, 0x2C, 0x2D, 0x2E, 0x2F, 0x3C],
    check: Check::Register {
        reg: 0x37,
        any_of: &[0x91, 0x92],
    },
};

/// A PCF8574 8-bit expander: an LCD backpack, or inputs and outputs. No
/// identity register, so only ever assumed.
pub const PCF8574: Part = Part {
    name: "PCF8574",
    addresses: &[0x20, 0x21, 0x22, 0x23, 0x24, 0x25, 0x26, 0x27],
    check: Check::Answers,
};

/// A PCF8574A: the same part at 0x38-0x3F.
pub const PCF8574A: Part = Part {
    name: "PCF8574A",
    addresses: &[0x38, 0x39, 0x3A, 0x3B, 0x3C, 0x3D, 0x3E, 0x3F],
    check: Check::Answers,
};

/// A TI INA226 current and power monitor: manufacturer ID (0xFE) reads 0x5449,
/// "TI".
pub const INA226: Part = Part {
    name: "INA226",
    addresses: &[
        0x40, 0x41, 0x42, 0x43, 0x44, 0x45, 0x46, 0x47, 0x48, 0x49, 0x4A, 0x4B, 0x4C, 0x4D, 0x4E,
        0x4F,
    ],
    check: Check::Register16 {
        reg: 0xFE,
        value: 0x5449,
    },
};

/// What a part turned out to be.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Identity {
    /// Its identity register read right.
    Confirmed,
    /// It has no identity register, and something answered where it would be.
    Assumed,
}

/// A part found on the bus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Found {
    pub part: &'static str,
    pub address: u8,
    pub identity: Identity,
}

impl fmt::Display for Found {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let how = match self.identity {
            Identity::Confirmed => "confirmed",
            Identity::Assumed => "assumed: it has no identity register",
        };
        write!(f, "{} at {:#04x} ({how})", self.part, self.address)
    }
}

/// Whether anything acknowledges `address`: a zero-length write, which a
/// device can acknowledge but cannot act on.
pub fn answers<B: I2c>(bus: &mut B, address: u8) -> bool {
    bus.write(address, &[]).is_ok()
}

/// Whether the part at `address` is `part`, by its check. Reads a register
/// only for a part that has one, and only by writing the register's number:
/// call it only at an address that has already [`answers`]-ed.
pub fn identify<B: I2c>(bus: &mut B, address: u8, part: &Part) -> Option<Identity> {
    match part.check {
        Check::Answers => Some(Identity::Assumed),
        Check::Register { reg, any_of } => {
            let mut buf = [0u8; 1];
            bus.write_read(address, &[reg], &mut buf).ok()?;
            any_of.contains(&buf[0]).then_some(Identity::Confirmed)
        }
        Check::Register16 { reg, value } => {
            let mut buf = [0u8; 2];
            bus.write_read(address, &[reg], &mut buf).ok()?;
            (u16::from_be_bytes(buf) == value).then_some(Identity::Confirmed)
        }
    }
}

/// The first of `part`'s addresses (or of `at`, when given: a board file
/// naming where this board puts it) that answers and identifies as `part`.
/// Addresses in `avoid` -- claimed by a part already found -- are not touched.
pub fn find<B: I2c>(bus: &mut B, part: &Part, at: Option<&[u8]>, avoid: &[u8]) -> Option<Found> {
    at.unwrap_or(part.addresses)
        .iter()
        .copied()
        .filter(|a| !avoid.contains(a))
        .find_map(|address| {
            if !answers(bus, address) {
                return None;
            }
            identify(bus, address, part).map(|identity| Found {
                part: part.name,
                address,
                identity,
            })
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use embedded_hal::i2c::{ErrorKind, ErrorType, NoAcknowledgeSource, Operation};
    use std::collections::BTreeMap;

    #[derive(Debug)]
    struct Nack;
    impl embedded_hal::i2c::Error for Nack {
        fn kind(&self) -> ErrorKind {
            ErrorKind::NoAcknowledge(NoAcknowledgeSource::Address)
        }
    }

    /// A bus with devices on it: address -> register -> bytes. Every write is
    /// recorded, so a test can say what was written where.
    #[derive(Default)]
    struct Fake {
        devices: BTreeMap<u8, BTreeMap<u8, Vec<u8>>>,
        writes: Vec<(u8, Vec<u8>)>,
    }

    impl Fake {
        fn with(mut self, address: u8, reg: u8, bytes: &[u8]) -> Self {
            self.devices
                .entry(address)
                .or_default()
                .insert(reg, bytes.to_vec());
            self
        }
        fn bare(mut self, address: u8) -> Self {
            self.devices.entry(address).or_default();
            self
        }
    }

    impl ErrorType for Fake {
        type Error = Nack;
    }

    impl I2c for Fake {
        fn transaction(&mut self, address: u8, ops: &mut [Operation<'_>]) -> Result<(), Nack> {
            let Some(device) = self.devices.get(&address) else {
                return Err(Nack);
            };
            let mut reg = None;
            for op in ops {
                match op {
                    Operation::Write(bytes) => {
                        self.writes.push((address, bytes.to_vec()));
                        reg = bytes.first().copied();
                    }
                    Operation::Read(buf) => {
                        let value = reg.and_then(|r| device.get(&r)).ok_or(Nack)?;
                        let n = buf.len().min(value.len());
                        buf[..n].copy_from_slice(&value[..n]);
                    }
                }
            }
            Ok(())
        }
    }

    #[test]
    fn a_reader_is_confirmed_by_its_version_register() {
        let mut bus = Fake::default().with(0x28, 0x37, &[0x92]);
        let found = find(&mut bus, &MFRC522, None, &[]).expect("found");
        assert_eq!(found.address, 0x28);
        assert_eq!(found.identity, Identity::Confirmed);
        assert_eq!(found.to_string(), "MFRC522 at 0x28 (confirmed)");
    }

    #[test]
    fn a_wrong_identity_is_not_the_part() {
        // Something answers at 0x28 and has a register 0x37, but not a
        // reader's version byte.
        let mut bus = Fake::default().with(0x28, 0x37, &[0x6E]);
        assert_eq!(find(&mut bus, &MFRC522, None, &[]), None);
    }

    #[test]
    fn nothing_there_is_nothing_found() {
        let mut bus = Fake::default();
        assert_eq!(find(&mut bus, &MFRC522, None, &[]), None);
        assert_eq!(find(&mut bus, &PCF8574, None, &[]), None);
    }

    #[test]
    fn an_expander_is_only_ever_assumed() {
        let mut bus = Fake::default().bare(0x27);
        let found = find(&mut bus, &PCF8574, None, &[]).expect("found");
        assert_eq!(found.address, 0x27);
        assert_eq!(found.identity, Identity::Assumed);
    }

    #[test]
    fn an_assumed_part_is_never_written_to() {
        // The probe is a zero-length write, which a PCF8574 cannot act on.
        // Any byte would become its port: on a relay expander, a switched
        // relay.
        let mut bus = Fake::default().bare(0x20);
        find(&mut bus, &PCF8574, None, &[]).expect("found");
        assert!(
            bus.writes.iter().all(|(_, bytes)| bytes.is_empty()),
            "a byte was written to an expander: {:?}",
            bus.writes
        );
    }

    #[test]
    fn a_register_is_read_only_where_something_answered() {
        // Nothing at 0x28-0x2F; the reader at 0x3C. No register pointer may
        // go to the empty addresses, and only one to the reader.
        let mut bus = Fake::default().with(0x3C, 0x37, &[0x91]);
        let found = find(&mut bus, &MFRC522, None, &[]).expect("found");
        assert_eq!(found.address, 0x3C);
        let pointers: Vec<_> = bus.writes.iter().filter(|(_, b)| !b.is_empty()).collect();
        assert_eq!(pointers, vec![&(0x3C, vec![0x37])]);
    }

    #[test]
    fn a_claimed_address_is_not_touched() {
        // The LCD's PCF8574A answered at 0x3C first. Looking for the reader
        // there would write 0x37 to the LCD's port.
        let mut bus = Fake::default().bare(0x3C);
        assert_eq!(find(&mut bus, &MFRC522, None, &[0x3C]), None);
        assert!(bus.writes.is_empty(), "touched: {:?}", bus.writes);
    }

    #[test]
    fn a_board_file_narrows_where_to_look() {
        let mut bus = Fake::default()
            .with(0x28, 0x37, &[0x92])
            .with(0x3C, 0x37, &[0x92]);
        let found = find(&mut bus, &MFRC522, Some(&[0x3C]), &[]).expect("found");
        assert_eq!(found.address, 0x3C);
        assert!(bus.writes.iter().all(|(a, _)| *a == 0x3C));
    }

    #[test]
    fn a_sixteen_bit_identity_is_read_big_endian() {
        let mut bus = Fake::default().with(0x40, 0xFE, &[0x54, 0x49]);
        let found = find(&mut bus, &INA226, None, &[]).expect("found");
        assert_eq!(found.identity, Identity::Confirmed);
        let mut swapped = Fake::default().with(0x40, 0xFE, &[0x49, 0x54]);
        assert_eq!(find(&mut swapped, &INA226, None, &[]), None);
    }

    #[test]
    fn an_assumed_part_says_so() {
        let found = Found {
            part: "PCF8574",
            address: 0x20,
            identity: Identity::Assumed,
        };
        assert_eq!(
            found.to_string(),
            "PCF8574 at 0x20 (assumed: it has no identity register)"
        );
    }
}

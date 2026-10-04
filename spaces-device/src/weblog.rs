//! The device's log, kept for a web page: the parts that need no hardware.
//!
//! - [`LogRing`]: the recent log, bounded by bytes, with every line numbered so
//!   a page can ask for what it has not seen yet and be told what it missed.
//! - [`LineAssembler`]: whole lines out of output that arrives in pieces.
//! - [`strip_ansi`]: the terminal's colour codes, removed.
//! - [`tail`]: the end of the previous boot's log, in memory that survives a
//!   restart, so a crash or a rollback can still be read about afterwards.

use std::collections::VecDeque;

/// The longest line kept. Anything longer is cut, and says so.
pub const MAX_LINE: usize = 512;

/// The most recent log lines, up to a budget of bytes.
#[derive(Debug)]
pub struct LogRing {
    lines: VecDeque<String>,
    bytes: usize,
    budget: usize,
    /// The number of the oldest line still held.
    first: u64,
}

/// What a reader gets for "everything since".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Batch {
    pub lines: Vec<String>,
    /// What to ask for next time.
    pub next: u64,
    /// Lines that had already been dropped to make room before they could be
    /// read: the page says so, rather than pretending the log is complete.
    pub missed: u64,
}

impl LogRing {
    pub fn new(budget: usize) -> LogRing {
        LogRing {
            lines: VecDeque::new(),
            bytes: 0,
            budget,
            first: 0,
        }
    }

    /// Add a line, dropping the oldest to stay within budget.
    pub fn push(&mut self, line: &str) {
        let line = clip(line);
        self.bytes += line.len();
        self.lines.push_back(line);
        while self.bytes > self.budget && self.lines.len() > 1 {
            if let Some(old) = self.lines.pop_front() {
                self.bytes -= old.len();
                self.first += 1;
            }
        }
    }

    /// The number the next line pushed will get.
    pub fn next(&self) -> u64 {
        self.first + self.lines.len() as u64
    }

    /// Every line numbered `since` or later that is still held.
    ///
    /// A `since` beyond anything written comes from a reader that last looked
    /// before the device restarted: it gets this boot's log from the start.
    pub fn since(&self, since: u64) -> Batch {
        let since = if since > self.next() { 0 } else { since };
        let start = since.max(self.first);
        let skip = (start - self.first) as usize;
        Batch {
            lines: self.lines.iter().skip(skip).cloned().collect(),
            next: self.next(),
            missed: start - since,
        }
    }
}

fn clip(line: &str) -> String {
    if line.len() <= MAX_LINE {
        return line.to_string();
    }
    let mut end = MAX_LINE;
    while !line.is_char_boundary(end) {
        end -= 1;
    }
    format!("{} [cut]", &line[..end])
}

/// Turns output that arrives in arbitrary pieces into whole lines.
#[derive(Debug, Default)]
pub struct LineAssembler {
    pending: String,
}

impl LineAssembler {
    /// Feed some output; get back every line it completed, without its line
    /// ending. A line that never ends is cut at [`MAX_LINE`] rather than held
    /// for ever.
    pub fn feed(&mut self, text: &str) -> Vec<String> {
        let mut done = Vec::new();
        for c in text.chars() {
            match c {
                '\n' => done.push(std::mem::take(&mut self.pending)),
                '\r' => {}
                c => {
                    self.pending.push(c);
                    if self.pending.len() >= MAX_LINE {
                        done.push(std::mem::take(&mut self.pending));
                    }
                }
            }
        }
        done
    }
}

/// Remove terminal escape sequences (the log's colours), keeping the text.
pub fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\x1b' {
            out.push(c);
            continue;
        }
        // CSI: ESC [ parameters, ended by a letter. Anything else after ESC
        // is dropped with it.
        if chars.peek() == Some(&'[') {
            chars.next();
            for c in chars.by_ref() {
                if c.is_ascii_alphabetic() {
                    break;
                }
            }
        }
    }
    out
}

/// The end of the log, kept in a fixed buffer in memory that survives a
/// restart but not a power cut, so it can be read after the restart.
///
/// The buffer starts with a header: a magic number, where the next byte goes,
/// and whether the text has wrapped round. After a power cut the memory holds
/// whatever it powered up with, and anything that does not have the magic
/// number and sane positions reads as nothing, not as garbage.
pub mod tail {
    const MAGIC: u32 = 0x5350_4c47; // "SPLG"
    const HEADER: usize = 12;

    /// The smallest buffer worth having.
    pub const MIN_LEN: usize = HEADER + 64;

    fn text_len(buf: &[u8]) -> usize {
        buf.len() - HEADER
    }

    fn word(buf: &[u8], at: usize) -> u32 {
        u32::from_le_bytes([buf[at], buf[at + 1], buf[at + 2], buf[at + 3]])
    }

    fn set_word(buf: &mut [u8], at: usize, value: u32) {
        buf[at..at + 4].copy_from_slice(&value.to_le_bytes());
    }

    /// Start empty.
    pub fn reset(buf: &mut [u8]) {
        assert!(
            buf.len() >= MIN_LEN,
            "a tail buffer needs at least {MIN_LEN} bytes"
        );
        set_word(buf, 0, MAGIC);
        set_word(buf, 4, 0);
        set_word(buf, 8, 0);
    }

    /// What the buffer holds, oldest first, if it holds anything readable.
    /// When it has wrapped, the first line is likely cut, so it is dropped.
    pub fn read(buf: &[u8]) -> Option<String> {
        if buf.len() < MIN_LEN || word(buf, 0) != MAGIC {
            return None;
        }
        let at = word(buf, 4) as usize;
        let wrapped = match word(buf, 8) {
            0 => false,
            1 => true,
            _ => return None,
        };
        let len = text_len(buf);
        if at >= len {
            return None;
        }
        let text = &buf[HEADER..];
        let bytes: Vec<u8> = if wrapped {
            text[at..].iter().chain(&text[..at]).copied().collect()
        } else {
            text[..at].to_vec()
        };
        let mut s = String::from_utf8_lossy(&bytes).into_owned();
        if wrapped {
            match s.find('\n') {
                Some(i) => s.drain(..=i),
                None => s.drain(..),
            };
        }
        (!s.is_empty()).then_some(s)
    }

    /// Add to the end, overwriting the oldest when full. Does nothing to a
    /// buffer that was never [`reset`].
    pub fn append(buf: &mut [u8], bytes: &[u8]) {
        if buf.len() < MIN_LEN || word(buf, 0) != MAGIC {
            return;
        }
        let len = text_len(buf);
        let mut at = (word(buf, 4) as usize).min(len - 1);
        let mut wrapped = word(buf, 8) == 1;
        for &b in bytes {
            buf[HEADER + at] = b;
            at += 1;
            if at == len {
                at = 0;
                wrapped = true;
            }
        }
        set_word(buf, 4, at as u32);
        set_word(buf, 8, u32::from(wrapped));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_reader_gets_what_it_has_not_seen() {
        let mut ring = LogRing::new(1000);
        ring.push("one");
        ring.push("two");
        let first = ring.since(0);
        assert_eq!(first.lines, ["one", "two"]);
        assert_eq!((first.next, first.missed), (2, 0));
        ring.push("three");
        let second = ring.since(first.next);
        assert_eq!(second.lines, ["three"]);
        assert_eq!((second.next, second.missed), (3, 0));
        assert!(ring.since(3).lines.is_empty());
    }

    #[test]
    fn the_oldest_lines_go_to_stay_within_budget() {
        let mut ring = LogRing::new(10);
        for line in ["aaaa", "bbbb", "cccc"] {
            ring.push(line);
        }
        let all = ring.since(0);
        assert_eq!(all.lines, ["bbbb", "cccc"]);
        // The reader is told one line went before it could be read.
        assert_eq!((all.next, all.missed), (3, 1));
        // A reader that was up to date misses nothing.
        let mut ring = LogRing::new(10);
        ring.push("aaaa");
        let seen = ring.since(0).next;
        ring.push("bbbb");
        ring.push("cccc");
        assert_eq!(ring.since(seen).missed, 0);
        assert_eq!(ring.since(seen).lines, ["bbbb", "cccc"]);
    }

    #[test]
    fn a_line_bigger_than_the_budget_is_still_kept() {
        let mut ring = LogRing::new(4);
        ring.push("too long for it");
        assert_eq!(ring.since(0).lines, ["too long for it"]);
    }

    #[test]
    fn a_reader_from_before_a_restart_starts_again() {
        // After a restart the page still holds the old boot's count.
        let mut ring = LogRing::new(100);
        ring.push("fresh");
        ring.push("boot");
        let batch = ring.since(500);
        assert_eq!(batch.lines, ["fresh", "boot"]);
        assert_eq!((batch.next, batch.missed), (2, 0));
        // And says what this boot has already dropped.
        let mut ring = LogRing::new(4);
        ring.push("aaaa");
        ring.push("bbbb");
        assert_eq!(ring.since(500).missed, 1);
    }

    #[test]
    fn long_lines_are_cut_on_a_character_boundary() {
        let mut ring = LogRing::new(10_000);
        let long = "é".repeat(MAX_LINE); // two bytes each
        ring.push(&long);
        let kept = &ring.since(0).lines[0];
        assert!(kept.ends_with(" [cut]"));
        assert!(kept.len() <= MAX_LINE + " [cut]".len());
    }

    #[test]
    fn lines_are_assembled_from_pieces() {
        let mut a = LineAssembler::default();
        assert!(a.feed("I (12) wifi: con").is_empty());
        assert_eq!(
            a.feed("nected\nW (13) x: y\npart"),
            ["I (12) wifi: connected", "W (13) x: y"]
        );
        assert_eq!(a.feed("ial\r\n"), ["partial"]);
    }

    #[test]
    fn a_line_that_never_ends_is_cut_rather_than_held() {
        let mut a = LineAssembler::default();
        let lines = a.feed(&"x".repeat(MAX_LINE + 10));
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].len(), MAX_LINE);
    }

    #[test]
    fn colours_are_stripped() {
        assert_eq!(
            strip_ansi("\x1b[0;32mI (5) boot: ok\x1b[0m"),
            "I (5) boot: ok"
        );
        assert_eq!(strip_ansi("plain"), "plain");
        assert_eq!(strip_ansi("a\x1b[1;31;40mb\x1b[mc"), "abc");
        assert_eq!(strip_ansi("café ✓"), "café ✓");
    }

    #[test]
    fn the_tail_reads_back_what_was_written() {
        let mut buf = [0u8; 128];
        tail::reset(&mut buf);
        assert_eq!(tail::read(&buf), None);
        tail::append(&mut buf, b"first line\n");
        tail::append(&mut buf, b"second line\n");
        assert_eq!(
            tail::read(&buf).as_deref(),
            Some("first line\nsecond line\n")
        );
    }

    #[test]
    fn the_tail_keeps_the_newest_and_drops_a_cut_first_line() {
        let mut buf = [0u8; tail::MIN_LEN]; // 64 bytes of text
        tail::reset(&mut buf);
        for i in 0..20 {
            tail::append(&mut buf, format!("line {i}\n").as_bytes());
        }
        let got = tail::read(&buf).unwrap();
        assert!(got.ends_with("line 19\n"), "{got:?}");
        assert!(
            got.starts_with("line "),
            "the cut line was dropped: {got:?}"
        );
        assert!(!got.contains("line 0\n"));
    }

    #[test]
    fn memory_that_was_never_written_reads_as_nothing() {
        // After a power cut the buffer holds whatever it powered up with.
        assert_eq!(tail::read(&[0u8; 128]), None);
        assert_eq!(tail::read(&[0xffu8; 128]), None);
        let mut noise = [0u8; 128];
        for (i, b) in noise.iter_mut().enumerate() {
            *b = (i * 37 + 11) as u8;
        }
        assert_eq!(tail::read(&noise), None);
        // Even with the magic number, impossible positions read as nothing.
        let mut buf = [0u8; 128];
        tail::reset(&mut buf);
        buf[4..8].copy_from_slice(&5000u32.to_le_bytes());
        assert_eq!(tail::read(&buf), None);
        let mut buf = [0u8; 128];
        tail::reset(&mut buf);
        buf[8..12].copy_from_slice(&7u32.to_le_bytes());
        assert_eq!(tail::read(&buf), None);
    }

    #[test]
    fn appending_to_an_unready_buffer_does_nothing() {
        let mut buf = [0u8; 128];
        tail::append(&mut buf, b"lost\n");
        assert_eq!(buf, [0u8; 128]);
    }
}

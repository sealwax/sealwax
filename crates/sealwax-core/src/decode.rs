//! Filter chains decoded as streams, to measure what each stage decodes to without keeping it:
//! what [`crate::Limits::max_stream_bytes`] checks before anything decodes a stream whole.
//!
//! Every stage is bounded, not only the last: a stage can decode to gigabytes that the next one
//! shrinks to nothing (spaces, which `ASCIIHexDecode` skips), and a reader that decodes a chain
//! stage by stage holds each stage whole.

use std::cell::{Cell, RefCell};
use std::io::{self, BufRead, BufReader, Read};
use std::rc::Rc;

use flate2::read::{DeflateDecoder, ZlibDecoder};

/// A filter decoded here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Step {
    Flate,
    /// `early_change`: the code width grows one code early (ISO 32000-1, 7.4.4.2; the default).
    Lzw {
        early_change: bool,
    },
    RunLength,
    Ascii85,
    AsciiHex,
}

/// The bytes the last stage of `steps` decodes `data` to, counted without keeping them, or `None`
/// once any stage passes `at_most`: its work stops there. What decodes before an error counts,
/// as a reader that keeps what it decoded would keep it.
///
/// Each stage is decoded to its end, not only as far as the next stage reads it: a stage can end
/// early (an end-of-data marker at the start of its input) while a reader that decodes a chain
/// stage by stage has decoded the whole stage before it.
pub(crate) fn decoded_size(data: &[u8], steps: &[Step], at_most: u64) -> Option<u64> {
    let Some(last) = steps.len().checked_sub(1) else {
        let size = u64::try_from(data.len()).unwrap_or(u64::MAX);
        return (size <= at_most).then_some(size);
    };
    let past = Rc::new(Cell::new(false));
    let mut stages: Vec<Rc<RefCell<Bounded<'_>>>> = Vec::with_capacity(steps.len());
    for step in steps {
        let input: Box<dyn Read + '_> = match stages.last() {
            None => Box::new(data),
            Some(previous) => Box::new(Shared(Rc::clone(previous))),
        };
        stages.push(Rc::new(RefCell::new(Bounded {
            inner: decoder(*step, input),
            produced: 0,
            at_most,
            past: Rc::clone(&past),
            done: false,
        })));
    }
    // The last stage, which reads the others as far as it needs them; then, from the last but
    // one back to the first, what each stage still had to give.
    let mut buffer = vec![0; 1 << 16];
    for stage in stages.iter().rev() {
        loop {
            let read = stage.borrow_mut().read(&mut buffer);
            if past.get() {
                return None;
            }
            if matches!(read, Ok(0) | Err(_)) {
                break;
            }
        }
    }
    let produced = stages.get(last)?.borrow().produced;
    Some(produced)
}

/// The decoder of `step`, reading `input`.
fn decoder<'a>(step: Step, input: Box<dyn Read + 'a>) -> Box<dyn Read + 'a> {
    match step {
        Step::Flate => {
            let mut buffered = BufReader::new(input);
            let zlib = matches!(buffered.fill_buf(), Ok([cmf, flg, ..])
                if cmf & 0x0f == 8
                    && (u16::from(*cmf) << 8 | u16::from(*flg)) % 31 == 0
                    && flg & 0x20 == 0);
            if zlib {
                Box::new(ZlibDecoder::new(buffered))
            } else {
                Box::new(DeflateDecoder::new(buffered))
            }
        }
        Step::Lzw { early_change } => Box::new(Lzw::new(input, early_change)),
        Step::RunLength => Box::new(RunLength::new(input)),
        Step::Ascii85 => Box::new(Ascii85::new(input)),
        Step::AsciiHex => Box::new(AsciiHex::new(input)),
    }
}

/// A stage that stops, and says so, once it has produced more than `at_most` bytes. Once its
/// decoder has ended, or failed, it gives nothing more.
struct Bounded<'a> {
    inner: Box<dyn Read + 'a>,
    produced: u64,
    at_most: u64,
    past: Rc<Cell<bool>>,
    done: bool,
}

impl Read for Bounded<'_> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if self.past.get() {
            return Err(io::Error::other("past the bound"));
        }
        if self.done {
            return Ok(0);
        }
        let read = match self.inner.read(out) {
            Ok(0) => {
                self.done = true;
                return Ok(0);
            }
            Ok(read) => read,
            Err(error) => {
                self.done = true;
                return Err(error);
            }
        };
        self.produced = self
            .produced
            .saturating_add(u64::try_from(read).unwrap_or(u64::MAX));
        if self.produced > self.at_most {
            self.past.set(true);
            return Err(io::Error::other("past the bound"));
        }
        Ok(read)
    }
}

/// A stage read by the next one, and drained apart afterwards.
struct Shared<'a>(Rc<RefCell<Bounded<'a>>>);

impl Read for Shared<'_> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        self.0.borrow_mut().read(out)
    }
}

fn invalid() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "invalid encoded data")
}

/// Output decoded ahead of what was asked for, handed out first.
#[derive(Default)]
struct Pending {
    bytes: Vec<u8>,
    at: usize,
}

impl Pending {
    fn give(&mut self, out: &mut [u8]) -> usize {
        let available = &self.bytes[self.at..];
        let n = available.len().min(out.len());
        out[..n].copy_from_slice(&available[..n]);
        self.at += n;
        if self.at == self.bytes.len() {
            self.bytes.clear();
            self.at = 0;
        }
        n
    }

    fn is_empty(&self) -> bool {
        self.at >= self.bytes.len()
    }
}

/// The next input byte, `None` at the end.
fn next_byte<R: Read>(input: &mut BufReader<R>) -> io::Result<Option<u8>> {
    let byte = input.fill_buf()?.first().copied();
    if byte.is_some() {
        input.consume(1);
    }
    Ok(byte)
}

/// `ASCIIHexDecode` (ISO 32000-1, 7.4.2).
struct AsciiHex<R> {
    input: BufReader<R>,
    /// A digit read, waiting for the one that completes its byte.
    high: Option<u8>,
    done: bool,
}

impl<R: Read> AsciiHex<R> {
    fn new(inner: R) -> Self {
        Self {
            input: BufReader::new(inner),
            high: None,
            done: false,
        }
    }
}

impl<R: Read> Read for AsciiHex<R> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        let mut written = 0;
        while written < out.len() {
            if self.done {
                // An odd last digit is followed by a zero.
                if let Some(high) = self.high.take() {
                    out[written] = high << 4;
                    written += 1;
                }
                break;
            }
            let Some(byte) = next_byte(&mut self.input)? else {
                self.done = true;
                continue;
            };
            let nibble = match byte {
                b'0'..=b'9' => byte - b'0',
                b'a'..=b'f' => byte - b'a' + 10,
                b'A'..=b'F' => byte - b'A' + 10,
                b'>' => {
                    self.done = true;
                    continue;
                }
                _ if byte.is_ascii_whitespace() || byte == 0 => continue,
                _ => return Err(invalid()),
            };
            match self.high.take() {
                None => self.high = Some(nibble),
                Some(high) => {
                    out[written] = high << 4 | nibble;
                    written += 1;
                }
            }
        }
        Ok(written)
    }
}

/// `ASCII85Decode` (ISO 32000-1, 7.4.3).
struct Ascii85<R> {
    input: BufReader<R>,
    pending: Pending,
    done: bool,
}

impl<R: Read> Ascii85<R> {
    fn new(inner: R) -> Self {
        Self {
            input: BufReader::new(inner),
            pending: Pending::default(),
            done: false,
        }
    }

    /// Decode the next group into `pending`.
    fn group(&mut self) -> io::Result<()> {
        let mut digits = [0u8; 5];
        let mut count = 0;
        loop {
            let Some(byte) = next_byte(&mut self.input)? else {
                self.done = true;
                break;
            };
            match byte {
                b'z' if count == 0 => {
                    self.pending.bytes.extend_from_slice(&[0; 4]);
                    return Ok(());
                }
                b'!'..=b'u' => {
                    digits[count] = byte - b'!';
                    count += 1;
                    if count == 5 {
                        break;
                    }
                }
                b'~' => {
                    self.done = true;
                    break;
                }
                _ if byte.is_ascii_whitespace() || byte == 0 => {}
                _ => return Err(invalid()),
            }
        }
        if count == 0 {
            return Ok(());
        }
        if count == 1 {
            return Err(invalid());
        }
        for digit in digits.iter_mut().skip(count) {
            *digit = 84;
        }
        let value = digits
            .iter()
            .try_fold(0u32, |acc, d| {
                acc.checked_mul(85)?.checked_add(u32::from(*d))
            })
            .ok_or_else(invalid)?;
        self.pending
            .bytes
            .extend_from_slice(&value.to_be_bytes()[..count - 1]);
        Ok(())
    }
}

impl<R: Read> Read for Ascii85<R> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        while self.pending.is_empty() && !self.done {
            self.group()?;
        }
        Ok(self.pending.give(out))
    }
}

/// `RunLengthDecode` (ISO 32000-1, 7.4.5).
struct RunLength<R> {
    input: BufReader<R>,
    /// Bytes still to copy as they come.
    copy: usize,
    /// A byte still to repeat, and how many times.
    repeat: Option<(u8, usize)>,
    done: bool,
}

impl<R: Read> RunLength<R> {
    fn new(inner: R) -> Self {
        Self {
            input: BufReader::new(inner),
            copy: 0,
            repeat: None,
            done: false,
        }
    }
}

impl<R: Read> Read for RunLength<R> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        let mut written = 0;
        while written < out.len() {
            if let Some((byte, left)) = self.repeat {
                let n = left.min(out.len() - written);
                out[written..written + n].fill(byte);
                written += n;
                self.repeat = (left > n).then_some((byte, left - n));
                continue;
            }
            if self.copy > 0 {
                let Some(byte) = next_byte(&mut self.input)? else {
                    self.done = true;
                    break;
                };
                out[written] = byte;
                written += 1;
                self.copy -= 1;
                continue;
            }
            if self.done {
                break;
            }
            match next_byte(&mut self.input)? {
                None | Some(128) => self.done = true,
                Some(length @ 0..=127) => self.copy = usize::from(length) + 1,
                Some(length) => match next_byte(&mut self.input)? {
                    Some(byte) => self.repeat = Some((byte, 257 - usize::from(length))),
                    None => self.done = true,
                },
            }
        }
        Ok(written)
    }
}

/// `LZWDecode` (ISO 32000-1, 7.4.4): 9 to 12-bit codes, most significant bit first, 256 clears
/// the table, 257 ends the data.
struct Lzw<R> {
    input: BufReader<R>,
    early: u16,
    bits: u32,
    bit_count: u32,
    width: u32,
    /// For each code from 258: the code it extends, its last byte, its length.
    prefix: Vec<u16>,
    suffix: Vec<u8>,
    length: Vec<u16>,
    previous: Option<u16>,
    pending: Pending,
    done: bool,
}

const LZW_CLEAR: u16 = 256;
const LZW_END: u16 = 257;
const LZW_FIRST: u16 = 258;
const LZW_TABLE: usize = 4096;

impl<R: Read> Lzw<R> {
    fn new(inner: R, early_change: bool) -> Self {
        Self {
            input: BufReader::new(inner),
            early: u16::from(early_change),
            bits: 0,
            bit_count: 0,
            width: 9,
            prefix: Vec::with_capacity(LZW_TABLE),
            suffix: Vec::with_capacity(LZW_TABLE),
            length: Vec::with_capacity(LZW_TABLE),
            previous: None,
            pending: Pending::default(),
            done: false,
        }
    }

    fn next_code(&mut self) -> io::Result<Option<u16>> {
        while self.bit_count < self.width {
            let Some(byte) = next_byte(&mut self.input)? else {
                return Ok(None);
            };
            self.bits = (self.bits << 8) | u32::from(byte);
            self.bit_count += 8;
        }
        self.bit_count -= self.width;
        let code = (self.bits >> self.bit_count) & ((1 << self.width) - 1);
        self.bits &= (1 << self.bit_count) - 1;
        Ok(u16::try_from(code).ok())
    }

    fn next_free(&self) -> u16 {
        LZW_FIRST + u16::try_from(self.prefix.len()).unwrap_or(u16::MAX - LZW_FIRST)
    }

    fn length_of(&self, code: u16) -> usize {
        match code.checked_sub(LZW_FIRST) {
            None => 1,
            Some(index) => usize::from(self.length[usize::from(index)]),
        }
    }

    /// Append the string of `code` (already in the table) to `pending`.
    fn emit(&mut self, code: u16) {
        let length = self.length_of(code);
        let start = self.pending.bytes.len();
        self.pending.bytes.resize(start + length, 0);
        let mut current = code;
        for slot in (start..start + length).rev() {
            match current.checked_sub(LZW_FIRST) {
                None => {
                    self.pending.bytes[slot] = u8::try_from(current).unwrap_or(0);
                    break;
                }
                Some(index) => {
                    let index = usize::from(index);
                    self.pending.bytes[slot] = self.suffix[index];
                    current = self.prefix[index];
                }
            }
        }
    }

    fn first_byte(&self, mut code: u16) -> u8 {
        while let Some(index) = code.checked_sub(LZW_FIRST) {
            code = self.prefix[usize::from(index)];
        }
        u8::try_from(code).unwrap_or(0)
    }

    fn add(&mut self, prefix: u16, byte: u8) {
        if self.prefix.len() + usize::from(LZW_FIRST) >= LZW_TABLE {
            return;
        }
        let length = self.length_of(prefix) + 1;
        self.prefix.push(prefix);
        self.suffix.push(byte);
        self.length.push(u16::try_from(length).unwrap_or(u16::MAX));
        if self.next_free() + self.early >= 1 << self.width && self.width < 12 {
            self.width += 1;
        }
    }

    /// Decode codes until something is pending or the data ends.
    fn step(&mut self) -> io::Result<()> {
        let Some(code) = self.next_code()? else {
            self.done = true;
            return Ok(());
        };
        match code {
            LZW_CLEAR => {
                self.prefix.clear();
                self.suffix.clear();
                self.length.clear();
                self.width = 9;
                self.previous = None;
            }
            LZW_END => self.done = true,
            _ => {
                let next = self.next_free();
                match self.previous {
                    _ if code < LZW_CLEAR || (code >= LZW_FIRST && code < next) => {
                        self.emit(code);
                        if let Some(previous) = self.previous {
                            let byte = self.first_byte(code);
                            self.add(previous, byte);
                        }
                    }
                    Some(previous) if code == next => {
                        let byte = self.first_byte(previous);
                        self.add(previous, byte);
                        self.emit(code);
                    }
                    _ => return Err(invalid()),
                }
                self.previous = Some(code);
            }
        }
        Ok(())
    }
}

impl<R: Read> Read for Lzw<R> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        while self.pending.is_empty() && !self.done {
            self.step()?;
        }
        Ok(self.pending.give(out))
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use std::io::Write as _;

    use super::{Step, decoded_size};

    fn deflate(data: &[u8]) -> Vec<u8> {
        let mut encoder =
            flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(data).unwrap();
        encoder.finish().unwrap()
    }

    #[test]
    fn each_filter_decodes_to_its_size() {
        assert_eq!(
            decoded_size(b"48 65 6c6C6f 7>", &[Step::AsciiHex], 100),
            Some(6)
        );
        assert_eq!(
            decoded_size(b"87cURD]i,\"Ebo80~>", &[Step::Ascii85], 100),
            Some(12)
        );
        assert_eq!(decoded_size(b"z!!~>", &[Step::Ascii85], 100), Some(5));
        // Copy 3 bytes, repeat one 100 times, end.
        assert_eq!(
            decoded_size(
                &[2, b'a', b'b', b'c', 157, b'x', 128],
                &[Step::RunLength],
                1000
            ),
            Some(103)
        );
        assert_eq!(
            decoded_size(&deflate(&[7; 5000]), &[Step::Flate], 10_000),
            Some(5000)
        );
    }

    /// The LZW example of ISO 32000-1, 7.4.4.2: `-----A---B` encoded with early change.
    #[test]
    fn lzw_decodes_the_standard_example() {
        let encoded = [0x80, 0x0B, 0x60, 0x50, 0x22, 0x0C, 0x0C, 0x85, 0x01];
        assert_eq!(
            decoded_size(&encoded, &[Step::Lzw { early_change: true }], 100),
            Some(10)
        );
    }

    /// Every stage is bounded: 8 KiB of spaces deflated, then hex-encoded, pass a bound of 1 KiB
    /// in the middle stage though the last one would shrink them to nothing.
    #[test]
    fn every_stage_is_bounded() {
        let spaces = vec![b' '; 8192];
        let hex: String = deflate(&spaces)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        let chain = [Step::AsciiHex, Step::Flate];
        assert_eq!(decoded_size(hex.as_bytes(), &chain, 1024), None);
        assert_eq!(decoded_size(hex.as_bytes(), &chain, 10_000), Some(8192));
        // A stage that ends at once (`>` first) leaves the stage before it to decode whole.
        let mut early = b">".to_vec();
        early.extend_from_slice(&spaces);
        let chain = [Step::Flate, Step::AsciiHex];
        assert_eq!(decoded_size(&deflate(&early), &chain, 1024), None);
        assert_eq!(decoded_size(&deflate(&early), &chain, 10_000), Some(0));
        let inner = deflate(&spaces);
        let twice = deflate(&inner);
        assert_eq!(
            decoded_size(&twice, &[Step::Flate, Step::Flate, Step::AsciiHex], 1024),
            None
        );
    }
}

//! Convert an Oracle NUMBER from its internal (base-100) byte representation
//! into an exact decimal string.
//!
//! Encoding summary:
//! * byte 0 is the sign/exponent byte, the remaining bytes are base-100 digits
//!   (most significant first).
//! * `0x80` alone is zero.
//! * positive: exponent byte >= 0x80, exponent = byte - 193, digit = byte - 1
//!   (`0x80 0x02` is 1e-130, the smallest positive value)
//! * negative: exponent byte < 0x80, exponent = 62 - byte, digit = 101 - byte,
//!   optionally terminated by a 102 (0x66) byte.
//! * `0xFF 0x65` is +infinity, `0x00` alone is -infinity.

use clap::{Parser, ValueEnum};
use std::fmt;
use std::process::ExitCode;

#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
enum Format {
    /// Detect the format from the input
    Auto,
    /// Decimal byte list, e.g. "194,2,24" (output of DUMP(x))
    Dec,
    /// Hex bytes, e.g. "C20218", "c2,2,18" or "col 0: [ 3] c2 02 18"
    /// (output of DUMP(x,16), RAWTOHEX or a block dump)
    Hex,
}

/// Convert an Oracle internal NUMBER representation to a decimal number.
#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
struct Args {
    /// Oracle internal number, e.g. "Typ=2 Len=3: 194,2,24", "194,2,24", "c2,2,18", "C20218"
    /// or "col 0: [ 3] c2 02 18"
    input: String,

    /// Input format
    #[arg(short, long, value_enum, default_value_t = Format::Auto)]
    format: Format,

    /// Print sign, exponent and base-100 digits as well
    #[arg(short, long)]
    verbose: bool,
}

#[derive(Debug, PartialEq, Eq)]
enum Error {
    Empty,
    TooLong(usize),
    BadToken(String),
    BadDigit { pos: usize, byte: u8 },
    BadExponent(u8),
    BadPrefix(String),
    LengthMismatch { declared: usize, actual: usize },
    NullColumn(u32),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Empty => write!(f, "no bytes given"),
            Error::TooLong(n) => write!(f, "{n} bytes given, an Oracle NUMBER has at most 22"),
            Error::BadToken(t) => write!(f, "cannot parse '{t}' as a byte"),
            Error::BadDigit { pos, byte } => {
                write!(f, "byte {pos} (value {byte}) is not a valid base-100 digit")
            }
            Error::BadExponent(b) => write!(f, "invalid sign/exponent byte {b} (0x{b:02X})"),
            Error::BadPrefix(t) => write!(
                f,
                "cannot parse block dump prefix in '{t}', expected e.g. 'col 0: [ 2] c1 04'"
            ),
            Error::NullColumn(col) => {
                write!(
                    f,
                    "column {col} is NULL in the block dump, there is no number to convert"
                )
            }
            Error::LengthMismatch { declared, actual } => {
                write!(
                    f,
                    "block dump declares {declared} bytes, but {actual} are given"
                )
            }
        }
    }
}

/// Parse the command line text into raw bytes.
fn parse_input(input: &str, format: Format) -> Result<Vec<u8>, Error> {
    let mut text = input.trim();
    let mut from_dump = false;
    // Strip a "Typ=2 Len=3:" prefix as produced by DUMP().
    if text.starts_with("Typ=")
        && let Some(idx) = text.find(':')
    {
        text = text[idx + 1..].trim();
        from_dump = true;
    }
    // Strip a "col 0: [ 2]" prefix as produced by a block dump; the bytes are hex.
    let mut declared_len = None;
    if let Some((rest, len)) = strip_block_dump_prefix(text)? {
        text = rest;
        declared_len = Some(len);
    }

    let tokens: Vec<&str> = text
        .split(|c: char| c == ',' || c.is_whitespace())
        .filter(|t| !t.is_empty())
        .collect();
    if tokens.is_empty() {
        return Err(Error::Empty);
    }

    let has_hex_letters = text.chars().any(|c| matches!(c, 'a'..='f' | 'A'..='F'));
    // Hex-dump style: space separated, no commas, every token exactly two characters ("c2 02 18").
    let hex_dump_style =
        tokens.len() > 1 && !text.contains(',') && tokens.iter().all(|t| t.len() == 2);
    let format = match format {
        Format::Auto if declared_len.is_some() => Format::Hex,
        Format::Auto if tokens.len() > 1 || from_dump => {
            if has_hex_letters || hex_dump_style {
                Format::Hex
            } else {
                Format::Dec
            }
        }
        Format::Auto => Format::Hex, // single token: contiguous hex string
        f => f,
    };

    let bytes = match format {
        Format::Dec => tokens
            .iter()
            .map(|t| t.parse::<u8>().map_err(|_| Error::BadToken(t.to_string())))
            .collect::<Result<Vec<_>, _>>()?,
        Format::Hex if tokens.len() == 1 => parse_hex_string(tokens[0])?,
        Format::Hex => tokens
            .iter()
            .map(|t| u8::from_str_radix(t, 16).map_err(|_| Error::BadToken(t.to_string())))
            .collect::<Result<Vec<_>, _>>()?,
        Format::Auto => unreachable!(),
    };
    if let Some(declared) = declared_len
        && declared != bytes.len()
    {
        return Err(Error::LengthMismatch {
            declared,
            actual: bytes.len(),
        });
    }
    Ok(bytes)
}

/// Strip a block dump prefix such as "col 0: [ 2]" (column number, then the
/// byte count in brackets). Returns the remaining text and the declared byte
/// count, or `None` if the text does not start with "col". A NULL column
/// ("col 1: *NULL*") is reported as `Error::NullColumn`.
fn strip_block_dump_prefix(text: &str) -> Result<Option<(&str, usize)>, Error> {
    let Some(rest) = text.strip_prefix("col") else {
        return Ok(None);
    };
    let bad = || Error::BadPrefix(text.to_string());
    let (col, rest) = rest.split_once(':').ok_or_else(bad)?;
    let col = col.trim().parse::<u32>().map_err(|_| bad())?;
    if rest.trim() == "*NULL*" {
        return Err(Error::NullColumn(col));
    }
    let rest = rest.trim_start().strip_prefix('[').ok_or_else(bad)?;
    let (len, rest) = rest.split_once(']').ok_or_else(bad)?;
    let len = len.trim().parse::<usize>().map_err(|_| bad())?;
    Ok(Some((rest.trim(), len)))
}

/// Parse a contiguous hex string such as "C20218" or "0xC20218".
fn parse_hex_string(s: &str) -> Result<Vec<u8>, Error> {
    let s = s
        .strip_prefix("0x")
        .or_else(|| s.strip_prefix("0X"))
        .unwrap_or(s);
    if !s.len().is_multiple_of(2) || !s.is_ascii() {
        return Err(Error::BadToken(s.to_string()));
    }
    (0..s.len())
        .step_by(2)
        .map(|i| {
            u8::from_str_radix(&s[i..i + 2], 16)
                .map_err(|_| Error::BadToken(s[i..i + 2].to_string()))
        })
        .collect()
}

/// Decoded components of an Oracle NUMBER.
#[derive(Debug, PartialEq, Eq)]
enum Decoded {
    Zero,
    PosInfinity,
    NegInfinity,
    Finite {
        negative: bool,
        /// Base-100 exponent of the first digit.
        exponent: i32,
        /// Base-100 digits (0..=99), most significant first.
        digits: Vec<u8>,
    },
}

fn decode(bytes: &[u8]) -> Result<Decoded, Error> {
    let (&first, rest) = bytes.split_first().ok_or(Error::Empty)?;
    if bytes.len() > 22 {
        return Err(Error::TooLong(bytes.len()));
    }
    match (first, rest) {
        (0x80, []) => return Ok(Decoded::Zero),
        (0xFF, [0x65]) => return Ok(Decoded::PosInfinity),
        (0x00, []) => return Ok(Decoded::NegInfinity),
        _ => {}
    }

    // 0x80 and 0x00 followed by digits are the smallest/largest exponents
    // (-65 and 62), not zero or -infinity.
    let (negative, exponent, digits) = if first >= 0x80 {
        let digits = rest
            .iter()
            .enumerate()
            .map(|(i, &b)| match b {
                1..=100 => Ok(b - 1),
                _ => Err(Error::BadDigit {
                    pos: i + 1,
                    byte: b,
                }),
            })
            .collect::<Result<Vec<_>, _>>()?;
        (false, first as i32 - 193, digits)
    } else {
        let rest = rest.strip_suffix(&[102]).unwrap_or(rest);
        let digits = rest
            .iter()
            .enumerate()
            .map(|(i, &b)| match b {
                2..=101 => Ok(101 - b),
                _ => Err(Error::BadDigit {
                    pos: i + 1,
                    byte: b,
                }),
            })
            .collect::<Result<Vec<_>, _>>()?;
        (true, 62 - first as i32, digits)
    };

    if digits.is_empty() {
        return Err(Error::BadExponent(first));
    }
    Ok(Decoded::Finite {
        negative,
        exponent,
        digits,
    })
}

/// Render the decoded number as an exact decimal string.
fn to_decimal(d: &Decoded) -> String {
    let (negative, exponent, digits) = match d {
        Decoded::Zero => return "0".into(),
        Decoded::PosInfinity => return "~".into(),
        Decoded::NegInfinity => return "-~".into(),
        Decoded::Finite {
            negative,
            exponent,
            digits,
        } => (*negative, *exponent, digits),
    };

    // Digit at index i has weight 100^(exponent - i).
    // Integer part: indices 0..=exponent; fraction: everything after.
    let pair = |i: i32| -> String {
        if i >= 0 && (i as usize) < digits.len() {
            format!("{:02}", digits[i as usize])
        } else {
            "00".into()
        }
    };

    let mut int_part = String::new();
    for i in 0..=exponent {
        int_part.push_str(&pair(i));
    }
    let mut frac_part = String::new();
    for i in (exponent + 1).max(0)..digits.len() as i32 {
        frac_part.push_str(&pair(i));
    }
    // Leading zero groups for numbers < 1 (exponent < -1).
    if exponent < -1 {
        frac_part.insert_str(0, &"00".repeat((-exponent - 1) as usize));
    }

    let int_part = int_part.trim_start_matches('0');
    let frac_part = frac_part.trim_end_matches('0');

    let mut out = String::new();
    if negative {
        out.push('-');
    }
    out.push_str(if int_part.is_empty() { "0" } else { int_part });
    if !frac_part.is_empty() {
        out.push('.');
        out.push_str(frac_part);
    }
    out
}

fn main() -> ExitCode {
    let args = Args::parse();
    let result = parse_input(&args.input, args.format).and_then(|bytes| {
        let decoded = decode(&bytes)?;
        Ok((bytes, decoded))
    });

    match result {
        Ok((bytes, decoded)) => {
            if args.verbose {
                let hex: Vec<String> = bytes.iter().map(|b| format!("{b:02X}")).collect();
                println!("bytes:    {}", hex.join(" "));
                if let Decoded::Finite {
                    negative,
                    exponent,
                    digits,
                } = &decoded
                {
                    println!("sign:     {}", if *negative { "-" } else { "+" });
                    println!("exponent: {exponent} (base 100)");
                    println!("digits:   {digits:?}");
                }
                print!("value:    ");
            }
            println!("{}", to_decimal(&decoded));
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conv(input: &str) -> String {
        to_decimal(&decode(&parse_input(input, Format::Auto).unwrap()).unwrap())
    }

    #[test]
    fn known_values() {
        assert_eq!(conv("80"), "0");
        assert_eq!(conv("C102"), "1");
        assert_eq!(conv("C202"), "100");
        assert_eq!(conv("C20218"), "123");
        assert_eq!(conv("3D644E66"), "-123");
        assert_eq!(conv("C033"), "0.5");
        assert_eq!(conv("3F3366"), "-0.5");
        assert_eq!(conv("C20D23394F"), "1234.5678");
        assert_eq!(conv("BF02"), "0.0001");
        assert_eq!(conv("C30B"), "100000");
        assert_eq!(conv("3E6366"), "-2");
        assert_eq!(conv("FF65"), "~");
        assert_eq!(conv("00"), "-~");
    }

    #[test]
    fn input_formats() {
        assert_eq!(conv("Typ=2 Len=3: 194,2,24"), "123");
        assert_eq!(conv("194,2,24"), "123");
        assert_eq!(conv("194 2 24"), "123");
        assert_eq!(conv("c2 02 18"), "123");
        assert_eq!(conv("C2 02 18"), "123");
        assert_eq!(conv("  c2  02 18 "), "123");
        assert_eq!(conv("3d 64 4e 66"), "-123");
        // No hex letters, but hex-dump style -> still hex: 0x99 0x02 = 1e-80
        assert_eq!(conv("99 02"), format!("0.{}1", "0".repeat(79)));
        assert_eq!(conv("Typ=2 Len=3: c2,2,18"), "123");
        assert_eq!(conv("0xC20218"), "123");
        assert_eq!(conv("Typ=2 Len=1: 128"), "0");
        let b = parse_input("10,20", Format::Hex).unwrap();
        assert_eq!(b, vec![0x10, 0x20]);
    }

    #[test]
    fn block_dump() {
        assert_eq!(conv("col 0: [ 2] c1 04"), "3");
        assert_eq!(conv("col  1: [ 3]  c2 02 18"), "123");
        assert_eq!(conv("col 2: [ 4] 3d 64 4e 66"), "-123");
        assert_eq!(conv("col 3: [ 1] 80"), "0");
        assert_eq!(conv("col 4: [ 5] c2 0d 23 39 4f"), "1234.5678");
        // Block dump bytes are hex even without hex letters.
        assert_eq!(conv("col 5: [ 2] 99 02"), format!("0.{}1", "0".repeat(79)));
        assert_eq!(
            conv("col 12: [10] c5 02 02 02 02 02 02 02 02 02"),
            "101010101.01010101"
        );
    }

    #[test]
    fn block_dump_errors() {
        assert_eq!(
            parse_input("col 0: [ 3] c1 04", Format::Auto),
            Err(Error::LengthMismatch {
                declared: 3,
                actual: 2
            })
        );
        assert_eq!(parse_input("col 0: [ 0]", Format::Auto), Err(Error::Empty));
        assert_eq!(
            parse_input("col 1: *NULL*", Format::Auto),
            Err(Error::NullColumn(1))
        );
        assert_eq!(
            parse_input("col  7:  *NULL* ", Format::Auto),
            Err(Error::NullColumn(7))
        );
        for bad in [
            "col 0 [ 2] c1 04",
            "col x: [ 2] c1 04",
            "col 0: 2] c1 04",
            "col 0: [ 2 c1 04",
        ] {
            assert!(
                matches!(parse_input(bad, Format::Auto), Err(Error::BadPrefix(_))),
                "{bad}"
            );
        }
    }

    #[test]
    fn errors() {
        assert_eq!(parse_input("", Format::Auto), Err(Error::Empty));
        assert!(parse_input("C2021", Format::Auto).is_err());
        assert_eq!(
            decode(&[0xC2, 0x00]),
            Err(Error::BadDigit { pos: 1, byte: 0 })
        );
        // Exponent byte without any digits.
        assert_eq!(decode(&[0xC1]), Err(Error::BadExponent(0xC1)));
        assert_eq!(decode(&[0x3E, 0x66]), Err(Error::BadExponent(0x3E)));
    }

    /// Hex string of an exponent byte followed by `n` copies of a digit byte.
    fn repeat(first: u8, digit: u8, n: usize) -> String {
        std::iter::once(first)
            .chain(std::iter::repeat_n(digit, n))
            .map(|b| format!("{b:02X}"))
            .collect()
    }

    #[test]
    fn range_limits() {
        // Smallest magnitudes: 1e-130 uses exponent byte 0x80 / 0x7F.
        assert_eq!(conv("8002"), format!("0.{}1", "0".repeat(129)));
        assert_eq!(conv("7F6466"), format!("-0.{}1", "0".repeat(129)));
        // 1e-128, the smallest value with a "regular" exponent byte.
        assert_eq!(conv("8102"), format!("0.{}1", "0".repeat(127)));
        // 1e125 and -1e125: exponent 62 (bytes 0xFF / 0x00).
        assert_eq!(conv("FF0B"), format!("1{}", "0".repeat(125)));
        assert_eq!(conv("005B66"), format!("-1{}", "0".repeat(125)));
        // Largest magnitudes: 40 nines followed by 86 zeros (126 digits).
        let max = format!("{}{}", "9".repeat(40), "0".repeat(86));
        assert_eq!(conv(&repeat(0xFF, 0x64, 20)), max);
        assert_eq!(
            conv(&format!("{}66", repeat(0x00, 0x02, 20))),
            format!("-{max}")
        );
        // Largest value below +infinity must not be confused with it.
        assert_eq!(conv("FF64"), format!("99{}", "0".repeat(124)));
        // Most negative value with one digit must not be confused with -infinity.
        assert_eq!(conv("0002"), format!("-99{}", "0".repeat(124)));
    }

    #[test]
    fn precision_limits() {
        // 20 base-100 digits = 40 significant decimal digits.
        let digits = "0D23394F5B".repeat(4);
        assert_eq!(
            conv(&format!("C1{digits}")),
            "12.3456789012345678901234567890123456789"
        );
        assert_eq!(
            conv(&format!("C0{digits}")),
            "0.123456789012345678901234567890123456789"
        );
        // All digits 99, as integer and as fraction.
        assert_eq!(conv(&repeat(0xD4, 0x64, 20)), "9".repeat(40));
        assert_eq!(
            conv(&repeat(0xC0, 0x64, 20)),
            format!("0.{}", "9".repeat(40))
        );
        // Negative with 20 digits: 22 bytes with terminator, 21 without.
        let neg = format!("-{}", "9".repeat(40));
        assert_eq!(conv(&format!("{}66", repeat(0x2B, 0x02, 20))), neg);
        assert_eq!(conv(&repeat(0x2B, 0x02, 20)), neg);
        // 22 bytes is the maximum, 23 is rejected.
        assert_eq!(
            conv(&repeat(0xC1, 0x02, 21)),
            format!("1.{}", "01".repeat(20))
        );
        assert_eq!(decode(&[0xC2; 23]), Err(Error::TooLong(23)));
    }

    #[test]
    fn zero_digits() {
        // Inner and trailing zero digits (byte 0x01 / 0x65).
        assert_eq!(conv("C3020102"), "10001");
        assert_eq!(conv("C30201"), "10000");
        assert_eq!(conv("C3020101"), "10000");
        assert_eq!(conv("3C64656466"), "-10001");
        // Fractions with leading zeros.
        assert_eq!(conv("C00B"), "0.1");
        assert_eq!(conv("C002"), "0.01");
        assert_eq!(conv("BF0B"), "0.001");
        assert_eq!(conv("3F5B66"), "-0.1");
        assert_eq!(conv("3F6466"), "-0.01");
        // Integer and fraction separated by zero digits: 1.0001
        assert_eq!(conv("C1020102"), "1.0001");
    }

    #[test]
    fn digit_boundaries() {
        // Positive digits are 1..=100, negative digits 2..=101.
        assert_eq!(conv("C164"), "99");
        assert_eq!(conv("C101"), "0");
        assert_eq!(
            decode(&[0xC1, 0x65]),
            Err(Error::BadDigit { pos: 1, byte: 0x65 })
        );
        assert_eq!(conv("3E0266"), "-99");
        assert_eq!(
            decode(&[0x3E, 0x01]),
            Err(Error::BadDigit { pos: 1, byte: 0x01 })
        );
        // 102 is only a terminator at the end, not in the middle.
        assert_eq!(
            decode(&[0x3E, 0x66, 0x02, 0x66]),
            Err(Error::BadDigit { pos: 1, byte: 0x66 })
        );
        assert_eq!(
            decode(&[0x3E, 0x67]),
            Err(Error::BadDigit { pos: 1, byte: 0x67 })
        );
        // 0xFF followed by anything other than a single 0x65 is a regular number.
        assert_eq!(
            decode(&[0xFF, 0x65, 0x02]),
            Err(Error::BadDigit { pos: 1, byte: 0x65 })
        );
    }

    #[test]
    fn input_limits() {
        // Decimal bytes must fit in u8.
        assert_eq!(conv("Typ=2 Len=2: 255,101"), "~");
        assert_eq!(conv("Typ=2 Len=1: 0"), "-~");
        assert_eq!(
            parse_input("194,256", Format::Auto),
            Err(Error::BadToken("256".into()))
        );
        assert_eq!(
            parse_input("194,-1", Format::Auto),
            Err(Error::BadToken("-1".into()))
        );
        assert_eq!(
            parse_input("c1 1g", Format::Hex),
            Err(Error::BadToken("1g".into()))
        );
        assert_eq!(
            parse_input("0x", Format::Auto).and_then(|b| decode(&b)),
            Err(Error::Empty)
        );
        // Non-ASCII input must not panic when slicing the hex string.
        assert!(parse_input("c1ä2", Format::Auto).is_err());
        // Maximum length in every input format.
        let hex22 = format!("{}66", repeat(0x2B, 0x02, 20));
        assert_eq!(conv(&hex22), format!("-{}", "9".repeat(40)));
        let spaced: Vec<String> = (0..22)
            .map(|i| hex22[2 * i..2 * i + 2].to_string())
            .collect();
        assert_eq!(
            conv(&format!("col 0: [22] {}", spaced.join(" "))),
            format!("-{}", "9".repeat(40))
        );
        let dec: Vec<String> = spaced
            .iter()
            .map(|t| u8::from_str_radix(t, 16).unwrap().to_string())
            .collect();
        assert_eq!(
            conv(&format!("Typ=2 Len=22: {}", dec.join(","))),
            format!("-{}", "9".repeat(40))
        );
    }
}

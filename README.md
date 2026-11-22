# ora-num

Convert an Oracle `NUMBER` from its internal byte representation (as shown by
`DUMP()`, `DUMP(x, 16)`, `RAWTOHEX()` or a block dump) into an exact decimal
string, and with `--reverse` a decimal number into its internal representation.

## Building

```sh
cargo build --release
```

The binary is written to `target/release/ora-num`.

## Usage

```
ora-num [OPTIONS] <INPUT>
```

| Option                  | Description                                                    |
|-------------------------|----------------------------------------------------------------|
| `-r, --reverse`         | Convert a decimal number into the internal representation     |
| `-f, --format <FORMAT>` | Input format (output format with `-r`): `auto` (default), `dec` or `hex` |
| `-v, --verbose`         | Also print the bytes, sign, exponent and digits                |
| `-h, --help`            | Show help                                                      |
| `-V, --version`         | Show version                                                   |

Without `-r`, the input is an Oracle internal number and the output is the
decimal value. This is the default mode.

### Accepted input

All of the following decode to `123`:

```sh
ora-num "Typ=2 Len=3: 194,2,24"   # DUMP(x)
ora-num "Typ=2 Len=3: c2,2,18"    # DUMP(x, 16)
ora-num 194,2,24                  # decimal byte list
ora-num "194 2 24"
ora-num C20218                    # RAWTOHEX / contiguous hex
ora-num 0xC20218
ora-num "c2 02 18"                # hex-dump style
ora-num "col 0: [ 3] c2 02 18"    # partial block dump
```

With `--format auto`, block dump input is always read as hex. Other input is
treated as hex if it is a single contiguous token, contains hex letters, or is
space-separated two-character pairs; otherwise it is read as decimal bytes. Use `-f dec` or `-f hex` to override
the detection.

### Examples by format

#### `DUMP(x)` output (decimal bytes)

Paste the result of `SELECT DUMP(x) FROM ...` as is; the `Typ=2 Len=n:`
prefix is stripped.

```sh
$ ora-num "Typ=2 Len=5: 194,13,35,57,79"
1234.5678

$ ora-num "Typ=2 Len=4: 61,100,78,102"
-123
```

#### `DUMP(x, 16)` output (hex bytes)

```sh
$ ora-num "Typ=2 Len=5: c2,d,23,39,4f"
1234.5678

$ ora-num "Typ=2 Len=3: 3f,33,66"
-0.5
```

#### Decimal byte list

Bytes separated by commas or whitespace.

```sh
$ ora-num 194,13,35,57,79
1234.5678

$ ora-num "192 51"
0.5
```

#### Contiguous hex (`RAWTOHEX`, `UTL_RAW`)

An optional `0x` prefix is allowed, and hex digits may be upper or lower case.

```sh
$ ora-num C20D23394F
1234.5678

$ ora-num 0xbf02
0.0001
```

#### Hex-dump style

Space-separated two-character hex pairs, as copied from a hex editor or a
block dump.

```sh
$ ora-num "3d 64 4e 66"
-123

$ ora-num "c3 0b"
100000
```

#### Partial block dump

A column entry from a block dump (`ALTER SYSTEM DUMP DATAFILE ...`): `col`,
the column number, `:`, the byte count in brackets, then the bytes in hex.
The bytes are always read as hex, and their number must match the count in
brackets.

```sh
$ ora-num "col 0: [ 2] c1 04"
3

$ ora-num "col  1: [ 5]  c2 0d 23 39 4f"
1234.5678

$ ora-num "col 0: [ 3] c1 04"
error: block dump declares 3 bytes, but 2 are given

$ ora-num "col 1: *NULL*"
error: column 1 is NULL in the block dump, there is no number to convert
```

#### Forcing the format

Some inputs are valid in both formats. `"62 99"` looks like a hex dump, so
`auto` reads it as hex, but `0x99` is not a valid digit byte:

```sh
$ ora-num "62 99"
error: byte 1 (value 153) is not a valid base-100 digit

$ ora-num -f dec "62 99"
-2
```

### Verbose output

```sh
$ ora-num -v C20218
bytes:    C2 02 18
sign:     +
exponent: 1 (base 100)
digits:   [1, 23]
value:    123
```

Infinity is printed the way Oracle displays it: `~` for +infinity (`FF 65`)
and `-~` for -infinity (`00`).

## Reverse conversion

With `-r`, the input is a decimal number and the output is the internal
representation, as contiguous hex by default (usable with `HEXTORAW()` or as
input to `ora-num` again) or as a decimal byte list with `-f dec`:

```sh
$ ora-num -r 123
C20218

$ ora-num -r -f dec -1234.5678
61,89,67,45,23,102

$ ora-num -r 1.5e-7
BD10
```

Accepted input: an optional sign, digits with an optional decimal point
(`.5` and `5.` are fine), an optional exponent (`1.5e-7`, `1E+3`), and `~` /
`-~` for infinity. Quote `"~"` so the shell does not expand it. Negative
numbers can be given directly, without `--`.

With `-v`, the components and the `DUMP()` form are printed as well:

```sh
$ ora-num -r -v -1234.5678
value:    -1234.5678
sign:     -
exponent: 1 (base 100)
digits:   [12, 34, 56, 78]
dump:     Typ=2 Len=6: 61,89,67,45,23,102
bytes:    3D59432D1766
```

An Oracle `NUMBER` holds at most 20 base-100 digits (up to 40 significant
decimal digits). Longer input is rounded half away from zero, like Oracle
does, and a note is printed to stderr:

```sh
$ ora-num -r 0.66666666666666666666666666666666666666666666
note: rounded to 0.6666666666666666666666666666666666666667 (20 base-100 digits)
C04343434343434343434343434343434343434344
```

Numbers of magnitude 1e126 or more, and non-zero numbers below 1e-130, are
rejected:

```sh
$ ora-num -r 1e126
error: number too large, an Oracle NUMBER must be below 1e126 in magnitude
```

Oracle itself raises an overflow error for the first case but silently stores
zero for the second; `ora-num` reports both as errors.

## Encoding

An Oracle `NUMBER` is stored as one sign/exponent byte followed by up to 20
base-100 digits (most significant first), 22 bytes at most.

| Value     | Exponent byte                | Digit byte          | Notes                           |
|-----------|------------------------------|---------------------|---------------------------------|
| zero      | `0x80`                       | —                   | single byte                     |
| positive  | `≥ 0x80`, exponent = b − 193 | digit = b − 1       | `80 02` is 1e-130 (smallest)    |
| negative  | `< 0x80`, exponent = 62 − b  | digit = 101 − b     | optionally terminated by `0x66` |
| +infinity | `0xFF`                       | `0x65`              |                                 |
| −infinity | `0x00`                       | —                   | single byte                     |

The value is `sign × Σ digitᵢ × 100^(exponent − i)`.

`0x80` and `0x00` only mean zero and −infinity when they are the only byte;
followed by digits they are the smallest (−65) and largest (62) exponent. The
representable range is therefore 1e-130 up to just below 1e126 in magnitude,
with at most 40 significant decimal digits.

## Testing

```sh
cargo test
```

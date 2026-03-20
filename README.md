SGInstrument
===

This is a utility enabling the use of the
[SGFuzz](https://github.com/bajinsheng/SGFuzz/tree/master) against Rust
applications. The library simply wraps the C instrumentation function. The
`sginstrument` executable traverses the target code and inserts instrumentation
where enums are used. This is equivalent to `State_machine_instrument.py`
from SGFuzz.

No attempt is made to maintain the formatting of the code — if you
need the output to be readable run `cargo fmt` after `sginstrument`.

## Instrumented patterns

The tool detects and instruments the following enum usage sites:

- **Let bindings**: `let status = Status::Active;`
- **Assignments**: `status = Status::Inactive;`
- **Function call arguments**: `process(Status::Active)`
- **Method call arguments**: `handler.process(Status::Active)`
- **Match arms**: `match s { Status::Active => ... }`
- **If-let expressions**: `if let Status::Active = s { ... }`
- **While-let expressions**: `while let Item::Value(v) = iter.next() { ... }`

Const functions, `const` items, and `static` items are excluded from
instrumentation since they cannot contain runtime calls.

## Getting started

1. Build and install [SGFuzz](https://github.com/bajinsheng/SGFuzz/tree/master)
2. Navigate to your target project and `cargo add sginstrument`
3. `cargo install sginstrument`
4. `sginstrument src/`
5. Build your harness against SGFuzz and profit

## Usage

```
sginstrument [OPTIONS] <path-to-rust-files>
```

### Options

| Flag | Description |
|------|-------------|
| `--dry-run` | Preview instrumented output without modifying files |
| `--backup` | Create `.rs.bak` files before overwriting source |
| `-h`, `--help` | Show help message |

### Examples

Preview what changes would be made:

```sh
sginstrument --dry-run src/
```

Instrument with backups:

```sh
sginstrument --backup src/
```

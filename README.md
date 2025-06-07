SGInstrument
===

This is a utility enabling the use of the
[SGFuzz](https://github.com/bajinsheng/SGFuzz/tree/master) against Rust
applications. The library simply wraps the C instrumentation function. The
`sginstrument` executable traverse the target code and inserts instrumentation
where enums are assigned. This is equivalent to `State_machine_instrument.py`
from SGFuzz.

No attempt is made to maintain the formatting of the code if you
need the output to be readable run `cargo fmt` after `sginstrument`.

# Getting started

1. Build and install [SGFuzz](https://github.com/bajinsheng/SGFuzz/tree/master)
2. Navigate to your target project and `cargo add sginstrument`
3. `cargo install sginstrument`
4. `sginstrument target/src/`
5. build your harness against SGFuzz and profit

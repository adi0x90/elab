# elab

`elab` runs YAML experiments against embedded targets through GDB/MI and UART, then saves the results as JSONL. It keeps one debug session open across phases so captures, memory writes, and observations belong to the same experiment.

Use it to repeat a debugging procedure, compare baseline and modified behavior, or collect memory snapshots alongside serial output. The optional probe-rs backend supports a smaller set of operations.

## Install

You need Rust and Cargo to build the tool. Hardware experiments also need a compatible GDB executable, a running debug server such as OpenOCD, and access to the target's debug interface or serial port.

```sh
git clone https://github.com/adi0x90/elab.git
cd elab
cargo build --locked --release -p elab-cli
cargo install --locked --path crates/elab-cli
```

The build produces `target/release/elab`. Installation puts `elab` in Cargo's binary directory, which must be on your `PATH`.

For GDB experiments, the default executable is `gdb-multiarch`. Set `ELAB_GDB` to a different executable if needed. The GDB backend uses hardware breakpoints and OpenOCD-style `monitor halt` and `monitor reset` commands; other servers may need adaptation.

To build with the experimental probe-rs backend:

```sh
cargo build --locked --release -p elab-cli --features probe
```

## Commands

```text
elab run <SPEC> [--runs DIR]   execute phases and save evidence
elab verify <RUN_DIR>         check recorded expectations without hardware
elab devices <DIR>            list TOML device profiles
elab ops                     show the operation and profile reference
elab <COMMAND> --help         show command help
```

The files in `devices/` and `experiments/` are templates. They do not include firmware or establish compatibility with a particular board. Edit the serial port, ELF path, addresses, and expressions for your target before running them.

```sh
elab devices devices
# After adapting the template to your target:
elab run experiments/capture.yaml
# Use the directory printed by the run command:
elab verify runs/capture-<timestamp>
```

The `device` path is relative to the YAML file. ELF paths are relative to the directory where you run `elab`. Evidence goes under `runs/` in that directory unless you pass `--runs`.

## Device profiles

A TOML profile names the target's stops and defines optional image checks. The following addresses are placeholders:

```toml
name = "example"
endian = "little"

[image]
# Set elf_sha256 to the full SHA-256 of your ELF.
# Add verify entries at code addresses to compare live memory with the ELF.
# elf_sha256 = "<64-character SHA-256>"
# verify = [{ addr = 0x08000100, n = 16 }]

[stops]
checkpoint = { addr = 0x08000100 }
```

A debug experiment checks the configured ELF hash, code ranges, and optional model hash before executing its phases. These checks are optional: an empty `[image]` section does not verify target identity. A serial-only experiment does not perform image checks. Probe experiments perform them only when an ELF is supplied.

For sampled outputs, add a `[published]` section with `x` and `count` expressions, plus one `[[channels]]` entry per measured value. See `elab ops` and [DESIGN.md](DESIGN.md).

## Experiments

```yaml
name: capture
device: ../devices/example.toml
transport:
  gdb:
    endpoint: "127.0.0.1:3333"
    elf: "firmware.elf"
phases:
  - name: baseline
    ops:
      - { op: stop, at: checkpoint }
      - { op: capture, name: pc, expr: "$pc" }
```

Operations run in order. `${name}` substitutes a captured value into an expression; GDB registers such as `$sp` remain unchanged. Stop names come from the device profile. Memory operations can use numeric addresses or GDB expressions.

| Operation | Purpose |
|---|---|
| `stop` | Run to a named hardware breakpoint and check the program counter. |
| `capture` | Evaluate an expression and save its value. |
| `write` | Save the old bytes, write a typed value, and check the readback. |
| `published` | Find a matching sample at a named stop and capture its channels. |
| `expect` | Compare fields from the latest published row with expected numbers. |
| `serial_expect` | Wait for a literal substring in serial output. |
| `serial_send` | Send text over the serial connection. |
| `reset` | Reset the target, optionally leaving it halted. |
| `power_cycle` | Execute the device profile's `[power] cycle` argument list. |
| `attachment` | Save a memory range as a binary file with a recorded SHA-256. |

Writes support `f32`, `f64`, `i8`, `u8`, `u32`, and `i32`. The profile's `endian` setting controls byte order. Use values within the selected type's range; integer conversions currently narrow or saturate rather than rejecting every out-of-range value.

## Recorded results

Each run creates a directory containing:

```text
runs/<name>-<timestamp>/
  events.jsonl
  manifest.json
  attachments/    # when an attachment operation is used
```

Events record phases, stops, captures, writes, observations, and expectations. Successful execution writes a manifest with the experiment and device profile hashes, any supplied ELF hash, published rows, and assertion failures. Execution errors after evidence creation produce a failed manifest with the error message; setup errors can occur before a run directory exists.

`elab verify` recomputes numeric expectations using the same absolute tolerance as the runner (`1e-6`), checks recorded expectation flags and event count, and requires the manifest's `ok` field to be true. It does not authenticate the record or rehash attachments, firmware, or source configuration. Treat it as a consistency check, not proof that the hardware behaved as recorded.

## Target state and limitations

Run only specifications and profiles you trust. GDB expressions can modify the target, and `[power] cycle` executes a local command. Profiles and specifications are not sandboxed. Use simple names without path separators for experiments and attachments.

On an execution error, the runner attempts to restore saved writes in reverse order. Restoration is best effort, and errors during restoration are currently ignored. Assertion failures are recorded while later phases continue, allowing an explicit recovery phase to run. Successful execution leaves the target in the state set by the final phase. Include recovery operations when you need them; resets, power cycles, peripheral writes, and external effects are not reversible through a byte restore.

The CLI attempts to detach after phase execution. Setup failures and interrupted processes can leave debug state behind, and GDB waits have no overall deadline. Check the target and debug server after an interrupted run.

When serial is configured, the CLI opens it before the debug transport and waits 1.5 seconds. Some USB debug adapters reset the target when their serial port opens. `serial_expect` consumes output through each match and retains trailing bytes for the next expectation; buffered output can predate a later reset or write, so a match alone does not establish causality.

The probe-rs backend is experimental. It accepts numeric expressions and `$pc`, but not symbols, register expressions other than `$pc`, or conditional stops. Target and probe support depend on the pinned probe-rs version. The project does not include a flash-programming command, trace capture, or RTT support. No physical target is validated by the software tests below.

## Development

```sh
cargo test --locked -p elab-cli -p elab-core -p elab-gdb -p elab-serial
cargo check --locked -p elab-cli --features probe
cargo fmt --all --check
```

Tests cover value encoding, substitutions, mock target execution, write restoration, evidence verification, and serial matching through a local pseudo-terminal on Unix. They do not exercise a physical debug probe.

The workspace contains `elab-core` (profiles, experiments, evidence), `elab-gdb` (GDB/MI), `elab-serial` (UART), `elab-probe` (probe-rs), and `elab-cli` (commands).

## License

[MIT](LICENSE).

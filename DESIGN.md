# Architecture

The CLI loads an experiment and its device profile, opens the configured transports, checks image identity when configured, and passes the phases to `elab-core`.

```text
experiment.yaml + device.toml
             |
          elab-cli
             |
         elab-core --------> events.jsonl + manifest.json
          /      \
     Machine    SerialLink
      /   \         |
    GDB  probe-rs  UART
```

`Machine` provides stops, expression evaluation, memory access, reset, and detach. `SerialLink` provides send and expect. The core runner uses these traits so its behavior can be tested without a board. The backends do not have identical capabilities: GDB handles symbols and expressions, while probe-rs currently supports numeric literals and `$pc`.

## Execution

An experiment is an ordered list of named phases. Each phase contains operations; phase names have no special behavior. Baseline, change, and recovery are conventions that an experiment author can choose.

Captured values are available to later operations through `${name}` substitution. Writes save the original bytes before attempting the mutation and check the resulting bytes. An execution error triggers best-effort restoration in reverse order. An expectation mismatch records a failure and permits subsequent operations to run. The caller receives published rows, failures, and the overall assertion result.

The runner does not implement transactions. Restoring bytes cannot undo peripheral side effects or a reset, and restoration can fail when the debug connection is lost. See the target-state limitations in [README.md](README.md).

## Published samples

A profile can define a sampled output:

```toml
[published]
x = "stats.input"
count = "stats.count"

[[channels]]
name = "output"
value = "stats.output"
```

The `published` operation runs to a named stop until `x` matches the requested value within `tol` (default `1e-4`), up to `tries` stops (default 30). It then reads the counter and all channels. It does not enforce counter freshness. An `expect` operation compares fields of the most recent published row using an absolute tolerance of `1e-6`; that row can belong to an earlier phase if the experiment has not captured another one.

## Image checks

The profile may specify an ELF SHA-256, live-memory code ranges, and a memory region with a model SHA-256. The ELF reader supports ELF32 and ELF64 through the `object` crate. Configure verification ranges inside file-backed code segments; zero-filled memory is not an appropriate code verification range.

Checks are only as strong as the supplied profile. An empty image section skips identity verification. They also do not prevent the target from changing after attachment.

## Transports

The GDB backend starts one MI session and uses temporary hardware breakpoints. Its parser covers the records used by this tool, rather than the entire MI grammar. It currently blocks while waiting for responses or a target stop; the remote timeout setting is not an overall experiment deadline. The stop and reset paths use OpenOCD monitor commands.

The serial backend reads bytes until a literal UTF-8 pattern matches. It consumes bytes through the match and keeps the suffix. It does not timestamp individual incoming bytes or discard output automatically on reset, and the receive buffer has no size limit.

The probe-rs backend attaches to core zero and has a timeout for halt operations. Conditional stops and symbol evaluation are unsupported. It is optional in the CLI so the default installation does not need probe-rs.

## Evidence

`Evidence` appends timestamped JSON events and writes the final manifest. Full successful-execution manifests contain hashes of the specification, profile, and supplied ELF. Error manifests contain the error and an event count. Attachment events record a file path, byte length, and SHA-256.

Verification checks numeric expectations, their recorded flags, the event count, and the final manifest status. The files are editable and unsigned. Attachment and input-file hashes are recorded for inspection but are not independently checked by `elab verify`.

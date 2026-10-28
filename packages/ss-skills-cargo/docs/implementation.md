# Cargo inventory implementation

The parser uses `toml_edit`'s immutable document and source spans. Its TOML
1.1 parser accepts multiline inline tables as well as arrays, so source
locations need no separate TOML scanner. Dotted keys are grouped using the
parser's table representation rather than adjacency in the file.

Member patterns are traversed by directory and pattern position. Open
filesystem identity handles identify visited directories and members,
avoiding both platform-based case assumptions and recursive symlink cycles.
Canonical paths are used for presentation, not as identity keys. Traversal
limits bound retained state and pending work; exceeding a limit or encountering
a directory or manifest error fails the inventory rather than returning an
incomplete result. Results are ordered deterministically by relative manifest
path and source position.

The caller supplies the scan time and handles output publication. No global
clock, process, network, or file-writing facility is needed by this library.
The inventory error retains typed rejection causes and the underlying TOML or
filesystem error. Manifest and operation context identifies the failed input.

## Parser fuzzing

The `fuzz/` package runs the same in-process parser used for manifests through
a bounded libFuzzer target. Input size is capped by the target, and the
target verifies that every emitted raw declaration belongs to its reported
source line. It ignores inputs that do not parse as UTF-8 or valid manifests.
The checked-in seeds cover declaration styles and malformed input. The fuzzer
maintains its generated corpus between runs.

With `cargo-fuzz` already available, run from `packages/ss-skills-cargo`:

```text
mkdir -p fuzz/corpus/manifest
cargo fuzz run manifest fuzz/corpus/manifest fuzz/seeds -- -max_total_time=60 -max_len=16384
```

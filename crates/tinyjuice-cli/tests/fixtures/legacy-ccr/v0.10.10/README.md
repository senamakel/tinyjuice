# TinyJuice v0.10.10 disk-tier CCR fixture

This file was produced by the public CCR writer in released TinyJuice `v0.10.10`,
not reconstructed from the current implementation. The tag resolves to
`4db24354d0e883f6a0aefe96e5cc0c76073fd76e`. Its root `Cargo.toml` and
`Cargo.lock` hashes are respectively
`689e3b78858fbf65a33b8b56e01134a49698bb835f00c2f0b5e05b93e6a9eb95` and
`62b38e108654d0b4b97009c6cd6cd412e954a5d9dc46bedc24b1767f3a51ba4c`.

The archived writer was injected with `examples/write_ccr_fixture.rs`
(`ab050f31a019797b5e5d05aa33ad315a61b5ffa6ec3b3b465644f1caaa5ee2bc`). It
calls `tinyjuice::cache::{configure, configure_disk_cap, enable_disk_tier,
offload_checked}`. The recorded TinyBus and TinyAgents gitlinks were restored
at `207a9a7df15e44fdba00a807d0f7eaf21904ba0e` and
`aa5476fa48092d9bf051fbd2d1bf07f14cc9f331`; neither archived manifest nor
lockfile was changed. Reproduce from the native repository root with:

```sh
mkdir -p target/legacy-writer-v0.10.10
git archive --format=tar 4db24354d0e883f6a0aefe96e5cc0c76073fd76e | tar -x -C target/legacy-writer-v0.10.10
mkdir -p target/legacy-writer-v0.10.10/vendor/tinybus target/legacy-writer-v0.10.10/vendor/tinyagents target/legacy-writer-v0.10.10/examples target/legacy-writer-v0.10.10/legacy-output target/legacy-source-cache/tinybus
git -C target/legacy-source-cache/tinybus init
git -C target/legacy-source-cache/tinybus fetch --depth=1 https://github.com/tinyhumansai/tinybus.git 207a9a7df15e44fdba00a807d0f7eaf21904ba0e
git -C target/legacy-source-cache/tinybus checkout --detach FETCH_HEAD
git -C target/legacy-source-cache/tinybus archive 207a9a7df15e44fdba00a807d0f7eaf21904ba0e | tar -x -C target/legacy-writer-v0.10.10/vendor/tinybus
git -C vendor/tinyagents archive aa5476fa48092d9bf051fbd2d1bf07f14cc9f331 | tar -x -C target/legacy-writer-v0.10.10/vendor/tinyagents
cp crates/tinyjuice-cli/tests/fixtures/legacy-ccr/v0.10.10/write_ccr_fixture.rs target/legacy-writer-v0.10.10/examples/
cargo run --locked --manifest-path target/legacy-writer-v0.10.10/Cargo.toml -p tinyjuice --example write_ccr_fixture -- target/legacy-writer-v0.10.10/legacy-output "Fixture written by TinyJuice v0.10.10 public disk-tier API."
```

The writer emitted token `1e66b4cff1835002102d821ffee64f75`. The checked-in
raw body is 59 bytes with SHA-256
`1e66b4cff1835002102d821ffee64f751d1c89efe286027d815bfdd8c25827b5`. Its timestamp is set by the import test
on a temporary copy so the test can check preserved semantic age without
changing the writer-produced bytes.

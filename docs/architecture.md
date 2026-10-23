# Architecture

TinyJuice is organized around two public layers:

1. a small `Compressor` trait scaffold for simple strategy boundaries
2. the TokenJuice content router for real tool-output compaction

The router detects content kind, chooses a specialized compressor, declines when
compression is unsafe or not smaller, and stores exact originals in CCR when a
lossy view is emitted.

The core crate remains independent of OpenHuman runtime internals. OpenHuman and
other hosts install policy through `CompressOptions`, agent profiles, CCR
configuration, optional ML callbacks, and optional savings recorders.

### CCR durable tier

`tinyjuice::cache::set_durable_tier(Some(store))` installs a process-global
`CcrStore` behind the in-memory CCR cache (`None` clears it; each call replaces
the previous tier). The crate has no storage dependency: a host builds the store
on whatever backend it has. `tinyjuice-module` uses the host storage bus
(`tinyjuice-drivers::BlobCcrStore`, expiring blobs).

- Offloads mirror into the tier synchronously, best-effort. The tier counts as
  retaining an original only when it reports success under the content hash
  that callers advertise as the recovery token.
- A memory miss in `retrieve` / `retrieve_range` falls through to the tier.
- The library has no file-backed persistence tier. The separate
  `tinyjuice-cli` package composes the file backend for standalone cache
  commands.
- The CLI stores the original CCR write time in a sibling metadata blob because
  the BlobStore port has no caller-supplied timestamp. TTLs use the remaining
  relative duration when the content and metadata blobs are written separately,
  so their backend expiration times can differ by a small write-time skew;
  collection ordering and age checks use the preserved original timestamp.
- Store calls may block, so a host must not call them from a single-threaded
  runtime that also drives the store's transport.

TinyJuice should prefer preventing redundant context from entering the model
over lossy mid-task prompt compaction. Tool-output reducers, command rules,
batched operations, validation reports, and explicit recovery metadata are the
primary integration points.

## Main Technical Docs

- [Wiki home](../wiki/Home.md)
- [Capabilities](../wiki/Capabilities.md)
- [Router and Compressors](../wiki/Router-and-Compressors.md)
- [Rule Engine](../wiki/Rule-Engine.md)
- [CCR Recovery](../wiki/CCR-Recovery.md)
- [OpenHuman Integration](../wiki/OpenHuman-Integration.md)

## Design References

- [Reference specs](references/README.md)

# tinyjuice-bus

The TinyBus wire contract for [TinyJuice](https://github.com/tinyhumansai/tinyjuice):
the interface names, request and response types, contract version, and storage
resources that a host may need to grant.

A host loads `tinyjuice-module` as a dynamic library and cannot import Rust
items from it. This crate supplies the call vocabulary instead. It depends on
the data-only `tinystoragedrivers-bus` schema for storage needs, alongside
`serde`, JSON Schema declarations, and error derives. The storage dependency
disables its optional TinyBus client, so this contract does not pull in TinyBus,
an async runtime, or compression code.

The values here are **moved out of** the `tinyjuice` library rather than copied
from it: `tinyjuice::types` re-exports them, so there is one definition of each
and a host is looking at the same bytes the module validates against.

`STORAGE_NEEDS` is TinyJuice's canonical host-grant metadata. Its CCR blob
prefix includes the trailing slash (`tinyjuice_ccr/`), matching the existing
`tinyjuice_ccr/<token>` keys while excluding sibling prefixes such as
`tinyjuice_ccr_backup/`. Hosts can consume this declaration instead of
restating its resources.

The isolated consumer under `tests/storage-needs-consumer/` pins this metadata
dependency in its own lockfile and verifies that a host needing only this
contract does not acquire TinyBus. When changing the TinyJuice workspace
version or the pinned `tinystoragedrivers-bus` revision, refresh the consumer
lockfile in the same change; `scripts/test-features.sh` checks the locked
consumer and its dependency graph.


Contract 1.2 adds `Query(QueryRequest)` and `ExtractHtml(content)`. `Query`
accepts either a CCR handle or supplied content that the host has authorized,
plus a typed REPL operation, limits, and an optional summary callback ticket.
The response distinguishes expired handles and operation errors from transport
failures. `Repl(handle, op_json)` retains its existing two-argument form.

`repl` owns the serialized operation/result vocabulary; `tools` owns the stock
REPL and retrieval declarations and summary-focus schema. The implementation
crate re-exports these definitions for library compatibility. The summary
callback prompt and unavailable notices are shared in `summary`.


See [the module specification](../../docs/specs/tinybus-module.md) for request
examples, operation tags/defaults, reply shapes, and the fixed input/output
ceilings. HTML extraction returns `Result<String, HtmlError>` so input-size
rejections remain distinct from transport failures.

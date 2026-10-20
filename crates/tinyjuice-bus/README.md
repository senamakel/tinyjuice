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

`STORAGE_NEEDS` is TinyJuice's data-only host-grant metadata. It declares the
`juice.savings` collection and `juice/ccr/` blob prefix from the same canonical
names used by the executable schema in `tinyjuice-drivers`. This crate keeps
only those names, the serialized DTOs, and the static needs declaration. Its
empty `storage` feature remains for compatibility and adds no TSD core
dependency. The legacy `tinyjuice_ccr/` prefix remains a migration source; it
is not granted to the module after migration.

The released TSD v1.8.0 runner reads a source blob and its absolute expiry
together, then writes the transformed blob with that same deadline. The
`BlobOutput` callback remains `(key, content type, bytes)`; expiry preservation
belongs to the runner, not the TinyJuice transform. The bus contract stays
data-only and pins source commit `595723e1b1cabeb3d36f0c5b6ff2e11b458e7558`.
That v1.8.0 source was released from the `senamakel/tinystoragedrivers` fork;
Cargo retains the canonical repository URL and resolves the immutable commit.

The isolated consumer under `tests/storage-needs-consumer/` directly pins the
same canonical-URL `tinystoragedrivers-bus` v1.8.0 source revision as the
workspace. Its return type names that crate directly, so compilation proves
the re-export is the same Rust type; its test checks all four resource fields. The locked
consumer also proves the resolved source and that a host needing only this
contract does not acquire TinyBus or enable the optional storage client.
When changing the TinyJuice workspace version or storage-bus revision, refresh
the consumer lockfile in the same change; `scripts/test-features.sh` checks the
locked consumer and its dependency graph.


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


Contract 3.0 replaces the host-supplied `diskTierRoot` install setting with an
explicit `ccrDurable` capability. This changes the meaning of old host requests,
so the module rejects contract 2.x hosts rather than silently ignoring their
persistence request.

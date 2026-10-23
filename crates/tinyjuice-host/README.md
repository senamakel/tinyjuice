# tinyjuice-host

`tinyjuice-host` contains reusable host adapters for the compiled TinyJuice
module. It depends on `tinyjuice-bus` for declarations and wire payloads, and
on `tinytools` for agent-tool adapters. It does not depend on the TinyJuice
implementation crate or TinyBus runtime.

Hosts provide the query and retrieval callbacks. `QuerySource` receives the
original optional `ToolRunContext` with each `QueryRequest`, allowing the host
to bind the request to its own runtime and policy. `repl_tools` uses current
bus declarations; `repl_tools_with_declarations` accepts frozen declarations
for resumed sessions. Both take an optional artifact directory, an explicit
maximum artifact size, and an optional Generate ticket token.

`retrieve_tool::RetrieveToolOutputTool` accepts a callback that receives the
token and optional byte or line range. `generate` provides a synchronous ticket
registry: a callback is consumed once, and dropping its ticket withdraws an
unused callback. `focus` and `recovery` expose schema and pass-through helpers
for hosts that compose their own tool surface.

The adapter reads artifact files only below the injected tool-result artifact
directory. Hosts remain responsible for deciding which directory is
authorized.

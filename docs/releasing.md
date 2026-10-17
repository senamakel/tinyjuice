# Releasing TinyJuice

Pushes of `v*` tags and manual runs using the default `main` release mode use
the existing TinyJuice module release workflow. For a reviewed-epic release,
manually run the Release workflow on the reviewed source branch, select
`reviewed-epic`, provide the existing `vX.Y.Z` tag, and enter the positive
canonical pull request number whose merge commit the tag names. The shared
reviewed-epic admission workflow checks that PR and tag before building or
publishing. TinyJuice's package profile remains fixed in this caller.
The reviewed caller pins the shared profile that emits `tinyjuice-module`
archives, matching the crate stem used by the platform build scripts. The
reviewed route accepts only the tag, its derived version, and the reviewed PR;
it does not accept caller-selected package or asset names.

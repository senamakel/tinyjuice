//! The `TinyJuice` `TinyBus` contract version and its binding rule.

/// The wire contract version this crate defines.
/// Version 3 removes the host-provided disk path and replaces it with an
/// explicit host-durability capability. Version 2 hosts must be rejected so
/// their old `diskTierRoot` request cannot silently lose its persistence.
pub const CONTRACT_VERSION: (u32, u32) = (3, 0);

/// Returns whether a host using [`CONTRACT_VERSION`] can bind to `module`.
///
/// The major versions must match, and the module must have at least the host's
/// minor version so it serves every member the host may call.
#[must_use]
pub fn is_compatible(module: (u32, u32)) -> bool {
    module.0 == CONTRACT_VERSION.0 && module.1 >= CONTRACT_VERSION.1
}

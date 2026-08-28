//! macOS snapshot backend: `sysinfo`-based collector plus the pure
//! conversion layer that adapts its values to the `procfs` snapshot types.

#[cfg(any(test, target_os = "macos"))]
pub mod convert;

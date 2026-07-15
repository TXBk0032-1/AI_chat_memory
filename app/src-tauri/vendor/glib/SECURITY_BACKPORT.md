# Security backport

This is the crates.io source for `glib` 0.18.5, vendored because Tauri's GTK3
stack requires the 0.18 API series.

It includes the upstream fix for RUSTSEC-2024-0429 from gtk-rs-core commit
`b5a4071e439bef2b5eea76c3aa25e5ae84839e34`. The backport changes the
`VariantStrIter::impl_get` out-argument from an immutable pointer reference to
a mutable pointer reference.

Remove this override when Tauri's Linux stack supports `glib` 0.20 or later.

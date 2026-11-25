# Local Media Foundation buffer-lock fix

This directory contains `nokhwa-bindings-windows` 0.4.6 from the locally pinned
Cargo registry package. Its original source, copyright notice, Apache-2.0
license declaration, README, and VCS information are retained.

- Upstream: https://github.com/l1npengtul/nokhwa
- Upstream commit: `fa5a19208ade2b22113bfaa48f20dfdd3758b751`
- Crate: https://crates.io/crates/nokhwa-bindings-windows/0.4.6
- Original crate checksum:
  `5be28886bad8abcec3655c1f24b965b4cb596a72b23164c910c54439ce55d2a4`

The workspace patch adds a borrowed RAII guard immediately after a successful
`IMFMediaBuffer::Lock` in `raw_bytes`. The guard calls `Unlock` once before the
COM buffer is released, including the null-pointer and zero-length error paths
and unwinding. A failed `Lock` returns before the guard is constructed and does
not call `Unlock`. No capture format, timing, device-control, or inference
behavior is otherwise changed. The original pinned implementation had no
`Unlock` call after its sole buffer `Lock`.

This corrects the source-level lock imbalance. It does not establish that the
imbalance caused any particular observed hardware timeout; device validation is
separate. The global Cargo registry cache is unchanged.

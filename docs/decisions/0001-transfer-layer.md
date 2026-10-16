# 0001: Transfer layer — own layer on bao-tree, not iroh-blobs

Date: 2026-10-16. Status: decided (Day 1).

## Question
Use iroh-blobs for verified, resumable transfers, or write a small layer over iroh
connections with the same BLAKE3 verified streaming?

## Findings
- **iroh-blobs 0.103.1**: its README still says "not yet considered production quality …
  use iroh-blobs 0.35". 0.35 targets pre-1.0 iroh, so it does not fit iroh 1.3.
- iroh-blobs is built around its own store (`FsStore`, redb database). The sender imports
  files into it, and the receiver downloads into it and then **exports**:
  - `ExportMode::Copy` (the default) writes every file twice, so a 10 GB file needs 20 GB.
  - `ExportMode::TryReference` avoids the copy only on the same filesystem, and the store
    may ignore it.
  - On Android, the receive folder is a Storage Access Framework tree. The store cannot
    live there, so every file would be written once into app storage and again through
    SAF, the slow path the plan measures on Day 2.
- iroh-blobs pulls in irpc, redb and roughly 30 direct dependencies, which weigh on the
  Android build.
- **bao-tree 0.16.1** is the verified-streaming crate iroh-blobs itself uses. It encodes
  and decodes BLAKE3 verified streams over any byte stream, for any chunk ranges, and
  checks every chunk group against the root hash as it arrives. The root hash is the
  ordinary BLAKE3 hash of the file, so the manifest fingerprint and the stream agree.

## Decision
Own layer (`crates/glft-core/src/transfer.rs`), about 300 lines:
- one control stream for offer, answer and done;
- one stream per file, where the receiver sends `{index, from_chunk}` and the sender
  answers with `encode_ranges_validated` output;
- the receiver decodes with `ResponseDecoder` and writes verified leaves straight into
  `<receive folder>/.glft-partial/<hash>`, then renames into place: one write, same
  filesystem;
- 16 KiB chunk groups (the iroh-blobs default), with 8 files in flight.

`from_chunk` is the hook for resume (Day 3): the receiver records verified ranges and asks
only for the rest. bao-tree already supports arbitrary range sets if that is needed.

## Measurements (Inspiron 15 3530, loopback, release build)
| Test | Result |
|---|---|
| 1 folder, 10 files, 417 MB (one 300 MB file) | 1.7 s, 250 MB/s, `diff -r` identical |
| 5000 files × 2 KB, one at a time | 20.0 s (≈4 ms per file: round trip + fsync) |
| 5000 files × 2 KB, 8 in flight | 3.2 s, identical |
| Discovery by mDNS only (no `--addr`) | found and connected in under 1 s |

## Costs accepted
- We maintain the wire protocol ourselves. It is small and versioned by ALPN `glft/1`.
- No content-addressed deduplication across transfers; not a goal.

## Also settled
- mDNS: `iroh-mdns-address-lookup` 0.6 (n0, MIT/Apache-2.0), with service name `glft`.
  `iroh-mdns-peer-lookup` is not used, so its license question is moot.
- iroh 1.3 is built without default features: no portmapper (no UPnP or NAT-PMP requests
  to the router) and no metrics; `tls-ring` only.

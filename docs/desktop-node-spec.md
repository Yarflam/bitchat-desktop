# Desktop Node Spec (`bitchatd`)

Status: draft, architecture-only. No protocol changes are introduced by this
document; it maps the existing wire protocol onto a desktop (Rust) node.

## 1. Goal

A headless desktop node that participates in an existing BitChat mesh with the
same features as a phone peer:

- **Mesh participation**: announce, discover, connect, relay (TTL-gated).
- **Chat over the mesh**: broadcast and private messages via Noise-encrypted
  sessions.
- **Discovery of nearby meshes**: BLE scanning/advertising, interoperable with
  the Android GATT profile.
- **Persistence bonus**: always-on store-and-forward and gossip sync
  (`REQUEST_SYNC`), which a phone cannot do reliably.

The node MUST interop with unmodified Android (and iOS) clients. It is a new
*client implementation*, not a protocol fork.

## 2. Normative references (implementation is the source of truth)

| Concern | File |
|---|---|
| Packet framing, flags, sections | `app/src/main/java/com/bitchat/android/protocol/BinaryProtocol.kt` |
| Selective padding of encoded packets | `app/src/main/java/com/bitchat/android/protocol/MessagePadding.kt` |
| Compression ("only if smaller", ratio bomb guard) | `app/src/main/java/com/bitchat/android/protocol/CompressionUtil.kt` |
| Golden wire vectors | `app/src/test/kotlin/com/bitchat/android/contracts/ClientRewriteWireContractTest.kt` |
| ANNOUNCE TLVs, gossip, capabilities | `docs/ANNOUNCEMENT_GOSSIP.md` |
| Noise handshake/transport, rekey limits | `app/src/main/java/com/bitchat/android/noise/NoiseSession.kt`, `AppConstants.Noise` |
| Noise interop vectors (Cacophony) | `app/src/test/kotlin/com/bitchat/android/noise/NoiseExternalVectorTest.kt` |
| BLE GATT profile, advertisement | `app/src/main/java/com/bitchat/android/util/AppConstants.kt` (`Mesh.Gatt`), `BluetoothGatt{Server,Client}Manager.kt` |
| BLE padding policy | `app/src/main/java/com/bitchat/android/mesh/BLEPacketPaddingPolicy.kt` |
| Fragmentation limits and header | `app/src/main/java/com/bitchat/android/mesh/FragmentManager.kt`, `AppConstants.Fragmentation` |
| Relay/TTL policy | `app/src/main/java/com/bitchat/android/mesh/PacketRelayManager.kt` |

If this document disagrees with the code, the code wins and this document gets
fixed. Where behavior is ambiguous, add a golden vector test on the Kotlin
side first, then port it.

## 3. Wire format (must reproduce byte-for-byte)

### 3.1 Packet header

- Version (1 byte): `0x01` or `0x02`; any other version ⇒ reject.
- Type (1 byte): see message types below.
- TTL (1 byte): decrement-on-relay; stop relaying at 0. Neighbor-only sync
  packets use TTL 0.
- Timestamp (8 bytes, unsigned big-endian, epoch millis).
- Flags (1 byte): `0x01` hasRecipient, `0x02` hasSignature, `0x04` isCompressed,
  `0x08` hasRoute (v2 only; ignored for v1).
- PayloadLength: 2 bytes big-endian (v1) / 4 bytes (v2), covering payload plus
  the original-size field when compressed. Reject above
  `Protocol.MAX_PAYLOAD_LENGTH = 10_485_760`.

Section order after the header (all multi-byte integers big-endian):

1. SenderID, exactly 8 bytes (zero-padded).
2. RecipientID, 8 bytes, if hasRecipient.
3. Route, v2+ only: 1-byte hop count (≤ 255) then N × 8-byte peer IDs, if
   hasRoute. Empty route list decodes as absent (canonical form).
4. Payload; if compressed, original size first (2 bytes v1 / 4 bytes v2), then
   raw-DEFLATE bytes.
5. Signature, 64 bytes, if hasSignature.

### 3.2 Signing

- Ed25519, key from ANNOUNCE TLV `0x03`.
- Signature input: re-encoded packet **with the signature omitted and TTL
  normalized to 0**. TTL changes during relay MUST NOT invalidate signatures.
- Signatures are verified before identity or capability state is applied.

### 3.3 Compression

- DEFLATE (raw, no zlib header).
- Senders compress only when the compressed result is smaller; receivers MUST
  accept both forms.
- Re-encode note: a decoded packet reuses the originator's compressed bytes so
  a relay does not substitute its own encoding. A Rust relay MUST preserve the
  wire payload bytes (compressed or not) when decrementing TTL and forwarding.
- Bomb guard: reject declared expanded sizes ≤ 0, > `MAX_PAYLOAD_LENGTH`, or
  with a compression ratio above 50 000:1 (exact values in `BinaryProtocol`).

### 3.4 Message types

`0x01` ANNOUNCE, `0x02` MESSAGE, `0x03` LEAVE, `0x10` NOISE_HANDSHAKE,
`0x11` NOISE_ENCRYPTED, `0x20` FRAGMENT, `0x21` REQUEST_SYNC,
`0x22` FILE_TRANSFER, `0x29` VOICE_FRAME (ephemeral, never gossiped).
Unknown types ⇒ ignore packet, keep connection alive.

## 4. Identity and Noise

- Peer ID: 8-byte value; hex form is 16 lowercase chars (the 8 bytes in
  order). Same truncation/padding rule as `hexStringToByteArray`.
- ANNOUNCE payload: ordered TLVs (`type:1, length:1, value:length`):
  - `0x01` nickname (UTF-8, ≤ 255 bytes)
  - `0x02` Noise static public key (X25519, 32 bytes)
  - `0x03` Ed25519 signing public key (32 bytes)
  - `0x04` direct neighbors gossip (optional, ≤ 10 × 8 bytes)
  - `0x05` capabilities (optional, little-endian bitfield; advertise only bits
    the node implements)
  - Unknown TLVs MUST be ignored; malformed required TLVs ⇒ reject announce.
- Handshake: `Noise_XX_25519_ChaChaPoly_SHA256`, no prologue. Handshake
  payloads are always encrypted, so the on-wire messages are 32 / 96 / 64
  bytes (messages 2 and 3 include the 16-byte MAC of the empty payload —
  the vendored noise-java writer calls `encryptAndHash(..., 0)` even for a
  null payload). Interop is pinned by the Cacophony vector in
  `NoiseExternalVectorTest` — ported as `desktop/crates/bitchat-wire/tests/noise_vector.rs`.
- Transport data: NoisePayload envelope (`type:1, payload:n`) inside
  NOISE_ENCRYPTED. Rekey limits per `AppConstants.Noise` (1 h / 1000 msgs
  service policy, 10 000 session ceiling, 256-byte max Noise payload).

## 5. BLE transport mapping (primary, replaces Wi-Fi Aware)

The node implements the same GATT profile as Android:

- Service UUID: `F47B5E2D-4A9E-4C5A-9B3F-8E1D2C3A4B5C`
- Characteristic UUID: `A1B2C3D4-E5F6-4A5B-8C9D-0E1F2A3B4C5D`
  (write + notify, CCCD `00002902-0000-1000-8000-00805f9b34fb`)
- Advertisement: service UUID in the service list plus service data =
  8-byte peer ID, so scanners can filter without connecting.
- Roles: the node acts as GATT server (accepts phone connections) and GATT
  client (connects out to phones), like `UnifiedMeshService` does.
- Padding: apply `MessagePadding` to NOISE_HANDSHAKE and NOISE_ENCRYPTED
  frames only; all other packet types go out unpadded
  (`BLEPacketPaddingPolicy`). Padding is stripped before decode; decode MUST
  also succeed on unpadded input.
- Fragmentation (BLE writes are MTU-bound):
  - Packets above `Fragmentation.FRAGMENT_SIZE_THRESHOLD = 512` bytes are
    fragmented; max fragment payload `MAX_FRAGMENT_SIZE = 469`.
  - Fragment header is exactly 13 bytes: fragmentID(8) || index(2) ||
    total(2) || originalType(1), followed by data.
  - Limits: ≤ 256 fragments per ID, ≤ 1 MiB total per message,
    ≤ 64 active fragment sets, ≤ 4 MiB global inflight. Reassembly timeout
    30 s. These caps are security limits, not tuning knobs.

### 5.1 Platform caveats (btleplug)

- **Linux (BlueZ)**: full central + peripheral support. Reference platform.
- **Windows / macOS**: peripheral (GATT server) support is limited or
  unreliable in btleplug. On those platforms the node degrades to a GATT
  client (it can discover and connect to phones, phones cannot connect to it).
  Documented degradation, not a protocol violation — the phone side initiates
  in most flows today, but Mesh Lab must confirm bidirectional connect on
  Linux before claiming parity.
- MTU negotiation: request the largest ATT MTU the OS offers; chunk writes to
  `MTU - 3`. Never assume a value larger than what was negotiated.

## 6. Discovery of nearby meshes

- Passive: BLE scan filtered on the service UUID; read peer ID from service
  data; treat as candidate peer. This matches Android scanning behavior.
- Active: after connecting, exchange signed ANNOUNCE (gossip TLV `0x04` gives
  a topology view of the neighborhood).
- Gossip sync (REQUEST_SYNC `0x21`): the node participates in the deployed
  GCS-based announcement gossip (`GCSFilter.kt`, `GossipSyncManager.kt`):
  deterministic packet IDs (`SHA-256(type|sender|timestamp|payload)[..16]`),
  Golomb-Rice filters (P from FPR 1%, M = N·2^P), sync requests neighbor-only
  (TTL 0) every 30 s plus one targeted request 5 s after a peer's first
  verified announce, and responses relay the originator-signed packets the
  requester is missing (announcements first, 20 ms pacing). Relay of ANNOUNCE
  and broadcast MESSAGE packets decrements TTL after a validPacket gate and
  a 10 000-entry seen cache, mirroring `PacketProcessor`.
- Optional LAN extension (not required for v1): mDNS advertisement carrying
  the same service UUID + peer ID, TCP transport carrying the same framed
  packets. Wi-Fi Aware is NOT available on desktop hardware and is replaced,
  not emulated.

## 7. Rust crate mapping

| Component | Crate |
|---|---|
| Async runtime, TCP/UDP (future LAN) | `tokio` |
| BLE central + peripheral | `btleplug` |
| Noise XX 25519/ChaChaPoly/SHA256 | `snow` |
| Ed25519 | `ed25519-dalek` |
| X25519 (announce keys) | `x25519-dalek` |
| Raw DEFLATE | `flate2` (miniz backend) |
| mDNS (optional) | `mdns-sd` |
| CLI/config | `clap`, `serde` |

`flate2` and `java.util.zip.Deflater` must be cross-checked with a byte
vector: DEFLATE output is not canonical across encoders, so the Rust side is
only required to *decode* Android bytes correctly and to *encode* something
Android decodes; byte-identical compression is NOT required (mirrors the
WirePayload rationale).

## 8. Compatibility and testing strategy

1. **Golden vectors first**: port `ClientRewriteWireContractTest` to Rust as
   `tests/wire_contract.rs`, asserting the same literal bytes. This is the
   definition of "compatible client".
2. **Noise vector**: port `NoiseExternalVectorTest` (Cacophony) against `snow`.
3. **Compression round-trip**: vectors generated by the Kotlin side (raw
   deflate of fixed inputs) that the Rust side must decompress to the exact
   bytes; and vice versa.
4. **Fragmentation vectors**: the 13-byte header and reassembly limits,
   including every truncation/rejection case.
5. **Fail-closed parity**: for every `return null` / `Log.w` rejection in
   `BinaryProtocol.decodeCore`, the Rust decoder has a matching reject test.
6. **Mesh Lab**: physical validation (phone ↔ PC node over BLE) per
   `docs/release-gate-runbook.md` before claiming parity, on Linux first.

## 9. Phases

1. **P0 wire core**: packet codec, TLVs, padding, fragmentation, Ed25519
   verify/sign. All golden vectors green.
2. **P1 Noise**: XX handshake + transport, rekey, payload envelope.
3. **P2 BLE central** (works everywhere): scan, connect, write/notify, phone
   sees node as a normal peer.
4. **P3 GATT server + relay + gossip sync**: node relays TTL-gated packets,
   answers `REQUEST_SYNC`, keeps store-and-forward.
5. **P4 LAN/mDNS extension and ops hardening**: config, logs, uptime handling.

## 9.1 No telemetry (hard requirement)

The desktop node ships no analytics, crash reporters, phone-home calls, or
third-party network dependencies. `bitchat-wire` is pure codec logic with zero
network I/O; its dependency tree contains only crypto and compression crates
(verified with `cargo tree`: no HTTP/socket stack anywhere). The only network
surfaces of the node are the mesh transports themselves (BLE, optional LAN
mDNS/TCP) and explicitly user-configured outbound connections (Tor). Anything
that would transmit node state off-device requires a spec change and explicit
review.

## 10. Out of scope (v1)

- UI (headless daemon; management over local CLI/JSON-RPC only).
- Geohash presence, voice, private media transfer.
- Wi-Fi Aware or any attempt to emulate it.
- Any protocol change. If the desktop node needs a new capability bit, it
  follows the spec change process: spec update in `docs/`, golden vectors,
  Mesh Lab, fail-closed review.

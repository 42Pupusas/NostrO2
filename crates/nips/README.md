# nostro2-nips

NIP implementations for [NostrO2](https://github.com/42Pupusas/NostrO2), built
on the crate's own spec-compliant primitives — no third-party `nostr` stack.

| NIP | Module | What it gives you |
|-----|--------|-------------------|
| 04 / 44 | `nip_44` | Versioned encrypted payloads; NIP-44 v2 (ChaCha20 + HMAC-SHA256), vector-gated |
| 17 | `nip_17` | Private direct messages |
| 46 | `nip_46` | Nostr Connect (remote signing) request/response |
| 59 | `nip_59` | Gift wrap (seal + rumor) |
| 104 | `nip_104`, `nip_104_invite`, `nip_104_manager` | Double Ratchet E2EE DMs — see below |

Schnorr signing goes through the `NostrSigner` trait, ECDH through
`NostrKeypair`, verification through `NostrNote::verify` — so the crate links no
curve of its own. Pick one via features: `k256` (default) or `secp256k1`.

## NIP-104 — Double Ratchet

A native, dependency-light port of
[`mmalmi/nostr-double-ratchet`](https://github.com/mmalmi/nostr-double-ratchet)
(the implementation `chat.iris.to` runs). Every primitive is byte-identical to
the reference, so sessions established here interoperate with Iris. The stack
is five layers:

1. **`nip_44` — NIP-44 v2 primitives.** The symmetric/DH foundation. Gated
   against the official test vectors.
2. **`nip_104::Session` — the 1:1 ratchet.** `new_initiator` / `new_responder`,
   then `plan_send` / `plan_receive` (which return the next state for atomic
   persistence) and `apply` to commit. Handles chain stepping and
   skipped-message keys (`MAX_SKIP`). Cross-checked against the reference at the
   crypto layer.
3. **`nip_104` codec — kind:1060 wire events.** `plan_send_event` /
   `plan_receive_event` and `MessageEnvelope::to_event` / `from_event`. Events
   are signed by the sender's *current ephemeral* key; the ciphertext is the
   `content` and the encrypted header rides in a `["header", …]` tag.
   Cross-checked against the reference at the event layer.
4. **`nip_104_invite::Invite` — session bootstrap.** Mint an invite
   (`create_new`), share it (`to_url` / `to_event`), `accept` it (invitee →
   initiator session + signed kind:1059 response), and `receive` the response
   (inviter → mirror responder session). The response is triple-encrypted —
   inner DH authenticates the invitee, the shared-secret layer proves link
   possession, the envelope hides the invitee from other link holders.
5. **`nip_104_manager::SessionManager` — multi-device fan-out.** Tracks many
   sessions keyed by `(peer, device)`, grouping a peer's devices under their
   owner pubkey. Routes inbound events to whichever session decrypts them and
   fans a `send` out to every device. Side-effect free: methods hand back the
   events to publish (or the decrypted message), leaving transport, storage and
   scheduling to you.

### End-to-end: bootstrap from an invite, then chat

```rust,ignore
use nostro2_nips::{Invite, SessionManager};

// Each side owns a long-term identity keypair `K: NostrKeypair`.
let mut alice = SessionManager::new(alice_identity);
let mut bob   = SessionManager::new(bob_identity);

// Alice mints an invite and shares the URL (she keeps the ephemeral secret).
let invite = Invite::create_new::<K>(alice.our_pubkey(), None)?;
let url = invite.to_url("https://chat.iris.to");

// Bob scans it, accepts, and publishes the response event.
let bob_side = Invite::from_url(&url)?;
let response = bob.accept_invite(&bob_side, None, now)?; // publish `response`

// Alice consumes the response and now shares a session with Bob.
let peer = alice.receive_invite_response(&invite, &response)?;

// Bob (the initiator) sends first; Alice routes the inbound event home.
for event in bob.send(alice.our_pubkey(), b"hi alice", now)? {
    // publish `event`; on Alice's side:
    if let Some(msg) = alice.process_event(&event) {
        assert_eq!(msg.plaintext, b"hi alice");
    }
}

// One send fans out to every device the peer runs.
for event in alice.send(&peer, b"hi bob", now)? {
    // publish each `event`
}
```

For a single session without the manager, drive `Session` directly:

```rust,ignore
let (next, envelope) = session.plan_send(b"hello")?;
session.apply(next); // commit only after you've persisted/published
```

### Scope

Layers 1–5 are the portable, interop-critical core. Deliberately left to the
application: persistent storage, live relay wiring, `AppKeys` device
authorization, read receipts, and disappearing-message expiration — all runtime
concerns the `SessionManager` is designed to sit beneath.

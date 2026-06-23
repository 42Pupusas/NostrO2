//! NIP-104 — Invite layer (session bootstrap).
//!
//! Note: `inviter` / `invitee` differ by two letters by nature; renaming them
//! for `clippy::similar_names` would only hurt readability against the
//! reference implementation, so it is allowed module-wide below.
//!
//! The double ratchet in [`crate::nip_104`] establishes *forward-secret*
//! messaging **once both sides share a session**. This module is the
//! key-exchange that gets them there: a native port of the reference
//! `Invite` / `inviteUtils` from
//! [`mmalmi/nostr-double-ratchet`](https://github.com/mmalmi/nostr-double-ratchet)
//! (what `chat.iris.to` runs).
//!
//! ## The handshake
//!
//! An **inviter** publishes (or QR/URL-shares) an [`Invite`]: an ephemeral
//! pubkey plus a random 32-byte *shared secret* (the "link"). An **invitee**
//! scans it and calls [`Invite::accept`], producing
//!
//! 1. a fresh [`Session`](crate::nip_104::Session) (initiator side), and
//! 2. a signed **invite-response** event (kind
//!    [`INVITE_RESPONSE_KIND`]) to publish back.
//!
//! The inviter consumes that event with [`Invite::receive`] to build the
//! mirror-image responder session. From there, [`crate::nip_104`] takes over.
//!
//! ## Why three encryption layers
//!
//! The response wraps the invitee's session key three times, exactly as the
//! reference does — each layer defends a distinct threat:
//!
//! | Layer | Key | Purpose |
//! |-------|-----|---------|
//! | inner DH | ECDH(invitee-id, inviter-id) | authenticates the invitee to the inviter |
//! | shared-secret | the link secret (raw conv-key) | proves possession of the invite |
//! | envelope | ECDH(random, inviter-ephemeral) | hides the invitee from anyone else holding the link |
//!
//! Forward secrecy survives compromise of *either* long-term identity key
//! *and* the link, as long as the per-session keys stay secret.
#![allow(clippy::similar_names)]

use super::{
    decode_hex_32, decrypt_with_message_key, encrypt_with_message_key, Nip104Error, Session,
};
use crate::Nip44;
use nostro2_traits::{hex::Hexable as _, NostrKeypair, SignerError};

type Result<T> = std::result::Result<T, Nip104Error>;

/// Nostr event kind for a published invite (parameterized replaceable),
/// matching the reference `INVITE_EVENT_KIND`.
pub const INVITE_EVENT_KIND: u32 = 30078;

/// Nostr event kind for an invite *response* (the gift-wrapped acceptance),
/// matching the reference `INVITE_RESPONSE_KIND`.
pub const INVITE_RESPONSE_KIND: u32 = 1059;

bourne::json! {
    /// Inner authenticated payload: the invitee's session pubkey plus, for
    /// multi-device users, their owner/identity pubkey. camelCase to match
    /// the reference JSON byte-for-byte.
    #[derive(Debug, Clone, PartialEq, Eq)]
    struct AcceptPayload {
        #[bourne(rename = "sessionKey")]
        session_key: String,
        #[bourne(rename = "ownerPublicKey")]
        #[bourne(skip_if_none)]
        owner_public_key: Option<String>,
    }
}

bourne::json! {
    /// The unsigned inner event carried inside the envelope. Mirrors the
    /// reference's `{ pubkey, content, created_at }` shape — `pubkey` is the
    /// invitee's identity key (which doubles as their device id).
    #[derive(Debug, Clone, PartialEq, Eq)]
    struct InnerEvent {
        pubkey: String,
        content: String,
        created_at: i64,
    }
}

/// What [`Invite::receive`] recovers from an accepted invite.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InviteResponse {
    /// The invitee's identity public key (also their device id).
    pub invitee_identity: String,
    /// The invitee's session public key (the ratchet's first DH key).
    pub invitee_session_pubkey: String,
    /// The invitee's owner/Nostr identity pubkey, when supplied.
    pub owner_public_key: Option<String>,
}

/// A double-ratchet invite: the inviter's ephemeral pubkey and the shared
/// "link" secret. The ephemeral *secret* is present only on the inviter's own
/// copy (it is needed to receive responses).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Invite {
    /// Inviter's ephemeral public key (x-only hex).
    pub inviter_ephemeral_pubkey: String,
    /// 32-byte shared secret / link (hex).
    pub shared_secret: String,
    /// Inviter's identity public key (x-only hex).
    pub inviter: String,
    /// Inviter's ephemeral secret key (hex) — present only on the inviter side.
    pub inviter_ephemeral_privkey: Option<String>,
    /// Optional device id (the `d`-tag suffix on a published invite).
    pub device_id: Option<String>,
}

impl Invite {
    /// Mint a fresh invite for `inviter` (their identity pubkey). Generates a
    /// new ephemeral keypair and a random 32-byte shared secret. The returned
    /// invite holds the ephemeral secret, so keep it private — share only
    /// [`Self::to_url`] / [`Self::to_event`] output.
    ///
    /// # Errors
    /// Propagates keypair construction failure.
    pub fn create_new<K: NostrKeypair>(inviter: &str, device_id: Option<&str>) -> Result<Self> {
        let ephemeral = K::generate();
        let mut secret = [0_u8; 32];
        getrandom::fill(&mut secret)
            .map_err(|e| Nip104Error::Signer(SignerError::Backend(format!("getrandom: {e}"))))?;
        Ok(Self {
            inviter_ephemeral_pubkey: ephemeral.public_key(),
            shared_secret: secret.to_hex(),
            inviter: inviter.to_owned(),
            inviter_ephemeral_privkey: Some(ephemeral.secret_key()),
            device_id: device_id.map(str::to_owned),
        })
    }

    /// Render the shareable URL. Invite parameters live in the URL **hash**
    /// (`#…`) so they never reach the server — matching the reference's
    /// `getUrl`. `root` is the app origin, e.g. `https://chat.iris.to`.
    #[must_use]
    pub fn to_url(&self, root: &str) -> String {
        // Minimal JSON object, field order matching the reference.
        let json = format!(
            r#"{{"inviter":"{}","ephemeralKey":"{}","sharedSecret":"{}"}}"#,
            self.inviter, self.inviter_ephemeral_pubkey, self.shared_secret
        );
        format!("{root}#{}", urlencode(&json))
    }

    /// Parse an invite from a shared URL (data in the `#` hash).
    ///
    /// # Errors
    /// [`Nip104Error::InvalidInvite`] if the hash is missing, not valid JSON,
    /// or missing a required field.
    pub fn from_url(url: &str) -> Result<Self> {
        let hash = url
            .split_once('#')
            .map(|(_, h)| h)
            .filter(|h| !h.is_empty())
            .ok_or_else(|| Nip104Error::InvalidInvite("no invite data in URL hash".into()))?;
        let decoded = urldecode(hash);
        let inviter = json_str_field(&decoded, "inviter")
            .ok_or_else(|| Nip104Error::InvalidInvite("missing inviter".into()))?;
        // The reference accepts either `ephemeralKey` or the older
        // `inviterEphemeralPublicKey`.
        let ephemeral = json_str_field(&decoded, "ephemeralKey")
            .or_else(|| json_str_field(&decoded, "inviterEphemeralPublicKey"))
            .ok_or_else(|| Nip104Error::InvalidInvite("missing ephemeralKey".into()))?;
        let shared = json_str_field(&decoded, "sharedSecret")
            .ok_or_else(|| Nip104Error::InvalidInvite("missing sharedSecret".into()))?;
        Ok(Self {
            inviter_ephemeral_pubkey: ephemeral,
            shared_secret: shared,
            inviter,
            inviter_ephemeral_privkey: None,
            device_id: None,
        })
    }

    /// Build the published invite event (kind [`INVITE_EVENT_KIND`]). The
    /// caller signs it with the inviter's identity key.
    ///
    /// # Errors
    /// [`Nip104Error::InvalidInvite`] if no `device_id` is set.
    pub fn to_event(&self, created_at: i64) -> Result<nostro2::NostrNote> {
        let device_id = self
            .device_id
            .as_deref()
            .ok_or_else(|| Nip104Error::InvalidInvite("device id required".into()))?;
        let mut tags = nostro2::NostrTags::new();
        tags.add_custom_tag("ephemeralKey", &self.inviter_ephemeral_pubkey);
        tags.add_custom_tag("sharedSecret", &self.shared_secret);
        tags.add_custom_tag("d", &format!("double-ratchet/invites/{device_id}"));
        tags.add_custom_tag("l", "double-ratchet/invites");
        Ok(nostro2::NostrNote {
            kind: INVITE_EVENT_KIND,
            pubkey: self.inviter.clone(),
            content: String::new(),
            created_at,
            tags,
            ..Default::default()
        })
    }

    /// Parse and verify a published invite event (kind [`INVITE_EVENT_KIND`]).
    /// The resulting invite has no ephemeral secret (receive-side use only via
    /// the inviter's own copy).
    ///
    /// # Errors
    /// [`Nip104Error::InvalidInvite`] on wrong kind, bad signature, or missing
    /// tags.
    pub fn from_event(event: &nostro2::NostrNote) -> Result<Self> {
        use nostro2::NostrEvent as _;
        if event.kind != INVITE_EVENT_KIND {
            return Err(Nip104Error::InvalidInvite("wrong kind".into()));
        }
        if !event.verify() {
            return Err(Nip104Error::InvalidInvite("bad signature".into()));
        }
        let ephemeral = first_tag(event, "ephemeralKey")
            .ok_or_else(|| Nip104Error::InvalidInvite("missing ephemeralKey".into()))?;
        let shared = first_tag(event, "sharedSecret")
            .ok_or_else(|| Nip104Error::InvalidInvite("missing sharedSecret".into()))?;
        // device id is the third segment of `double-ratchet/invites/<id>`.
        let device_id = first_tag(event, "d")
            .and_then(|d| d.split('/').nth(2).map(str::to_owned))
            .filter(|id| id != "public");
        Ok(Self {
            inviter_ephemeral_pubkey: ephemeral,
            shared_secret: shared,
            inviter: event.pubkey.clone(),
            inviter_ephemeral_privkey: None,
            device_id,
        })
    }

    /// **Invitee side.** Accept this invite: create the initiator session and
    /// the signed response event to publish back to the inviter.
    ///
    /// `invitee` is the invitee's *identity* keypair (used to authenticate via
    /// the inner DH layer and as the inner event's `pubkey`). `owner_pubkey`
    /// is the optional multi-device owner key. `created_at` stamps the events.
    ///
    /// Returns the new [`Session`] and a signed kind-[`INVITE_RESPONSE_KIND`]
    /// event. Publish the event; keep (and [`apply`](Session::apply)-drive)
    /// the session.
    ///
    /// # Errors
    /// Propagates key, NIP-44, and signing failures.
    pub fn accept<K: NostrKeypair>(
        &self,
        invitee: &K,
        owner_pubkey: Option<&str>,
        created_at: i64,
    ) -> Result<(Session<K>, nostro2::NostrNote)> {
        let shared_secret = decode_hex_32(&self.shared_secret)?;
        let their_ephemeral = decode_hex_32(&self.inviter_ephemeral_pubkey)?;

        // Fresh per-session keypair; its secret seeds the initiator ratchet.
        let session_kp = K::generate();
        let session = Session::<K>::new_initiator(
            &their_ephemeral,
            &session_kp.secret_bytes(),
            &shared_secret,
        )?;

        // Layer 1 (inner DH): authenticate the invitee to the inviter.
        let payload = AcceptPayload {
            session_key: session_kp.public_key(),
            owner_public_key: owner_pubkey.map(str::to_owned),
        };
        let payload_json = bourne::to_string(&payload)?;
        let dh_encrypted = invitee.nip_44_encrypt(&payload_json, &self.inviter)?.into_owned();

        // Layer 2 (shared secret): prove possession of the link.
        let inner_content = encrypt_with_message_key::<K>(&shared_secret, dh_encrypted.as_bytes())?;
        let inner_event = InnerEvent {
            pubkey: invitee.public_key(),
            content: inner_content,
            created_at,
        };
        let inner_json = bourne::to_string(&inner_event)?;

        // Layer 3 (envelope): hide the invitee behind a random sender.
        let random_sender = K::generate();
        let envelope_content = random_sender
            .nip_44_encrypt(&inner_json, &self.inviter_ephemeral_pubkey)?
            .into_owned();
        let mut tags = nostro2::NostrTags::new();
        tags.add_pubkey_tag(&self.inviter_ephemeral_pubkey, None);
        let mut envelope = nostro2::NostrNote {
            kind: INVITE_RESPONSE_KIND,
            content: envelope_content,
            created_at,
            tags,
            ..Default::default()
        };
        envelope
            .sign_with(&random_sender)
            .map_err(|_| Nip104Error::Signer(SignerError::InvalidSignature))?;

        Ok((session, envelope))
    }

    /// **Inviter side.** Consume an accepted invite-response event and build
    /// the mirror responder session.
    ///
    /// `event` is the kind-[`INVITE_RESPONSE_KIND`] envelope the invitee
    /// published. `inviter_identity` is the inviter's *identity* keypair (for
    /// the inner DH layer). This invite must carry its ephemeral secret
    /// ([`create_new`](Self::create_new) provides it).
    ///
    /// Returns the responder [`Session`] and the decoded [`InviteResponse`]
    /// (whose `invitee_identity` names the peer).
    ///
    /// # Errors
    /// [`Nip104Error::InvalidInvite`] on wrong kind / bad signature / missing
    /// ephemeral secret, plus any crypto failure.
    pub fn receive<K: NostrKeypair>(
        &self,
        event: &nostro2::NostrNote,
        inviter_identity: &K,
    ) -> Result<(Session<K>, InviteResponse)> {
        use nostro2::NostrEvent as _;
        if event.kind != INVITE_RESPONSE_KIND {
            return Err(Nip104Error::InvalidInvite("wrong kind".into()));
        }
        if !event.verify() {
            return Err(Nip104Error::InvalidInvite("bad signature".into()));
        }
        let ephemeral_sk = self
            .inviter_ephemeral_privkey
            .as_deref()
            .ok_or_else(|| Nip104Error::InvalidInvite("ephemeral secret unavailable".into()))?;
        let ephemeral_kp = K::from_secret_bytes(&decode_hex_32(ephemeral_sk)?)?;
        let shared_secret = decode_hex_32(&self.shared_secret)?;

        // Peel layer 3: ephemeral × random-sender.
        let inner_json = ephemeral_kp.nip_44_decrypt(&event.content, &event.pubkey)?;
        let inner_event: InnerEvent = bourne::parse_str(&inner_json)?;
        let invitee_identity = inner_event.pubkey;

        // Peel layer 2: the raw shared secret.
        let dh_encrypted_bytes =
            decrypt_with_message_key::<K>(&shared_secret, &inner_event.content)?;
        let dh_encrypted = String::from_utf8(dh_encrypted_bytes)
            .map_err(|e| Nip104Error::Json(format!("inner utf8: {e}")))?;

        // Peel layer 1: inviter-id × invitee-id DH.
        let payload_json = inviter_identity.nip_44_decrypt(&dh_encrypted, &invitee_identity)?;
        let payload: AcceptPayload = bourne::parse_str(&payload_json)?;

        let their_session = decode_hex_32(&payload.session_key)?;
        let session =
            Session::<K>::new_responder(&their_session, &ephemeral_kp.secret_bytes(), &shared_secret)?;

        Ok((
            session,
            InviteResponse {
                invitee_identity,
                invitee_session_pubkey: payload.session_key,
                owner_public_key: payload.owner_public_key,
            },
        ))
    }
}

// ── Helpers ───────────────────────────────────────────────────────────

fn first_tag(event: &nostro2::NostrNote, name: &str) -> Option<String> {
    event
        .tags
        .iter()
        .find(|row| row.first().is_some_and(|t| t == name))
        .and_then(|row| row.get(1).cloned())
}

/// Extract a top-level string field from a flat JSON object by key. Adequate
/// for the tiny, well-formed invite hash payloads (not a general parser).
fn json_str_field(json: &str, key: &str) -> Option<String> {
    let needle = format!("\"{key}\"");
    let start = json.find(&needle)? + needle.len();
    let rest = &json[start..];
    let colon = rest.find(':')?;
    let after = &rest[colon + 1..];
    let q1 = after.find('"')? + 1;
    let q2 = after[q1..].find('"')?;
    Some(after[q1..q1 + q2].to_owned())
}

/// Percent-encode the characters the invite hash JSON can contain that are
/// unsafe in a URL fragment. Conservative but sufficient (and `urldecode`
/// inverts it).
fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            other => {
                const HEX: &[u8; 16] = b"0123456789ABCDEF";
                out.push('%');
                out.push(HEX[(other >> 4) as usize] as char);
                out.push(HEX[(other & 0xf) as usize] as char);
            }
        }
    }
    out
}

fn urldecode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(byte) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(byte);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    // Concrete-typed `public_key()` calls below need the trait in scope; the
    // library code reaches it through the `NostrKeypair: NostrSigner` bound.
    use nostro2_traits::NostrSigner as _;

    type K = crate::tests::NipTester;

    fn ident(seed: u8) -> K {
        K::from_secret_bytes(&[seed; 32]).unwrap()
    }

    #[test]
    fn full_invite_handshake_bootstraps_a_session() {
        let inviter_id = ident(0xA1);
        let invitee_id = ident(0xB2);

        // Inviter mints an invite (holds the ephemeral secret).
        let invite = Invite::create_new::<K>(&inviter_id.public_key(), Some("dev1")).unwrap();
        assert!(invite.inviter_ephemeral_privkey.is_some());

        // Invitee accepts -> initiator session + response event.
        let (mut invitee_session, response) =
            invite.accept::<K>(&invitee_id, None, 1_700_000_000).unwrap();
        assert_eq!(response.kind, INVITE_RESPONSE_KIND);

        // Inviter receives -> responder session + invitee identity.
        let (mut inviter_session, recovered) =
            invite.receive::<K>(&response, &inviter_id).unwrap();
        assert_eq!(recovered.invitee_identity, invitee_id.public_key());

        // The two sessions must now actually talk. Invitee (initiator) sends first.
        let (s1, env) = invitee_session.plan_send(b"hello inviter").unwrap();
        invitee_session.apply(s1);
        let (s2, pt) = inviter_session.plan_receive(&env).unwrap();
        inviter_session.apply(s2);
        assert_eq!(pt, b"hello inviter");

        // And the reply direction works too.
        let (s3, env2) = inviter_session.plan_send(b"hello invitee").unwrap();
        inviter_session.apply(s3);
        let (s4, pt2) = invitee_session.plan_receive(&env2).unwrap();
        invitee_session.apply(s4);
        assert_eq!(pt2, b"hello invitee");
    }

    #[test]
    fn owner_pubkey_round_trips() {
        let inviter_id = ident(0x11);
        let invitee_id = ident(0x22);
        let owner = ident(0x33).public_key();

        let invite = Invite::create_new::<K>(&inviter_id.public_key(), None).unwrap();
        let (_s, response) = invite
            .accept::<K>(&invitee_id, Some(&owner), 1_700_000_000)
            .unwrap();
        let (_session, recovered) = invite.receive::<K>(&response, &inviter_id).unwrap();
        assert_eq!(recovered.owner_public_key.as_deref(), Some(owner.as_str()));
    }

    #[test]
    fn wrong_identity_key_cannot_receive() {
        let inviter_id = ident(0x44);
        let invitee_id = ident(0x55);
        let impostor = ident(0x66);

        let invite = Invite::create_new::<K>(&inviter_id.public_key(), None).unwrap();
        let (_s, response) = invite.accept::<K>(&invitee_id, None, 1_700_000_000).unwrap();

        // Same ephemeral secret (so the envelope opens), but the wrong identity
        // key fails the inner DH layer.
        assert!(invite.receive::<K>(&response, &impostor).is_err());
    }

    #[test]
    fn invite_event_round_trips() {
        let inviter_id = ident(0x77);
        let invite = Invite::create_new::<K>(&inviter_id.public_key(), Some("dev9")).unwrap();
        let mut event = invite.to_event(1_700_000_000).unwrap();
        event.sign_with(&inviter_id).unwrap();

        let parsed = Invite::from_event(&event).unwrap();
        assert_eq!(parsed.inviter_ephemeral_pubkey, invite.inviter_ephemeral_pubkey);
        assert_eq!(parsed.shared_secret, invite.shared_secret);
        assert_eq!(parsed.inviter, inviter_id.public_key());
        assert_eq!(parsed.device_id.as_deref(), Some("dev9"));
    }

    #[test]
    fn url_round_trips() {
        let inviter_id = ident(0x88);
        let invite = Invite::create_new::<K>(&inviter_id.public_key(), None).unwrap();
        let url = invite.to_url("https://chat.iris.to");
        let parsed = Invite::from_url(&url).unwrap();
        assert_eq!(parsed.inviter, invite.inviter);
        assert_eq!(parsed.inviter_ephemeral_pubkey, invite.inviter_ephemeral_pubkey);
        assert_eq!(parsed.shared_secret, invite.shared_secret);
    }

    #[test]
    fn tampered_envelope_rejected() {
        let inviter_id = ident(0x99);
        let invitee_id = ident(0xAA);
        let invite = Invite::create_new::<K>(&inviter_id.public_key(), None).unwrap();
        let (_s, mut response) = invite.accept::<K>(&invitee_id, None, 1_700_000_000).unwrap();
        response.content.push('A'); // breaks the signature
        assert!(invite.receive::<K>(&response, &inviter_id).is_err());
    }
}

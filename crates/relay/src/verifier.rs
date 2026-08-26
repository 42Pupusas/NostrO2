//! Signature checking on the inbound path.
//!
//! A relay is untrusted. It can invent a note, attribute it to any pubkey,
//! and send it down a subscription the application asked for. Only the
//! Schnorr signature over the canonical event id proves authorship, so
//! [`NoteVerifier`] checks every inbound note before the driver publishes it.
//!
//! The check needs a curve, which is a cargo feature. Without one the
//! verifier still exists and still has a policy, but it cannot inspect a
//! signature, so it admits every note. Enable `k256` or `secp256k1` to make
//! the check real.

/// What to do with a note whose signature does not check out.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum VerifyPolicy {
    /// Drop the note and log it. A forged note never reaches the reader.
    #[default]
    Reject,
    /// Publish every note without looking at its signature.
    ///
    /// For a trusted relay on a loopback socket, or for measuring the cost
    /// of verification itself.
    Trust,
}

/// The verdict on one inbound note.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// The note may reach the application.
    Admit,
    /// The note is forged or corrupt, and the driver must drop it.
    Reject,
}

impl Verdict {
    /// Whether the note may reach the application.
    #[must_use]
    pub const fn is_admit(self) -> bool {
        matches!(self, Self::Admit)
    }
}

/// Applies a [`VerifyPolicy`] to inbound relay messages.
///
/// Only [`NostrRelayEvent::NewNote`] carries a signature. Every other frame
/// is relay bookkeeping with nothing to verify, so it passes through.
///
/// [`NostrRelayEvent::NewNote`]: nostro2::NostrRelayEvent::NewNote
#[derive(Debug, Clone, Copy, Default)]
pub struct NoteVerifier {
    policy: VerifyPolicy,
}

impl NoteVerifier {
    /// A verifier that rejects notes which fail the check.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            policy: VerifyPolicy::Reject,
        }
    }

    /// A verifier with an explicit policy.
    #[must_use]
    pub const fn with_policy(policy: VerifyPolicy) -> Self {
        Self { policy }
    }

    /// The policy this verifier applies.
    #[must_use]
    pub const fn policy(&self) -> VerifyPolicy {
        self.policy
    }

    /// Whether this build can actually inspect a signature.
    ///
    /// Returns `false` when no curve feature is enabled, in which case
    /// [`Self::judge`] admits everything.
    #[must_use]
    pub const fn is_enforcing(&self) -> bool {
        matches!(self.policy, VerifyPolicy::Reject) && Self::HAS_CURVE
    }

    #[cfg(any(feature = "k256", feature = "secp256k1"))]
    const HAS_CURVE: bool = true;
    #[cfg(not(any(feature = "k256", feature = "secp256k1")))]
    const HAS_CURVE: bool = false;

    /// Decides whether one relay message may reach the application.
    #[must_use]
    pub fn judge(&self, event: &nostro2::NostrRelayEvent) -> Verdict {
        if matches!(self.policy, VerifyPolicy::Trust) {
            return Verdict::Admit;
        }
        match event {
            nostro2::NostrRelayEvent::NewNote(_, _, note) => Self::judge_note(note),
            _ => Verdict::Admit,
        }
    }

    #[cfg(any(feature = "k256", feature = "secp256k1"))]
    fn judge_note(note: &nostro2::NostrNote) -> Verdict {
        if nostro2::NostrEvent::verify(note) {
            Verdict::Admit
        } else {
            Verdict::Reject
        }
    }

    #[cfg(not(any(feature = "k256", feature = "secp256k1")))]
    fn judge_note(_note: &nostro2::NostrNote) -> Verdict {
        Verdict::Admit
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(feature = "k256")]
    fn signed_note() -> nostro2::NostrNote {
        use nostro2::{NostrKeypair as _, NostrSigner as _};
        let keypair = nostro2_signer::NostrKeypair::generate();
        let mut note = nostro2::NostrNote {
            kind: 1,
            content: "a signed note".to_string(),
            pubkey: keypair.public_key(),
            ..Default::default()
        };
        note.sign_with(&keypair).unwrap();
        note
    }

    #[cfg(feature = "k256")]
    fn new_note_event(note: nostro2::NostrNote) -> nostro2::NostrRelayEvent {
        nostro2::NostrRelayEvent::NewNote(nostro2::RelayEventTag::Event, "sub".to_string(), note)
    }

    #[test]
    #[cfg(feature = "k256")]
    fn a_signed_note_is_admitted() {
        let verifier = NoteVerifier::new();
        assert_eq!(
            verifier.judge(&new_note_event(signed_note())),
            Verdict::Admit
        );
    }

    #[test]
    #[cfg(feature = "k256")]
    fn a_note_with_a_tampered_content_is_rejected() {
        let mut note = signed_note();
        note.content = "swapped after signing".to_string();
        assert_eq!(
            NoteVerifier::new().judge(&new_note_event(note)),
            Verdict::Reject
        );
    }

    #[test]
    #[cfg(feature = "k256")]
    fn a_note_attributed_to_another_pubkey_is_rejected() {
        use nostro2::{NostrKeypair as _, NostrSigner as _};
        let mut note = signed_note();
        note.pubkey = nostro2_signer::NostrKeypair::generate().public_key();
        assert_eq!(
            NoteVerifier::new().judge(&new_note_event(note)),
            Verdict::Reject
        );
    }

    #[test]
    #[cfg(feature = "k256")]
    fn an_unsigned_note_is_rejected() {
        let note = nostro2::NostrNote {
            kind: 1,
            content: "never signed".to_string(),
            ..Default::default()
        };
        assert_eq!(
            NoteVerifier::new().judge(&new_note_event(note)),
            Verdict::Reject
        );
    }

    #[test]
    #[cfg(feature = "k256")]
    fn a_trusting_verifier_admits_a_forged_note() {
        let mut note = signed_note();
        note.content = "swapped after signing".to_string();
        let verifier = NoteVerifier::with_policy(VerifyPolicy::Trust);
        assert_eq!(verifier.judge(&new_note_event(note)), Verdict::Admit);
        assert!(!verifier.is_enforcing());
    }

    #[test]
    fn bookkeeping_frames_carry_no_signature_and_pass() {
        let verifier = NoteVerifier::new();
        let frames = [
            nostro2::NostrRelayEvent::Notice(nostro2::RelayEventTag::Notice, "hi".to_string()),
            nostro2::NostrRelayEvent::EndOfSubscription(
                nostro2::RelayEventTag::Eose,
                "sub".to_string(),
            ),
            nostro2::NostrRelayEvent::SentOk(
                nostro2::RelayEventTag::Ok,
                "id".to_string(),
                true,
                String::new(),
            ),
        ];
        for frame in &frames {
            assert_eq!(verifier.judge(frame), Verdict::Admit);
        }
    }

    #[test]
    fn the_default_policy_rejects() {
        assert_eq!(NoteVerifier::new().policy(), VerifyPolicy::Reject);
        assert_eq!(VerifyPolicy::default(), VerifyPolicy::Reject);
    }
}

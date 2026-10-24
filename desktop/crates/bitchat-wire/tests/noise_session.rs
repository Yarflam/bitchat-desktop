//! Session-level tests for the NoiseSession port: full handshake flow,
//! nonce-prefixed transport frames, replay-window behavior, peer identity
//! binding, and rekey policy.

use bitchat_wire::noise::{derive_peer_id, NoiseError, NoiseSession, SessionState};

fn keypair() -> snow::Keypair {
    let params: snow::params::NoiseParams = "Noise_XX_25519_ChaChaPoly_SHA256".parse().unwrap();
    snow::Builder::new(params).generate_keypair().unwrap()
}

fn established_pair(now_ms: u64) -> (NoiseSession, NoiseSession) {
    let init_keys = keypair();
    let resp_keys = keypair();
    let init_public: [u8; 32] = init_keys.public.try_into().unwrap();
    let resp_public: [u8; 32] = resp_keys.public.try_into().unwrap();
    // Each session authenticates the REMOTE peer: the initiator claims the
    // responder's wire ID and vice versa.
    let mut initiator = NoiseSession::new(
        &derive_peer_id(&resp_public),
        true,
        init_keys.private.try_into().unwrap(),
        now_ms,
    )
    .unwrap();
    let mut responder = NoiseSession::new(
        &derive_peer_id(&init_public),
        false,
        resp_keys.private.try_into().unwrap(),
        now_ms,
    )
    .unwrap();

    let message1 = initiator.start_handshake().unwrap();
    assert_eq!(32, message1.len(), "XX message 1 is e (32 bytes)");
    let message2 = responder
        .process_handshake_message(&message1)
        .unwrap()
        .unwrap();
    assert_eq!(
        96,
        message2.len(),
        "XX message 2 is e, ee, s, es (96 bytes)"
    );
    let message3 = initiator
        .process_handshake_message(&message2)
        .unwrap()
        .unwrap();
    // 48-byte encrypted static key + 16-byte MAC of the empty payload;
    // deployed clients always encrypt the handshake payload (noise-java's
    // null-payload branch still calls encryptAndHash(..., 0)).
    assert_eq!(
        64,
        message3.len(),
        "XX message 3 is s, se + empty-payload MAC"
    );
    assert!(initiator.is_established());
    assert!(responder
        .process_handshake_message(&message3)
        .unwrap()
        .is_none());
    assert!(responder.is_established());

    // The authenticated remote key must be the responder's static public key.
    assert_eq!(Some(resp_public), initiator.remote_static_public_key());
    // Channel binding: both sides agree on the handshake hash.
    assert_eq!(initiator.handshake_hash(), responder.handshake_hash());

    (initiator, responder)
}

#[test]
fn full_handshake_establishes_bidirectional_transport() {
    let (mut initiator, mut responder) = established_pair(0);

    let frame = initiator.encrypt(b"hello mesh").unwrap();
    assert_eq!(4 + 10 + 16, frame.len(), "<nonce:4><ciphertext+tag>");
    assert_eq!(&[0, 0, 0, 0], &frame[..4], "first send nonce is 0");
    assert_eq!(b"hello mesh", responder.decrypt(&frame).unwrap().as_slice());

    let reply = responder.encrypt(b"hi back").unwrap();
    assert_eq!(&[0, 0, 0, 0], &reply[..4]);
    assert_eq!(b"hi back", initiator.decrypt(&reply).unwrap().as_slice());

    // Nonces increment per direction.
    let frame2 = initiator.encrypt(b"again").unwrap();
    assert_eq!(&[0, 0, 0, 1], &frame2[..4]);
    assert_eq!(b"again", responder.decrypt(&frame2).unwrap().as_slice());
}

#[test]
fn out_of_order_delivery_within_window_succeeds() {
    let (mut initiator, mut responder) = established_pair(0);
    let f0 = initiator.encrypt(b"m0").unwrap();
    let f1 = initiator.encrypt(b"m1").unwrap();
    let f2 = initiator.encrypt(b"m2").unwrap();

    // Deliver 0, 2, then 1: nonce 1 is still inside the window.
    assert_eq!(b"m0", responder.decrypt(&f0).unwrap().as_slice());
    assert_eq!(b"m2", responder.decrypt(&f2).unwrap().as_slice());
    assert_eq!(b"m1", responder.decrypt(&f1).unwrap().as_slice());
}

#[test]
fn replayed_nonce_is_rejected() {
    let (mut initiator, mut responder) = established_pair(0);
    let frame = initiator.encrypt(b"once").unwrap();
    responder.decrypt(&frame).unwrap();
    assert_eq!(Err(NoiseError::DecryptionFailed), responder.decrypt(&frame));
}

#[test]
fn nonce_outside_window_is_rejected() {
    let (mut initiator, mut responder) = established_pair(0);
    // Encrypt 1025 frames so nonce 0 falls outside the 1024-bit window.
    let mut frames = Vec::new();
    for _ in 0..1025 {
        frames.push(initiator.encrypt(b"x").unwrap());
    }
    for f in &frames[1..] {
        responder.decrypt(f).unwrap();
    }
    assert_eq!(
        Err(NoiseError::DecryptionFailed),
        responder.decrypt(&frames[0])
    );
}

#[test]
fn tampered_ciphertext_is_rejected() {
    let (mut initiator, mut responder) = established_pair(0);
    let mut frame = initiator.encrypt(b"integrity").unwrap();
    let last = frame.len() - 1;
    frame[last] ^= 1;
    assert_eq!(Err(NoiseError::DecryptionFailed), responder.decrypt(&frame));
}

#[test]
fn peer_identity_mismatch_fails_before_established() {
    let init_keys = keypair();
    let resp_keys = keypair();
    // Claim an ID that does not derive from the responder's static key.
    let claimed = "0000000000000000";

    let mut initiator =
        NoiseSession::new(claimed, true, init_keys.private.try_into().unwrap(), 0).unwrap();
    let mut responder =
        NoiseSession::new(claimed, false, resp_keys.private.try_into().unwrap(), 0).unwrap();

    let message1 = initiator.start_handshake().unwrap();
    let message2 = responder
        .process_handshake_message(&message1)
        .unwrap()
        .unwrap();
    match initiator.process_handshake_message(&message2) {
        Err(NoiseError::PeerIdentityMismatch {
            claimed: c,
            derived,
        }) => {
            assert_eq!(claimed, c);
            let resp_public: [u8; 32] = resp_keys.public.try_into().unwrap();
            assert_eq!(derive_peer_id(&resp_public), derived);
        }
        other => panic!("expected PeerIdentityMismatch, got {other:?}"),
    }
    assert_eq!(SessionState::Failed, initiator.state());
    assert!(!initiator.is_established());
}

#[test]
fn responder_needs_correct_claimed_id_too() {
    let init_keys = keypair();
    let resp_keys = keypair();
    let init_public: [u8; 32] = init_keys.public.try_into().unwrap();
    let wrong_claimed = "ffffffffffffffff";

    let resp_public: [u8; 32] = resp_keys.public.try_into().unwrap();
    let mut initiator = NoiseSession::new(
        &derive_peer_id(&resp_public),
        true,
        init_keys.private.try_into().unwrap(),
        0,
    )
    .unwrap();
    let mut responder = NoiseSession::new(
        wrong_claimed,
        false,
        resp_keys.private.try_into().unwrap(),
        0,
    )
    .unwrap();

    let message1 = initiator.start_handshake().unwrap();
    let message2 = responder
        .process_handshake_message(&message1)
        .unwrap()
        .unwrap();
    let message3 = initiator
        .process_handshake_message(&message2)
        .unwrap()
        .unwrap();
    // The responder authenticates the initiator's static key when message 3
    // completes the pattern.
    assert_eq!(
        Err(NoiseError::PeerIdentityMismatch {
            claimed: wrong_claimed.to_string(),
            derived: derive_peer_id(&init_public),
        }),
        responder.process_handshake_message(&message3)
    );
}

#[test]
fn encrypt_before_establishment_is_rejected() {
    let keys = keypair();
    let mut initiator = NoiseSession::new(
        &derive_peer_id(&[0xAA; 32]),
        true,
        keys.private.try_into().unwrap(),
        0,
    )
    .unwrap();
    assert_eq!(
        Err(NoiseError::InvalidState(
            "session not established".to_string()
        )),
        initiator.encrypt(b"nope")
    );
}

#[test]
fn needs_rekey_tracks_time_and_message_limits() {
    let (mut initiator, mut responder) = established_pair(0);
    assert!(!initiator.needs_rekey(0));

    initiator.encrypt(&(0u8..200).collect::<Vec<u8>>()).unwrap();
    let frame = initiator.encrypt(&(0u8..200).collect::<Vec<u8>>()).unwrap();
    responder.decrypt(&frame).unwrap();
    assert_eq!(2, initiator.messages_sent());
    assert_eq!(1, responder.messages_received());
    assert!(!initiator.needs_rekey(0));

    // Past the 1-hour time limit.
    assert!(initiator.needs_rekey(3_600_001));
    // Past the 10k session message limit.
    let mut busy = initiator;
    // Drive the session message total one past the 10k ceiling.
    let extra = 10_000 - busy.messages_sent() - busy.messages_received() + 1;
    for _ in 0..extra {
        let _ = busy.encrypt(b"x").unwrap();
    }
    assert!(busy.needs_rekey(0));
}

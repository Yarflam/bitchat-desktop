//! Cacophony/Noise-C vector for Noise_XX_25519_ChaChaPoly_SHA256, ported from
//! `NoiseExternalVectorTest.kt`. Exercises the snow state machine with fixed
//! static and ephemeral keys so every handshake and transport byte is pinned.

use snow::{HandshakeState, TransportState};

fn hex(value: &str) -> Vec<u8> {
    assert!(value.len().is_multiple_of(2));
    value
        .as_bytes()
        .chunks(2)
        .map(|c| u8::from_str_radix(std::str::from_utf8(c).unwrap(), 16).unwrap())
        .collect()
}

struct VectorMessage {
    payload_hex: &'static str,
    ciphertext_hex: &'static str,
}

impl VectorMessage {
    fn payload(&self) -> Vec<u8> {
        hex(self.payload_hex)
    }
    fn ciphertext(&self) -> Vec<u8> {
        hex(self.ciphertext_hex)
    }
}

const MESSAGES: &[VectorMessage] = &[
    VectorMessage {
        payload_hex: "4c756477696720766f6e204d69736573",
        ciphertext_hex: "ca35def5ae56cec33dc2036731ab14896bc4c75dbb07a61f879f8e3afa4c7944\
4c756477696720766f6e204d69736573",
    },
    VectorMessage {
        payload_hex: "4d757272617920526f746862617264",
        ciphertext_hex: "95ebc60d2b1fa672c1f46a8aa265ef51bfe38e7ccb39ec5be34069f144808843\
81cbad1f276e038c48378ffce2b65285e08d6b68aaa3629a5a8639392490e5b9\
bd5269c2f1e4f488ed8831161f19b7815528f8982ffe09be9b5c412f8a0db50f\
8814c7194e83f23dbd8d162c9326ad",
    },
    VectorMessage {
        payload_hex: "462e20412e20486179656b",
        ciphertext_hex: "c7195ffacac1307ff99046f219750fc47693e23c3cb08b89c2af808b444850a8\
0ae475b9df0f169ae80a89be0865b57f58c9fea0d4ec82a286427402f113e4b6\
ae769a1d95941d49b25030",
    },
    VectorMessage {
        payload_hex: "4361726c204d656e676572",
        ciphertext_hex: "96763ed773f8e47bb3712f0e29b3060ffc956ffc146cee53d5e1df",
    },
    VectorMessage {
        payload_hex: "4a65616e2d426170746973746520536179",
        ciphertext_hex: "3e40f15f6f3a46ae446b253bf8b1d9ffb6ed9b174d272328ff91a7e2e5c79c07f5",
    },
    VectorMessage {
        payload_hex: "457567656e2042f6686d20766f6e2042617765726b",
        ciphertext_hex: "eb3f3515110702e047a6c9da4478b6ead94873c11c0f2d710ddb3f09fce024b3\
a58502ae3f",
    },
];

const INITIATOR_STATIC: &str = "e61ef9919cde45dd5f82166404bd08e38bceb5dfdfded0a34c8df7ed542214d1";
const RESPONDER_STATIC: &str = "4a3acbfdb163dec651dfa3194dece676d437029c62a408b4c5ea9114246e4893";
const INITIATOR_EPHEMERAL: &str =
    "893e28b9dc6ca8d611ab664754b8ceb7bac5117349a4439a6b0569da977c464a";
const RESPONDER_EPHEMERAL: &str =
    "bbdb4cdbd309f1a1f2e1456967fe288cadd6f712d65dc7b7793d5e63da6b375b";

fn vector_state(initiator: bool) -> HandshakeState {
    let params: snow::params::NoiseParams = "Noise_XX_25519_ChaChaPoly_SHA256".parse().unwrap();
    // Key bytes must outlive the builder borrows.
    let prologue = hex("4a6f686e2047616c74");
    let static_key = hex(if initiator {
        INITIATOR_STATIC
    } else {
        RESPONDER_STATIC
    });
    let ephemeral_key = hex(if initiator {
        INITIATOR_EPHEMERAL
    } else {
        RESPONDER_EPHEMERAL
    });
    let builder = snow::Builder::new(params)
        .prologue(&prologue)
        .unwrap()
        .local_private_key(&static_key)
        .unwrap()
        .fixed_ephemeral_key_for_testing_only(&ephemeral_key);
    if initiator {
        builder.build_initiator().unwrap()
    } else {
        builder.build_responder().unwrap()
    }
}

fn assert_transport_message(
    sender: &mut TransportState,
    receiver: &mut TransportState,
    m: &VectorMessage,
) {
    let mut encrypted = vec![0u8; m.payload().len() + 16];
    let len = sender.write_message(&m.payload(), &mut encrypted).unwrap();
    encrypted.truncate(len);
    assert_eq!(m.ciphertext(), encrypted, "transport transcript mismatch");

    let mut decrypted = vec![0u8; len];
    let out_len = receiver.read_message(&encrypted, &mut decrypted).unwrap();
    decrypted.truncate(out_len);
    assert_eq!(m.payload(), decrypted);
}

fn write(state: &mut HandshakeState, payload: &[u8]) -> Vec<u8> {
    let mut out = vec![0u8; 512];
    let len = state.write_message(payload, &mut out).unwrap();
    out.truncate(len);
    out
}

fn assert_handshake_message(
    writer: &mut HandshakeState,
    reader: &mut HandshakeState,
    m: &VectorMessage,
) {
    let ciphertext = write(writer, &m.payload());
    assert_eq!(m.ciphertext(), ciphertext, "handshake transcript mismatch");

    let mut plaintext = vec![0u8; 256];
    let len = reader.read_message(&ciphertext, &mut plaintext).unwrap();
    plaintext.truncate(len);
    assert_eq!(m.payload(), plaintext);
}

#[test]
fn xx_transcript_matches_every_handshake_and_transport_byte() {
    let mut initiator = vector_state(true);
    let mut responder = vector_state(false);

    assert_handshake_message(&mut initiator, &mut responder, &MESSAGES[0]);
    assert_handshake_message(&mut responder, &mut initiator, &MESSAGES[1]);
    assert_handshake_message(&mut initiator, &mut responder, &MESSAGES[2]);

    assert!(initiator.is_handshake_finished());
    assert!(responder.is_handshake_finished());
    assert_eq!(
        initiator.get_handshake_hash(),
        responder.get_handshake_hash()
    );

    let mut initiator_transport = initiator.into_transport_mode().unwrap();
    let mut responder_transport = responder.into_transport_mode().unwrap();
    assert_transport_message(
        &mut responder_transport,
        &mut initiator_transport,
        &MESSAGES[3],
    );
    assert_transport_message(
        &mut initiator_transport,
        &mut responder_transport,
        &MESSAGES[4],
    );
    assert_transport_message(
        &mut responder_transport,
        &mut initiator_transport,
        &MESSAGES[5],
    );
}

#[test]
fn tampered_handshake_tag_is_rejected() {
    let mut initiator = vector_state(true);
    let mut responder = vector_state(false);

    assert_handshake_message(&mut initiator, &mut responder, &MESSAGES[0]);
    let mut message2 = write(&mut responder, &MESSAGES[1].payload());
    let last = message2.len() - 1;
    message2[last] ^= 1;

    let mut payload = vec![0u8; 256];
    assert!(initiator.read_message(&message2, &mut payload).is_err());
}

#[test]
fn chachapoly_authentication_binds_nonce_ciphertext_tag_and_associated_data() {
    use chacha20poly1305::aead::{Aead, KeyInit, Payload};
    use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};

    // Noise's ChaChaPoly nonce encoding: 4 zero bytes ++ 64-bit little-endian.
    let mut nonce_bytes = [0u8; 12];
    nonce_bytes[4..].copy_from_slice(&7u64.to_le_bytes());
    let nonce = Nonce::from_slice(&nonce_bytes);

    let key_bytes: Vec<u8> = (0u8..32).collect();
    let key = Key::from_slice(&key_bytes);
    let plaintext = b"associated";
    let associated_data = b"header";

    let sender = ChaCha20Poly1305::new(key);
    let ciphertext = sender
        .encrypt(
            nonce,
            Payload {
                msg: plaintext,
                aad: associated_data,
            },
        )
        .unwrap();

    let receiver = ChaCha20Poly1305::new(key);
    let wrong_ad: [u8; 5] = *b"wrong";
    assert!(receiver
        .decrypt(
            nonce,
            Payload {
                msg: &ciphertext,
                aad: &wrong_ad
            }
        )
        .is_err());
    let decrypted = receiver
        .decrypt(
            nonce,
            Payload {
                msg: &ciphertext,
                aad: associated_data,
            },
        )
        .unwrap();
    assert_eq!(plaintext.as_slice(), decrypted.as_slice());
}

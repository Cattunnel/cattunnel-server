//! The encryption of the XHTTP session body.
//!
//! The endpoint may sit behind a CDN which terminates the TLS, so the tunnel
//! payload has to be encrypted on its own.  The scheme is a signed ephemeral
//! key agreement, the same as the one of the client:
//!
//! 1. The client generates an ephemeral X25519 key pair (`e_c`) and 16 random
//!    bytes (`n_c`), and sends them in the `HELLO` frame.
//! 2. The endpoint generates an ephemeral X25519 key pair (`e_s`) and replies
//!    with the `HELLO_ACK` frame: `e_s` and an Ed25519 signature of the
//!    transcript.  The signature is made with the static key of the endpoint,
//!    the client checks it with the public key from its configuration.
//! 3. The shared secret is `X25519(e_c, e_s)`: both keys are ephemeral, so the
//!    traffic stays unreadable even if the static key of the endpoint leaks.
//! 4. The keys of the two directions are derived from the shared secret with
//!    HKDF-SHA256.
//!
//! The frames are then sent as records: a 4 byte length and the ciphertext of
//! the frame with the AEAD tag.  The nonce is a counter, one per direction.

use ring::aead;
use ring::agreement::{self, EphemeralPrivateKey, UnparsedPublicKey, X25519};
use ring::hkdf;
use ring::signature::{Ed25519KeyPair, UnparsedPublicKey as SignaturePublicKey, ED25519};
use std::fmt;
use std::io;

/// The length of an X25519 key.
pub(crate) const KEY_LEN: usize = 32;
/// The length of the AEAD key.
pub(crate) const AEAD_KEY_LEN: usize = 16;
/// The length of the AEAD tag.
pub(crate) const TAG_LEN: usize = 16;
/// The length of the AEAD nonce.
pub(crate) const NONCE_LEN: usize = 12;
/// The length of the payload of the `HELLO` frame.
pub(crate) const NEGOTIATION_LEN: usize = 6;
const NEGOTIATION: [u8; NEGOTIATION_LEN] = [0, 1, 0, 0, 0, 0];
pub(crate) const HELLO_LEN: usize = NEGOTIATION_LEN + 48;
/// The length of the payload of the `HELLO_ACK` frame.
pub(crate) const HELLO_ACK_LEN: usize = NEGOTIATION_LEN + 96;
/// The length of the header of an encrypted record.
pub(crate) const RECORD_HEADER_LEN: usize = 4;
/// The maximum length of the plaintext of a single record.
pub(crate) const MAX_RECORD_LEN: usize = 1024 * 1024;

/// The context of the key derivation, it must be the same on both sides.
const KDF_INFO_PREFIX: &[u8] = b"cattunnel-xhttp-v1";

/// The error of the encryption.
#[derive(Debug)]
pub(crate) enum CryptoError {
    /// The key exchange or the signature has failed.
    Invalid,
    /// The data cannot be decrypted.
    Decrypt,
}

impl fmt::Display for CryptoError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CryptoError::Invalid => write!(f, "the key exchange has failed"),
            CryptoError::Decrypt => write!(f, "the record cannot be decrypted"),
        }
    }
}

impl std::error::Error for CryptoError {}

/// Encrypt outgoing records independently of the negotiated suite.
pub(crate) trait RecordSealer: Send {
    fn seal(&mut self, data: &[u8]) -> Result<Vec<u8>, CryptoError>;
}

/// Authenticate incoming records independently of the negotiated suite.
pub(crate) trait RecordOpener: Send {
    fn open(&mut self, record: &[u8]) -> Result<Vec<u8>, CryptoError>;
}

pub(crate) type Sealer = Box<dyn RecordSealer>;
pub(crate) type Opener = Box<dyn RecordOpener>;

/// The encryption of the outgoing direction of suite 1.
struct Suite1Sealer {
    key: aead::LessSafeKey,
    counter: u64,
    sid: String,
}

/// The decryption of the incoming direction of a session.
struct Suite1Opener {
    key: aead::LessSafeKey,
    counter: u64,
    sid: String,
}

/// The length of the derived key, the HKDF of `ring` needs a type for it.
struct KeyLen(usize);

impl hkdf::KeyType for KeyLen {
    fn len(&self) -> usize {
        self.0
    }
}

/// Derive the key of one direction from the shared secret.
fn derive_key(prk: &hkdf::Prk, info: &[u8]) -> Result<aead::LessSafeKey, CryptoError> {
    let info_slice = [info];
    let okm = prk
        .expand(&info_slice, KeyLen(AEAD_KEY_LEN))
        .map_err(|_| CryptoError::Invalid)?;
    let mut key = [0u8; AEAD_KEY_LEN];
    okm.fill(&mut key).map_err(|_| CryptoError::Invalid)?;
    let unbound =
        aead::UnboundKey::new(&aead::AES_128_GCM, &key).map_err(|_| CryptoError::Invalid)?;
    Ok(aead::LessSafeKey::new(unbound))
}

/// Build the transcript the signature is computed over.
fn build_transcript(
    sid: &str,
    client_ephemeral: &[u8],
    server_ephemeral: &[u8],
    nonce: &[u8],
) -> Vec<u8> {
    let mut transcript = b"cattunnel-xhttp-handshake-v1".to_vec();
    transcript.extend_from_slice(&1u16.to_be_bytes());
    transcript.extend_from_slice(&NEGOTIATION);
    transcript.extend_from_slice(sid.as_bytes());
    transcript.extend_from_slice(client_ephemeral);
    transcript.extend_from_slice(server_ephemeral);
    transcript.extend_from_slice(nonce);
    transcript
}

/// The nonce of a record: the direction and the counter.
fn make_nonce(direction: u8, counter: u64) -> [u8; NONCE_LEN] {
    let mut nonce = [0u8; NONCE_LEN];
    nonce[0] = direction;
    nonce[4..].copy_from_slice(&counter.to_be_bytes());
    nonce
}

impl RecordSealer for Suite1Sealer {
    /// Encrypt the frame and wrap it into a record.
    fn seal(&mut self, data: &[u8]) -> Result<Vec<u8>, CryptoError> {
        if data.len() > MAX_RECORD_LEN {
            return Err(CryptoError::Invalid);
        }

        let nonce = aead::Nonce::assume_unique_for_key(make_nonce(2, self.counter));
        self.counter = self.counter.checked_add(1).ok_or(CryptoError::Invalid)?;

        let mut in_out = data.to_vec();
        self.key
            .seal_in_place_append_tag(nonce, aead::Aad::from(self.sid.as_bytes()), &mut in_out)
            .map_err(|_| CryptoError::Invalid)?;

        let cipher_len = in_out.len();
        let mut record = Vec::with_capacity(RECORD_HEADER_LEN + cipher_len);
        record.extend_from_slice(&(cipher_len as u32).to_be_bytes());
        record.extend_from_slice(&in_out);
        Ok(record)
    }
}

impl RecordOpener for Suite1Opener {
    /// Unwrap the record and decrypt the frame.
    fn open(&mut self, record: &[u8]) -> Result<Vec<u8>, CryptoError> {
        if record.len() < RECORD_HEADER_LEN + TAG_LEN {
            return Err(CryptoError::Decrypt);
        }

        let cipher_len = u32::from_be_bytes([record[0], record[1], record[2], record[3]]) as usize;
        if cipher_len != record.len() - RECORD_HEADER_LEN {
            return Err(CryptoError::Decrypt);
        }

        let next_counter = self.counter.checked_add(1).ok_or(CryptoError::Invalid)?;
        let nonce = aead::Nonce::assume_unique_for_key(make_nonce(1, self.counter));

        let mut in_out = record[RECORD_HEADER_LEN..].to_vec();
        let plain = self
            .key
            .open_in_place(nonce, aead::Aad::from(self.sid.as_bytes()), &mut in_out)
            .map_err(|_| CryptoError::Decrypt)?;
        self.counter = next_counter;
        Ok(plain.to_vec())
    }
}

/// The endpoint side of the handshake: check the `HELLO` frame of the client
/// and prepare the payload of the `HELLO_ACK` frame.
///
/// * `seed` is the seed of the static Ed25519 key of the endpoint.
/// * `hello` is the payload of the `HELLO` frame.
pub(crate) fn server_start(
    sid: &str,
    seed: &[u8; KEY_LEN],
    hello: &[u8],
) -> Result<(Sealer, Opener, [u8; HELLO_ACK_LEN]), CryptoError> {
    let (sealer, opener, ack) = suite1_start(sid, seed, hello)?;
    Ok((Box::new(sealer), Box::new(opener), ack))
}

fn suite1_start(
    sid: &str,
    seed: &[u8; KEY_LEN],
    hello: &[u8],
) -> Result<(Suite1Sealer, Suite1Opener, [u8; HELLO_ACK_LEN]), CryptoError> {
    if hello.len() != HELLO_LEN || hello[..NEGOTIATION_LEN] != NEGOTIATION {
        return Err(CryptoError::Invalid);
    }

    let client_ephemeral = &hello[NEGOTIATION_LEN..NEGOTIATION_LEN + KEY_LEN];
    let nonce = &hello[NEGOTIATION_LEN + KEY_LEN..];

    let rng = ring::rand::SystemRandom::new();
    let peer_public = UnparsedPublicKey::new(&X25519, client_ephemeral);
    let ephemeral =
        EphemeralPrivateKey::generate(&X25519, &rng).map_err(|_| CryptoError::Invalid)?;
    let own_public = ephemeral
        .compute_public_key()
        .map_err(|_| CryptoError::Invalid)?;

    let mut shared = [0u8; KEY_LEN];
    agreement::agree_ephemeral(ephemeral, &peer_public, |material| {
        shared.copy_from_slice(material);
        Ok::<(), ring::error::Unspecified>(())
    })
    .map_err(|_| CryptoError::Invalid)?
    .map_err(|_| CryptoError::Invalid)?;

    let transcript = build_transcript(sid, client_ephemeral, own_public.as_ref(), nonce);
    let transcript_hash = ring::digest::digest(&ring::digest::SHA256, &transcript);
    let salt = hkdf::Salt::new(hkdf::HKDF_SHA256, transcript_hash.as_ref());
    let prk = salt.extract(&shared);

    // The endpoint sends the data down, so its transmit key is the receive
    // key of the client.
    let mut tx_info = KDF_INFO_PREFIX.to_vec();
    tx_info.extend_from_slice(b" s2c");
    let mut rx_info = KDF_INFO_PREFIX.to_vec();
    rx_info.extend_from_slice(b" c2s");

    let tx = derive_key(&prk, &tx_info)?;
    let rx = derive_key(&prk, &rx_info)?;

    let key_pair = Ed25519KeyPair::from_seed_unchecked(seed).map_err(|_| CryptoError::Invalid)?;
    let signature = key_pair.sign(&transcript);

    let mut hello_ack = [0u8; HELLO_ACK_LEN];
    hello_ack[..NEGOTIATION_LEN].copy_from_slice(&NEGOTIATION);
    hello_ack[NEGOTIATION_LEN..NEGOTIATION_LEN + KEY_LEN].copy_from_slice(own_public.as_ref());
    hello_ack[NEGOTIATION_LEN + KEY_LEN..].copy_from_slice(signature.as_ref());

    Ok((
        Suite1Sealer {
            key: tx,
            counter: 0,
            sid: sid.to_string(),
        },
        Suite1Opener {
            key: rx,
            counter: 0,
            sid: sid.to_string(),
        },
        hello_ack,
    ))
}

/// Check the signature of the endpoint with its static public key.
///
/// It is used by the tests: the client does the same check with the same
/// public key.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn verify_signature(
    public_key: &[u8; KEY_LEN],
    transcript: &[u8],
    signature: &[u8],
) -> bool {
    let key = SignaturePublicKey::new(&ED25519, public_key);
    key.verify(transcript, signature).is_ok()
}

/// Decode the seed of the static key of the endpoint.
pub(crate) fn decode_seed(encoded: &str) -> Option<[u8; KEY_LEN]> {
    use base64::Engine;

    let bytes = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .ok()?;
    bytes.try_into().ok()
}

/// The result of taking a record from the decoder.
#[derive(Debug, PartialEq)]
pub(crate) enum RecordTake {
    /// The record is complete.
    Record(Vec<u8>),
    /// The record is not complete yet: more data is needed.
    Incomplete,
    /// The length of the record is not valid: the stream is broken.
    Invalid,
}

/// The decoder of the encrypted records.
#[derive(Default)]
pub(crate) struct RecordDecoder {
    /// The bytes which do not form a complete record yet.
    buffer: Vec<u8>,
}

impl RecordDecoder {
    pub(crate) fn new() -> Self {
        Self { buffer: Vec::new() }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.buffer.is_empty()
    }

    pub(crate) fn capacity(&self) -> usize {
        self.buffer.capacity()
    }

    /// Append the data to the buffer.
    ///
    /// The buffer holds at most one record: a longer one means a broken
    /// stream, and the error stops the session.
    pub(crate) fn push(&mut self, data: &[u8]) -> io::Result<()> {
        if self.buffer.len() + data.len() > RECORD_HEADER_LEN + MAX_RECORD_LEN + TAG_LEN {
            return Err(io::Error::other("The XHTTP record is too long"));
        }
        self.buffer.extend_from_slice(data);
        Ok(())
    }

    /// Take the next complete record from the buffer, if there is one.
    ///
    /// The length which is not valid is reported apart from the record which
    /// is not complete yet: the first one breaks the stream, the second one is
    /// the normal state of the buffer.
    pub(crate) fn take_record(&mut self) -> RecordTake {
        if self.buffer.len() < RECORD_HEADER_LEN {
            return RecordTake::Incomplete;
        }

        let cipher_len = u32::from_be_bytes([
            self.buffer[0],
            self.buffer[1],
            self.buffer[2],
            self.buffer[3],
        ]) as usize;
        if !(TAG_LEN..=MAX_RECORD_LEN + TAG_LEN).contains(&cipher_len) {
            return RecordTake::Invalid;
        }
        if self.buffer.len() < RECORD_HEADER_LEN + cipher_len {
            return RecordTake::Incomplete;
        }

        let record = self
            .buffer
            .drain(..RECORD_HEADER_LEN + cipher_len)
            .collect::<Vec<u8>>();
        RecordTake::Record(record)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SEED: [u8; KEY_LEN] = [
        0x70, 0x8b, 0x7d, 0xb8, 0x1e, 0x45, 0x7d, 0x6b, 0xf8, 0x64, 0xd8, 0x81, 0x16, 0x57, 0xfa,
        0xd5, 0x11, 0x58, 0x02, 0xdd, 0x5d, 0x6d, 0x0a, 0xbc, 0x32, 0xa2, 0x1e, 0x8a, 0xfc, 0x43,
        0x6f, 0xeb,
    ];

    const PUBLIC: [u8; KEY_LEN] = [
        0x99, 0xf3, 0x8c, 0xbc, 0x84, 0xe2, 0x35, 0x84, 0x4a, 0xdd, 0x10, 0x09, 0xef, 0x38, 0x6e,
        0x69, 0x07, 0x4c, 0xc9, 0x51, 0x05, 0x2e, 0xf9, 0x56, 0xcd, 0x0e, 0x64, 0x13, 0xaf, 0xa8,
        0x4a, 0x4d,
    ];

    const SID: &str = "abcdef0123456789";

    #[test]
    fn protocol1_shared_x25519_hkdf_aead_vector() {
        use boring::{derive::Deriver, pkey::PKey};
        let vector: serde_json::Value =
            serde_json::from_str(include_str!("../tests/fixtures/xhttp_suite1.json")).unwrap();
        let bytes = |name: &str| hex::decode(vector[name].as_str().unwrap()).unwrap();
        let mut private_der = hex::decode("302e020100300506032b656e04220420").unwrap();
        private_der.extend_from_slice(&bytes("server_private"));
        let own = PKey::private_key_from_der(&private_der).unwrap();
        let mut public_der = hex::decode("302a300506032b656e032100").unwrap();
        public_der.extend_from_slice(&bytes("client_public"));
        let peer = PKey::public_key_from_der(&public_der).unwrap();
        let mut deriver = Deriver::new(&own).unwrap();
        deriver.set_peer(&peer).unwrap();
        let shared = deriver.derive_to_vec().unwrap();
        assert_eq!(shared, bytes("shared"));
        let transcript = build_transcript(
            SID,
            &bytes("client_public"),
            &bytes("server_public"),
            &bytes("nonce"),
        );
        let hash = ring::digest::digest(&ring::digest::SHA256, &transcript);
        assert_eq!(hash.as_ref(), bytes("salt"));
        assert!(verify_signature(
            &PUBLIC,
            &transcript,
            &bytes("hello_ack")[NEGOTIATION_LEN + KEY_LEN..]
        ));
        let prk = hkdf::Salt::new(hkdf::HKDF_SHA256, hash.as_ref()).extract(&shared);
        let mut sealer = Suite1Sealer {
            key: derive_key(&prk, b"cattunnel-xhttp-v1 s2c").unwrap(),
            counter: 0,
            sid: SID.into(),
        };
        let mut opener = Suite1Opener {
            key: derive_key(&prk, b"cattunnel-xhttp-v1 c2s").unwrap(),
            counter: 0,
            sid: SID.into(),
        };
        assert_eq!(sealer.seal(&bytes("plain")).unwrap(), bytes("s2c_record"));
        let mut invalid = bytes("c2s_record");
        *invalid.last_mut().unwrap() ^= 1;
        assert!(opener.open(&invalid).is_err());
        assert_eq!(opener.counter, 0);
        assert_eq!(opener.open(&bytes("c2s_record")).unwrap(), bytes("plain"));
        assert_eq!(opener.counter, 1);
        assert!(opener.open(&bytes("c2s_record")).is_err());
        assert_eq!(opener.counter, 1);
    }

    /// The payload of the `HELLO` frame of the client: an ephemeral key and a
    /// nonce.
    fn client_hello() -> [u8; HELLO_LEN] {
        let rng = ring::rand::SystemRandom::new();
        let ephemeral = EphemeralPrivateKey::generate(&X25519, &rng).unwrap();
        let public = ephemeral.compute_public_key().unwrap();

        let mut hello = [0u8; HELLO_LEN];
        hello[..NEGOTIATION_LEN].copy_from_slice(&NEGOTIATION);
        hello[NEGOTIATION_LEN..NEGOTIATION_LEN + KEY_LEN].copy_from_slice(public.as_ref());
        for (i, b) in hello[NEGOTIATION_LEN + KEY_LEN..].iter_mut().enumerate() {
            *b = i as u8;
        }
        hello
    }

    #[test]
    fn nonce_exhaustion_never_wraps() {
        let (mut sealer, mut opener, _) = suite1_start(SID, &SEED, &client_hello()).unwrap();
        sealer.counter = u64::MAX;
        assert!(sealer.seal(b"secret").is_err());
        assert_eq!(sealer.counter, u64::MAX);
        opener.counter = u64::MAX;
        let mut record = [0u8; RECORD_HEADER_LEN + TAG_LEN];
        record[..4].copy_from_slice(&(TAG_LEN as u32).to_be_bytes());
        assert!(opener.open(&record).is_err());
        assert_eq!(opener.counter, u64::MAX);
    }

    #[test]
    fn unknown_suite_and_capabilities_are_rejected() {
        for index in 0..NEGOTIATION_LEN {
            let mut hello = client_hello();
            hello[index] ^= 1;
            assert!(server_start(SID, &SEED, &hello).is_err());
        }
        assert!(server_start(SID, &SEED, &[0; 48]).is_err());
    }

    #[test]
    fn the_handshake_produces_a_checkable_signature() {
        let hello = client_hello();

        let (_sealer, _opener, hello_ack) = server_start(SID, &SEED, &hello).unwrap();

        let transcript = build_transcript(
            SID,
            &hello[NEGOTIATION_LEN..NEGOTIATION_LEN + KEY_LEN],
            &hello_ack[NEGOTIATION_LEN..NEGOTIATION_LEN + KEY_LEN],
            &hello[NEGOTIATION_LEN + KEY_LEN..],
        );
        assert!(verify_signature(
            &PUBLIC,
            &transcript,
            &hello_ack[NEGOTIATION_LEN + KEY_LEN..]
        ));
    }

    #[test]
    fn signature_binds_version_suite_and_every_capability_bit() {
        let hello = client_hello();
        let (_, _, ack) = server_start(SID, &SEED, &hello).unwrap();
        let transcript = build_transcript(
            SID,
            &hello[NEGOTIATION_LEN..NEGOTIATION_LEN + KEY_LEN],
            &ack[NEGOTIATION_LEN..NEGOTIATION_LEN + KEY_LEN],
            &hello[NEGOTIATION_LEN + KEY_LEN..],
        );
        let prefix_len = b"cattunnel-xhttp-handshake-v1".len();
        for index in prefix_len..prefix_len + 2 + NEGOTIATION_LEN {
            for bit in 0..8 {
                let mut tampered = transcript.clone();
                tampered[index] ^= 1 << bit;
                assert!(!verify_signature(
                    &PUBLIC,
                    &tampered,
                    &ack[NEGOTIATION_LEN + KEY_LEN..]
                ));
            }
        }
    }

    #[test]
    fn the_signature_of_another_key_is_rejected() {
        let hello = client_hello();
        let (_sealer, _opener, hello_ack) = server_start(SID, &SEED, &hello).unwrap();

        let transcript = build_transcript(
            SID,
            &hello[NEGOTIATION_LEN..NEGOTIATION_LEN + KEY_LEN],
            &hello_ack[NEGOTIATION_LEN..NEGOTIATION_LEN + KEY_LEN],
            &hello[NEGOTIATION_LEN + KEY_LEN..],
        );
        let mut other_public = PUBLIC;
        other_public[0] ^= 0xff;
        assert!(!verify_signature(
            &other_public,
            &transcript,
            &hello_ack[NEGOTIATION_LEN + KEY_LEN..]
        ));
    }

    #[test]
    fn the_records_are_encrypted() {
        let hello = client_hello();
        let (mut sealer, _opener, _hello_ack) = server_start(SID, &SEED, &hello).unwrap();

        let record = sealer.seal(b"hello").unwrap();
        assert_eq!(record.len(), RECORD_HEADER_LEN + 5 + TAG_LEN);
        assert_ne!(&record[RECORD_HEADER_LEN..RECORD_HEADER_LEN + 5], b"hello");
    }

    #[test]
    fn the_record_decoder_waits_for_the_whole_record() {
        let hello = client_hello();
        let (mut sealer, _opener, _hello_ack) = server_start(SID, &SEED, &hello).unwrap();
        let record = sealer.seal(b"hello").unwrap();

        let mut decoder = RecordDecoder::new();
        for byte in &record[..record.len() - 1] {
            decoder.push(&[*byte]).unwrap();
            assert!(matches!(decoder.take_record(), RecordTake::Incomplete));
        }
        decoder.push(&record[record.len() - 1..]).unwrap();
        assert_eq!(decoder.take_record(), RecordTake::Record(record.clone()));
        assert!(matches!(decoder.take_record(), RecordTake::Incomplete));
    }

    #[test]
    fn the_record_decoder_reports_an_invalid_length() {
        let mut decoder = RecordDecoder::new();
        // The length is less than the tag.
        decoder.push(&[0, 0, 0, 1]).unwrap();
        assert!(matches!(decoder.take_record(), RecordTake::Invalid));

        // The length is more than the maximum record.
        let mut decoder = RecordDecoder::new();
        let too_long = (MAX_RECORD_LEN + TAG_LEN + 1) as u32;
        decoder.push(&too_long.to_be_bytes()).unwrap();
        assert!(matches!(decoder.take_record(), RecordTake::Invalid));
    }

    #[test]
    fn the_record_decoder_does_not_grow_forever() {
        let mut decoder = RecordDecoder::new();
        let max = (MAX_RECORD_LEN + TAG_LEN) as u32;
        decoder.push(&max.to_be_bytes()).unwrap();
        // The record is not complete yet, the buffer holds its beginning.
        assert!(matches!(decoder.take_record(), RecordTake::Incomplete));
        // More data than a record may hold: the buffer must not grow.
        let too_much = vec![0u8; RECORD_HEADER_LEN + MAX_RECORD_LEN + TAG_LEN];
        assert!(decoder.push(&too_much).is_err());
    }
}

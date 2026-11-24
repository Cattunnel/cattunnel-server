use super::*;
use serde_json::Value;

fn vectors() -> Value {
    serde_json::from_str(include_str!("../../../docs/cattail-vectors.json")).unwrap()
}

fn bytes(v: &Value) -> Vec<u8> {
    hex::decode(v.as_str().unwrap()).unwrap()
}

fn array<const N: usize>(v: &Value) -> [u8; N] {
    bytes(v).try_into().unwrap()
}

fn rng() -> StdRng {
    StdRng::seed_from_u64(1595)
}

#[test]
fn vector_keys() {
    for s in vectors()["sessions"].as_array().unwrap() {
        let key: [u8; KEY_LEN] = array(&s["key"]);
        assert_eq!(decode_key(s["key_base64"].as_str().unwrap()), Some(key));
        let n_c = array(&s["client_nonce"]);
        let n_s = array(&s["server_nonce"]);
        assert_eq!(
            key_c2s(&key, &n_c).to_vec(),
            bytes(&s["key_c2s"]),
            "{}",
            s["name"]
        );
        assert_eq!(
            key_s2c(&key, &n_c, &n_s).to_vec(),
            bytes(&s["key_s2c"]),
            "{}",
            s["name"]
        );
    }
}

/// Every frame of the vectors is produced byte for byte from its fields.
#[test]
fn vector_frames_encode_byte_for_byte() {
    for s in vectors()["sessions"].as_array().unwrap() {
        for (dir, key_name, nonce_name, frames, wire) in [
            (
                DIRECTION_C2S,
                "key_c2s",
                "client_nonce",
                "c2s_frames",
                "c2s_wire",
            ),
            (
                DIRECTION_S2C,
                "key_s2c",
                "server_nonce",
                "s2c_frames",
                "s2c_wire",
            ),
        ] {
            let mut sealer = Sealer::new(&array(&s[key_name]), dir);
            let mut all = bytes(&s[nonce_name]);
            for f in s[frames].as_array().unwrap() {
                let plain: [u8; HEADER_LEN] = array(&f["header_plain"]);
                let header = Header::decode(&plain);
                assert_eq!(header.encode(), plain);
                let mut out = Vec::new();
                sealer
                    .frame_into(
                        header,
                        &bytes(&f["body"]),
                        &bytes(&f["mid_padding"]),
                        &bytes(&f["end_padding"]),
                        &mut out,
                    )
                    .unwrap();
                assert_eq!(
                    hex::encode(&out),
                    f["wire"].as_str().unwrap(),
                    "{}",
                    s["name"]
                );
                all.extend_from_slice(&out);
            }
            assert_eq!(hex::encode(all), s[wire].as_str().unwrap());
        }
    }
}

/// The wire of the vectors passes the admission and decodes to the plaintext.
#[tokio::test]
async fn vector_wire_decodes() {
    for s in vectors()["sessions"].as_array().unwrap() {
        let key = array(&s["key"]);
        let ts = s["timestamp"].as_u64().unwrap();
        let gate = Gate::new(key, ts);
        let wire = bytes(&s["c2s_wire"]);
        let Admitted {
            client_nonce,
            mut opener,
            header,
        } = gate
            .admit(&wire[..FIRST_SEGMENT_LEN].try_into().unwrap(), ts)
            .ok()
            .unwrap();
        assert_eq!(client_nonce.to_vec(), bytes(&s["client_nonce"]));
        assert_eq!(header.identity.to_vec(), bytes(&s["identity"]));
        assert_eq!(header.counter, s["counter"].as_u64().unwrap());
        assert_eq!(header.timestamp, ts);
        let mut reader = &wire[FIRST_SEGMENT_LEN..];
        let mut plain = read_frame_rest(&mut reader, &mut opener, &header)
            .await
            .unwrap();
        while let Some(body) = read_frame(&mut reader, &mut opener).await.unwrap() {
            plain.extend_from_slice(&body);
        }
        assert_eq!(hex::encode(plain), s["c2s_plain"].as_str().unwrap());

        let wire = bytes(&s["s2c_wire"]);
        let n_s: [u8; NONCE_LEN] = wire[..NONCE_LEN].try_into().unwrap();
        let mut opener = Opener::new(&key_s2c(&key, &client_nonce, &n_s), DIRECTION_S2C);
        let mut reader = &wire[NONCE_LEN..];
        let mut plain = Vec::new();
        while let Some(body) = read_frame(&mut reader, &mut opener).await.unwrap() {
            plain.extend_from_slice(&body);
        }
        assert_eq!(hex::encode(plain), s["s2c_plain"].as_str().unwrap());
    }
}

#[test]
fn vector_rejects() {
    let v = vectors();
    let ts = v["sessions"][0]["timestamp"].as_u64().unwrap();
    for r in v["rejects"].as_array().unwrap() {
        let gate = Gate::new(array(&r["key"]), 0);
        let wire = bytes(&r["c2s_wire"]);
        let result = gate.admit(&wire[..FIRST_SEGMENT_LEN].try_into().unwrap(), ts);
        assert_eq!(result.err(), Some(Reject::Tag), "{}", r["name"]);
    }
}

#[test]
fn vector_timestamps() {
    for t in vectors()["timestamps"].as_array().unwrap() {
        let get = |k: &str| t[k].as_u64().unwrap();
        assert_eq!(
            timestamp_ok(get("now"), get("start"), get("timestamp")),
            t["accept"].as_bool().unwrap(),
            "{t}"
        );
    }
}

#[test]
fn vector_replay() {
    let mut registry = ReplayRegistry::new(REPLAY_IDENTITIES);
    for r in vectors()["replay"].as_array().unwrap() {
        assert_eq!(
            registry.accept(array(&r["identity"]), r["counter"].as_u64().unwrap()),
            r["accept"].as_bool().unwrap(),
            "{r}"
        );
    }
}

fn scheme_json(s: &PaddingScheme) -> Value {
    let mut lines = serde_json::Map::new();
    for (i, line) in s.lines.iter().enumerate() {
        if let Some(items) = line {
            let items: Vec<Value> = items
                .iter()
                .map(|item| match item {
                    Item::Check => Value::from("c"),
                    Item::Size(lo, hi) => serde_json::json!([lo, hi]),
                })
                .collect();
            lines.insert(i.to_string(), Value::from(items));
        }
    }
    serde_json::json!({
        "stop": s.stop,
        "lines": lines,
        "mid": [s.mid.0, s.mid.1],
        "end": [s.end.0, s.end.1],
        "nonce": [s.nonce.0, s.nonce.1],
    })
}

#[test]
fn vector_patterns() {
    for p in vectors()["patterns"].as_array().unwrap() {
        let parsed = PaddingScheme::parse(p["pattern"].as_str().unwrap());
        if p["valid"].as_bool().unwrap() {
            assert_eq!(
                scheme_json(&parsed.unwrap()),
                p["parsed"],
                "{}",
                p["pattern"]
            );
        } else {
            assert!(parsed.is_err(), "{}", p["pattern"]);
        }
    }
    assert_eq!(
        PaddingScheme::default(),
        PaddingScheme::parse(vectors()["patterns"][0]["pattern"].as_str().unwrap()).unwrap()
    );
}

#[test]
fn shared_pattern_contract() {
    let cases: Value =
        serde_json::from_str(include_str!("../../../docs/cattail-pattern-contract.json")).unwrap();
    for case in cases.as_array().unwrap() {
        let pattern = case["pattern"].as_str().unwrap();
        assert_eq!(
            PaddingScheme::parse(pattern).is_ok(),
            case["valid"].as_bool().unwrap(),
            "{pattern:?}"
        );
    }
}

#[test]
fn replay_registry_forgets_the_oldest_identity() {
    let mut registry = ReplayRegistry::new(3);
    for id in 1..=3u8 {
        assert!(registry.accept([id; 8], 0));
    }
    assert!(!registry.accept([1; 8], 0));
    assert!(registry.accept([4; 8], 0));
    assert_eq!(registry.len(), 3);
    // identity 1 was added first and is gone, identity 2 is still there
    assert!(!registry.accept([2; 8], 0));
    assert!(registry.accept([1; 8], 0));
}

#[test]
fn replay_window_keeps_every_counter_of_the_window() {
    let mut registry = ReplayRegistry::new(4);
    let id = [9; 8];
    for c in 0..REPLAY_WINDOW {
        assert!(registry.accept(id, c), "{c}");
    }
    for c in 0..REPLAY_WINDOW {
        assert!(!registry.accept(id, c), "{c}");
    }
    // the window moves by 70 (more than one word of the map)
    assert!(registry.accept(id, REPLAY_WINDOW + 69));
    for c in 0..70 {
        assert!(!registry.accept(id, c), "too old {c}");
    }
    for c in 70..REPLAY_WINDOW {
        assert!(!registry.accept(id, c), "seen {c}");
    }
    for c in REPLAY_WINDOW..REPLAY_WINDOW + 69 {
        assert!(registry.accept(id, c), "new {c}");
    }
}

#[test]
fn plan_carries_all_data_and_keeps_the_scheme_sizes() {
    let scheme = PaddingScheme::default();
    let mut rng = rng();
    for len in [0usize, 1, 63, 64, 300, 1800, 5000, 40000] {
        for write in 0..5u64 {
            let plans = scheme.plan(write, len, &mut rng);
            assert_eq!(plans.iter().map(|p| p.body).sum::<usize>(), len);
            for p in &plans {
                assert!(p.body <= MAX_BODY_LEN);
            }
            if write == 0 && len > 0 {
                let p = plans[0];
                let wire = SEALED_HEADER_LEN + p.mid as usize + p.end as usize + p.body + TAG_LEN;
                assert!((400..=1400).contains(&wire), "write 0 len {len}: {wire}");
            }
            if write >= 3 {
                for p in &plans {
                    assert!(p.mid <= 32 && p.end <= 96);
                }
            }
        }
    }
    // a write with no data: only the items before the first check
    let plans = scheme.plan(0, 0, &mut rng);
    assert_eq!(plans.len(), 1);
    let wire = SEALED_HEADER_LEN + plans[0].mid as usize + plans[0].end as usize;
    assert!((400..=1400).contains(&wire));
}

#[test]
fn printable_nonce_prefix() {
    let scheme = PaddingScheme::parse("nonce=8-8").unwrap();
    let mut rng = rng();
    for _ in 0..100 {
        let n = scheme.make_nonce(&mut rng);
        assert!(n[..8].iter().all(|b| (0x21..=0x7e).contains(b)), "{n:?}");
    }
}

#[test]
fn header_check() {
    let mut h = Header::open(1, [1; 8], 2);
    assert!(h.check(TYPE_OPEN).is_ok());
    assert!(h.check(TYPE_DATA).is_err());
    h.version = 2;
    assert!(h.check(TYPE_OPEN).is_err());
    h.version = VERSION;
    h.body_len = MAX_BODY_LEN as u16 + 1;
    assert!(h.check(TYPE_OPEN).is_err());
    let h = Header::open(u64::MAX, [3; 8], 0xffff_ffff_ffff);
    assert_eq!(Header::decode(&h.encode()), h);
}

#[test]
fn opener_counter_stays_on_a_bad_tag() {
    let key = [5u8; 16];
    let mut sealer = Sealer::new(&key, DIRECTION_C2S);
    let mut out = Vec::new();
    sealer
        .frame_into(Header::data(), b"abc", &[], &[], &mut out)
        .unwrap();
    let mut opener = Opener::new(&key, DIRECTION_C2S);
    let mut bad: [u8; SEALED_HEADER_LEN] = out[..SEALED_HEADER_LEN].try_into().unwrap();
    bad[SEALED_HEADER_LEN - 1] ^= 1;
    assert_eq!(opener.open_header(&bad), Err(Error::Decrypt));
    assert!(opener
        .open_header(&out[..SEALED_HEADER_LEN].try_into().unwrap())
        .is_ok());
    // the other direction byte does not open
    let mut other = Opener::new(&key, DIRECTION_S2C);
    assert!(other
        .open_header(&out[..SEALED_HEADER_LEN].try_into().unwrap())
        .is_err());
}

/// Random writes through the client encoder come out of the endpoint side
/// unchanged, with the shaping of the default scheme.
#[tokio::test]
async fn encoder_round_trip() {
    let key = [0x42u8; KEY_LEN];
    let mut rng = rng();
    let now = unix_now();
    let (mut encoder, _) = Encoder::client(
        &key,
        PaddingScheme::default(),
        Header::open(now, [1; 8], 7),
        &mut rng,
    );
    let mut wire = Vec::new();
    let mut plain = Vec::new();
    for len in [1800usize, 1, 0, 70000, 5, 16384, 333] {
        let data: Vec<u8> = (0..len).map(|_| rng.gen()).collect();
        wire.extend_from_slice(&encoder.encode(&data, &mut rng).unwrap());
        plain.extend_from_slice(&data);
    }
    let gate = Gate::new(key, now);
    let Admitted {
        mut opener, header, ..
    } = gate
        .admit(&wire[..FIRST_SEGMENT_LEN].try_into().unwrap(), now)
        .ok()
        .unwrap();
    let mut reader = &wire[FIRST_SEGMENT_LEN..];
    let mut got = read_frame_rest(&mut reader, &mut opener, &header)
        .await
        .unwrap();
    while let Some(body) = read_frame(&mut reader, &mut opener).await.unwrap() {
        got.extend_from_slice(&body);
    }
    assert_eq!(got, plain);
    // the same first segment again is a replay
    assert_eq!(
        gate.admit(&wire[..FIRST_SEGMENT_LEN].try_into().unwrap(), now)
            .err(),
        Some(Reject::Replay)
    );
}

#[test]
fn gate_rejects_time_and_type() {
    let key = [0x11u8; KEY_LEN];
    let now = 1_795_219_200;
    let segment = |header: Header| {
        let mut rng = rng();
        let (mut encoder, _) =
            Encoder::client(&key, PaddingScheme::parse("").unwrap(), header, &mut rng);
        let wire = encoder.encode(b"x", &mut rng).unwrap();
        <[u8; FIRST_SEGMENT_LEN]>::try_from(&wire[..FIRST_SEGMENT_LEN]).unwrap()
    };
    let gate = Gate::new(key, now - 60);
    assert_eq!(
        gate.admit(&segment(Header::open(now - 121, [1; 8], 0)), now)
            .err(),
        Some(Reject::Timestamp)
    );
    assert_eq!(
        gate.admit(&segment(Header::open(now + 121, [2; 8], 0)), now)
            .err(),
        Some(Reject::Timestamp)
    );
    assert_eq!(
        gate.admit(&segment(Header::open(now - 61, [3; 8], 0)), now)
            .err(),
        Some(Reject::Timestamp)
    );
    let mut data_first = Header::open(now, [4; 8], 0);
    data_first.frame_type = TYPE_DATA;
    assert_eq!(
        gate.admit(&segment(data_first), now).err(),
        Some(Reject::Frame)
    );
    // a rejected timestamp does not take the counter: the right one passes
    assert!(gate
        .admit(&segment(Header::open(now, [1; 8], 0)), now)
        .is_ok());
}

#[test]
fn settings_validation() {
    use crate::settings::{CatTailSettings, Http2Settings, ListenProtocolSettings, Settings};
    use base64::Engine;
    let good_key = base64::engine::general_purpose::STANDARD.encode([1u8; 32]);
    let main: SocketAddr = "127.0.0.1:4433".parse().unwrap();
    let make = |addr: &str, key: &str, pattern: Option<&str>| {
        Settings::builder()
            .listen_address(main)
            .unwrap()
            .listen_protocols(ListenProtocolSettings {
                http2: Some(Http2Settings::builder().build()),
                ..Default::default()
            })
            .cattail(CatTailSettings {
                listen_address: addr.parse().unwrap(),
                key: key.into(),
                pattern: pattern.map(String::from),
            })
            .build()
    };
    assert!(make("127.0.0.1:20443", &good_key, None).is_ok());
    assert!(make("127.0.0.1:20443", &good_key, Some("stop=1\n0=64")).is_ok());
    assert!(make("127.0.0.1:20443", "short", None).is_err());
    assert!(make("127.0.0.1:20443", &good_key, Some("stop=99")).is_err());
    assert!(make("0.0.0.0:4433", &good_key, None).is_err());
}

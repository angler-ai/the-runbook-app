//! CF3: the Workers sealer interoperates with the native `ring` sealer in
//! both directions, opens the native v1 fixture, and keeps the native
//! guarantees (context binding, key ids, rotation, tamper refusal).
use runbook_core::webhooks::{SealedSecret, SecretSealer, encode_webhook_secret};
use runbook_server::webhooks::AeadSealer;
use runbook_worker::sealer::AesSealer;

/// The native sealer's own v1 fixture (key [7; 32]), pinned in
/// `runbook-server/src/webhooks.rs`.
const V1_FIXTURE: &str = "v1.u73Surapxd1KnyaICzYt4UI5ZXfOBEqo7xlBePD-0PQKEJs-e0s8M9LoYjA0SXt_NMXxGRiOYBm0ekLg1x-xOsOr45aZuyE_cqKdVsk";
const CONTEXT: &str = "whk_00000000000000000000000001";

fn nonce(bytes: &mut [u8]) -> Result<(), ()> {
    getrandom::fill(bytes).map_err(|_| ())
}
fn workers(current: [u8; 32], previous: &[[u8; 32]]) -> AesSealer {
    AesSealer::with_previous(&current, previous, nonce)
}

#[test]
fn key_ids_match_the_native_sealer() {
    for key in [[7; 32], [8; 32], [0; 32], [255; 32]] {
        assert_eq!(AesSealer::key_id(&key), AeadSealer::key_id(&key));
    }
}

#[test]
fn the_native_v1_fixture_opens() {
    let secret = encode_webhook_secret([3; 32]);
    let fixture = SealedSecret(V1_FIXTURE.into());
    assert_eq!(
        workers([7; 32], &[]).open(CONTEXT, &fixture).unwrap(),
        secret
    );
    assert_eq!(
        workers([8; 32], &[[7; 32]])
            .open(CONTEXT, &fixture)
            .unwrap(),
        secret
    );
    assert!(workers([8; 32], &[]).open(CONTEXT, &fixture).is_err());
}

#[test]
fn values_move_between_editions_in_both_directions() {
    let secret = encode_webhook_secret([5; 32]);
    let native = AeadSealer::new(&[7; 32]);
    let cloudflare = workers([7; 32], &[]);
    let from_native = native.seal(CONTEXT, &secret).unwrap();
    assert_eq!(cloudflare.open(CONTEXT, &from_native).unwrap(), secret);
    let from_workers = cloudflare.seal(CONTEXT, &secret).unwrap();
    assert!(from_workers.0.starts_with("v2."));
    assert_eq!(native.open(CONTEXT, &from_workers).unwrap(), secret);
    // Rotation: a value sealed under the previous key opens on both.
    let rotated_native = AeadSealer::with_previous(&[8; 32], &[[7; 32]]);
    assert_eq!(rotated_native.open(CONTEXT, &from_workers).unwrap(), secret);
    let rotated = workers([8; 32], &[[7; 32]]);
    assert_eq!(rotated.open(CONTEXT, &from_native).unwrap(), secret);
}

#[test]
fn a_value_opens_only_for_its_context_key_and_bytes() {
    let secret = encode_webhook_secret([6; 32]);
    let sealer = workers([7; 32], &[]);
    let sealed = sealer.seal("whk_a", &secret).unwrap();
    assert_ne!(
        sealed,
        sealer.seal("whk_a", &secret).unwrap(),
        "fresh nonce"
    );
    assert!(!sealed.0.contains(&secret));
    assert!(sealer.open("whk_b", &sealed).is_err());
    assert!(workers([9; 32], &[]).open("whk_a", &sealed).is_err());
    let mut tampered = sealed.0.clone();
    let last = tampered.pop().unwrap();
    tampered.push(if last == 'A' { 'B' } else { 'A' });
    assert!(sealer.open("whk_a", &SealedSecret(tampered)).is_err());
    assert!(
        sealer
            .open("whk_a", &SealedSecret("v3.AAAA".into()))
            .is_err()
    );
}

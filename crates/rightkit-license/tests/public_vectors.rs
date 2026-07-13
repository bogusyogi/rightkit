use base64::Engine;
use rightkit_license::verify_token;
use serde::Deserialize;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct LicenseVector {
    public_key_base64url: String,
    token: String,
    expected_app: String,
    expected_device_id: String,
    now_unix: i64,
    expected_tier: String,
    expected_features: Vec<String>,
}

#[test]
fn published_v2_vector_verifies() {
    let vector: LicenseVector =
        serde_json::from_str(include_str!("../test-vectors/license-v2.json"))
            .expect("public license vector must be valid JSON");
    let key = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(vector.public_key_base64url)
        .expect("public key must be base64url");
    let key: [u8; 32] = key.try_into().expect("public key must be 32 bytes");

    let entitlement = verify_token(
        &vector.token,
        &key,
        &vector.expected_app,
        &vector.expected_device_id,
        vector.now_unix,
    )
    .expect("published vector must verify");

    assert_eq!(entitlement.tier, vector.expected_tier);
    assert_eq!(entitlement.features, vector.expected_features);
}

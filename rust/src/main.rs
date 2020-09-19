//! Cryptographic helpers functions (signing and signature verification).
use anyhow::Context;
use base64::URL_SAFE_NO_PAD;
use clap::Clap;
use openssl::ec::EcKey;
use openssl::ecdsa::EcdsaSig;
use openssl::hash::MessageDigest;
use openssl::nid::Nid;
use openssl::pkey::Private;
use serde_json::{json, Value};
use std::path::PathBuf;
use uuid::Uuid;

/// A small command line interface to sign POST requests for Payouts API.
#[derive(Clap)]
#[clap(version = "1.0", author = "TrueLayer")]
struct Command {
    /// The filename of the payload you want to sign, in JSON format.
    #[clap(long)]
    payload_filename: PathBuf,
    /// The filename of the Elliptic Curve private key used to sign, in PEM format.
    #[clap(long)]
    private_key_filename: PathBuf,
    /// The certificate id associated to the public certificate you uploaded in TrueLayer's Console.
    /// The certificate id can be retrieved in the Payouts Setting section.
    /// It will be used as the `kid` header in the JWS.
    #[clap(long)]
    certificate_id: Uuid,
}

impl Command {
    /// Parse the JSON payload from the specified file.
    pub fn payload(&self) -> Result<Value, anyhow::Error> {
        let raw_payload = std::fs::read(&self.payload_filename)
            .context("Failed to read the request payload file.")?;
        let payload: Value = serde_json::from_slice(&raw_payload)
            .context("Failed to parse the request payload as JSON.")?;
        Ok(payload)
    }

    /// Parse the EC private key from the specified file.
    pub fn private_key(&self) -> Result<EcKey<Private>, anyhow::Error> {
        let raw_private_key = std::fs::read(&self.private_key_filename)
            .context("Failed to read the private key file.")?;
        let private_key = openssl::pkey::PKey::private_key_from_pem(&raw_private_key)
            .context("Failed to parse the private key as PEM.")?
            .ec_key()
            .context("The private key must be an Elliptic Curve key.")?;
        private_key.check_key().context("Key verification failed")?;
        Ok(private_key)
    }
}

#[derive(serde::Serialize)]
pub struct JwsPayload {
    #[serde(rename = "Content-Type")]
    content_type: String,
    body: Value,
}

#[tokio::main]
pub async fn main() -> Result<(), anyhow::Error> {
    let options = Command::parse();

    let jws_header = json!({
        "alg": "ES512",
        "kid": options.certificate_id.to_string()
    });
    let jws_payload = options.payload()?;
    let jws_payload = serde_json::to_string(&jws_payload)?;
    let private_key = options.private_key()?;

    let jws = get_jws(&jws_header, &jws_payload, private_key)?;
    println!("JWS:\n{}\n", jws);

    let parts = jws.split(".").collect::<Vec<_>>();
    let detached_jsw = format!("{}..{}", parts[0], parts[2]);
    // Omit the payload for a JWS with detached payload
    println!("JWS with detached content:\n{}\n", detached_jsw);

    let response = reqwest::Client::new()
        .post("https://payouts.t7r.co/v1/test")
        .bearer_auth("eyJhbGciOiJSUzI1NiIsImtpZCI6IjVCM0ExQzhGODMyOTlEQjJCNTE3NUVGMDBGQjYwOTc2QTkwQTMzMjFSUzI1NiIsInR5cCI6ImF0K2p3dCIsIng1dCI6Ild6b2NqNE1wbmJLMUYxN3dEN1lKZHFrS015RSJ9.eyJuYmYiOjE2MDA1NDM3OTEsImV4cCI6MTYwMDU0NzM5MSwiaXNzIjoiaHR0cHM6Ly9hdXRoLnQ3ci5jbyIsImF1ZCI6InBheW91dHNfYXBpIiwiY2xpZW50X2lkIjoidGVzdC1wbW90IiwianRpIjoiQTBDREVEODU2NDdBMkM1ODA5MUFCQzcyNjJFNTU5RUYiLCJpYXQiOjE2MDA1NDM3OTEsInNjb3BlIjpbInBheW91dHMiXX0.Z_Dgx6QkRq7Y3dSYPuteztxceaklSrn8I1Xr68UtqLy-THMiJ2v33-x3_E2-A2PyDKPcS8LEnVL-M2pKOvqMvL89wfhcG50xR7NNV3p7rFrMobGfEJbo17-AfiABzlTGzForerIwDaVp5mPn6Q0eYgrnY5hNmuWjEkhVAvOaSBikg0m_1x3gh_u-fhEL-urgn0Er-vzs6v87yXlUbo38RF_DvUdHEXV4TthsWlQPyv069SfROu0Z_WUV8phl370YqLJiMpHN29tYVBRbPD5jIBhzTSw3TSuPARTZ2z2qaEz-6ewKouiN4Ogj6Qa2pgGHDvSzEygE1C5mYn-Pu_pLYw")
        .header("X-TL-Signature", detached_jsw)
        .header("Content-Type", "application/json")
        .body(jws_payload.as_bytes().to_vec())
        .send()
        .await
        .expect("Failed to get response");
    let status_code = response.status();
    if status_code.is_success() {
        println!("The request to Payouts API /test endpoint succeeded!")
    } else {
        let body = response.text().await.unwrap();
        println!(
            "The request to Payouts API /test endpoint failed with status code {} and body: {}",
            status_code, body
        );
    }

    Ok(())
}

/// Get a JWS using the ES512 signing scheme.
///
/// Check section A.4 of RFC7515 for the details: https://www.rfc-editor.org/rfc/rfc7515.txt
pub fn get_jws(
    jws_header: &Value,
    jws_payload: &str,
    pkey: EcKey<Private>,
) -> Result<String, anyhow::Error> {
    let to_be_signed = format!(
        "{}.{}",
        base64_encode(serde_json::to_string(&jws_header)?.as_bytes()),
        base64_encode(jws_payload.as_bytes()),
    );
    let signature = sign_es512(to_be_signed.as_bytes(), pkey)?;

    let jws = format!(
        "{}.{}.{}",
        base64_encode(serde_json::to_string(&jws_header)?.as_bytes()),
        base64_encode(jws_payload.as_bytes()),
        signature
    );
    Ok(jws)
}

/// Sign a payload using the provided private key and return the signature as a base64 encoded string.
///
/// Check section A.4 of RFC7515 for the details: https://www.rfc-editor.org/rfc/rfc7515.txt
pub fn sign_es512(payload: &[u8], pkey: EcKey<Private>) -> Result<String, anyhow::Error> {
    if pkey.group().curve_name() != Some(Nid::SECP521R1) {
        return Err(anyhow::anyhow!(
            "The underlying elliptic curve must be P-521 to sign using ES512."
        ));
    }
    let hash = openssl::hash::hash(MessageDigest::sha512(), &payload)?;
    let structured_signature = EcdsaSig::sign(&hash, &pkey)?;

    let r = structured_signature.r().to_vec();
    let s = structured_signature.s().to_vec();
    let mut signature_bytes: Vec<u8> = Vec::new();
    // Padding to fixed length
    signature_bytes.extend(std::iter::repeat(0x00).take(66 - r.len()));
    signature_bytes.extend(r);
    // Padding to fixed length
    signature_bytes.extend(std::iter::repeat(0x00).take(66 - s.len()));
    signature_bytes.extend(s);

    Ok(base64_encode(&signature_bytes))
}

/// Base64 encoding according to RFC7515 - see `Base64url` in section 2.
pub fn base64_encode(payload: &[u8]) -> String {
    base64::encode_config(payload, URL_SAFE_NO_PAD)
}
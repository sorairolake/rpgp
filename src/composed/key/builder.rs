use std::time::Duration;

use chrono::SubsecRound;
use derive_builder::Builder;
use rand::{thread_rng, CryptoRng, Rng};
use smallvec::SmallVec;

use crate::composed::{KeyDetails, SecretKey, SecretSubkey};
use crate::crypto::ecc_curve::ECCCurve;
use crate::crypto::hash::HashAlgorithm;
use crate::crypto::public_key::PublicKeyAlgorithm;
use crate::crypto::sym::SymmetricKeyAlgorithm;
use crate::crypto::{dsa, ecdh, ecdsa, eddsa, rsa};
use crate::errors::Result;
use crate::packet::{self, KeyFlags, UserAttribute, UserId};
use crate::types::{self, CompressionAlgorithm, PublicParams, RevocationKey, S2kParams};

#[derive(Debug, PartialEq, Eq, Builder)]
#[builder(build_fn(validate = "Self::validate"))]
pub struct SecretKeyParams {
    key_type: KeyType,

    // -- Keyflags
    #[builder(default)]
    can_sign: bool,
    #[builder(default)]
    can_certify: bool,
    #[builder(default)]
    can_encrypt: bool,

    // -- Preferences
    /// List of symmetric algorithms that indicate which algorithms the key holder prefers to use.
    #[builder(default)]
    preferred_symmetric_algorithms: SmallVec<[SymmetricKeyAlgorithm; 8]>,
    /// List of hash algorithms that indicate which algorithms the key holder prefers to use.
    #[builder(default)]
    preferred_hash_algorithms: SmallVec<[HashAlgorithm; 8]>,
    /// List of compression algorithms that indicate which algorithms the key holder prefers to use.
    #[builder(default)]
    preferred_compression_algorithms: SmallVec<[CompressionAlgorithm; 8]>,
    #[builder(default)]
    revocation_key: Option<RevocationKey>,

    #[builder]
    primary_user_id: String,

    #[builder(default)]
    user_ids: Vec<String>,
    #[builder(default)]
    user_attributes: Vec<UserAttribute>,
    #[builder(default)]
    passphrase: Option<String>,
    #[builder(default)]
    s2k: Option<S2kParams>,
    #[builder(default = "chrono::Utc::now().trunc_subsecs(0)")]
    created_at: chrono::DateTime<chrono::Utc>,
    #[builder(default)]
    packet_version: types::Version,
    #[builder(default)]
    version: types::KeyVersion,
    #[builder(default)]
    expiration: Option<Duration>,

    #[builder(default)]
    subkeys: Vec<SubkeyParams>,
}

#[derive(Debug, Clone, PartialEq, Eq, Builder)]
pub struct SubkeyParams {
    key_type: KeyType,

    #[builder(default)]
    can_sign: bool,
    #[builder(default)]
    can_certify: bool,
    #[builder(default)]
    can_encrypt: bool,
    #[builder(default)]
    can_authenticate: bool,

    #[builder(default)]
    user_ids: Vec<UserId>,
    #[builder(default)]
    user_attributes: Vec<UserAttribute>,
    #[builder(default)]
    passphrase: Option<String>,
    #[builder(default)]
    s2k: Option<S2kParams>,
    #[builder(default = "chrono::Utc::now().trunc_subsecs(0)")]
    created_at: chrono::DateTime<chrono::Utc>,
    #[builder(default)]
    packet_version: types::Version,
    #[builder(default)]
    version: types::KeyVersion,
    #[builder(default)]
    expiration: Option<Duration>,
}

impl SecretKeyParamsBuilder {
    fn validate(&self) -> std::result::Result<(), String> {
        match &self.key_type {
            Some(KeyType::Rsa(size)) => {
                if *size < 2048 {
                    return Err("Keys with less than 2048bits are considered insecure".into());
                }
            }
            Some(KeyType::EdDSALegacy) => {
                if let Some(can_encrypt) = self.can_encrypt {
                    if can_encrypt {
                        return Err("EdDSA can only be used for signing keys".into());
                    }
                }
            }
            Some(KeyType::ECDSA(curve)) => {
                if let Some(can_encrypt) = self.can_encrypt {
                    if can_encrypt {
                        return Err("ECDSA can only be used for signing keys".into());
                    }
                };
                match curve {
                    ECCCurve::P256 | ECCCurve::P384 | ECCCurve::P521 | ECCCurve::Secp256k1 => {}
                    _ => return Err(format!("Curve {} is not supported for ECDSA", curve.name())),
                }
            }
            Some(KeyType::ECDH(_)) => {
                if let Some(can_sign) = self.can_sign {
                    if can_sign {
                        return Err("ECDH can only be used for encryption keys".into());
                    }
                }
            }
            Some(KeyType::Dsa(_)) => {
                if let Some(can_encrypt) = self.can_encrypt {
                    if can_encrypt {
                        return Err("DSA can only be used for signing keys".into());
                    }
                }
            }
            _ => {}
        }

        Ok(())
    }

    pub fn user_id<VALUE: Into<String>>(&mut self, value: VALUE) -> &mut Self {
        if let Some(ref mut user_ids) = self.user_ids {
            user_ids.push(value.into());
        } else {
            self.user_ids = Some(vec![value.into()]);
        }
        self
    }

    pub fn subkey<VALUE: Into<SubkeyParams>>(&mut self, value: VALUE) -> &mut Self {
        if let Some(ref mut subkeys) = self.subkeys {
            subkeys.push(value.into());
        } else {
            self.subkeys = Some(vec![value.into()]);
        }
        self
    }
}

impl SecretKeyParams {
    pub fn generate(self) -> Result<SecretKey> {
        let rng = thread_rng();
        self.generate_with_rng(rng)
    }

    pub fn generate_with_rng<R: Rng + CryptoRng>(self, mut rng: R) -> Result<SecretKey> {
        let passphrase = self.passphrase;
        let s2k = self.s2k.unwrap_or_else(|| S2kParams::new_default(&mut rng));
        let (public_params, secret_params) =
            self.key_type.generate_with_rng(&mut rng, passphrase, s2k)?;
        let primary_key = packet::SecretKey::new(
            packet::PublicKey::new(
                self.packet_version,
                self.version,
                self.key_type.to_alg(),
                self.created_at,
                self.expiration.map(|v| v.as_secs() as u16),
                public_params,
            )?,
            secret_params,
        );

        let mut keyflags = KeyFlags::default();
        keyflags.set_certify(self.can_certify);
        keyflags.set_encrypt_comms(self.can_encrypt);
        keyflags.set_encrypt_storage(self.can_encrypt);
        keyflags.set_sign(self.can_sign);

        Ok(SecretKey::new(
            primary_key,
            KeyDetails::new(
                UserId::from_str(Default::default(), &self.primary_user_id),
                self.user_ids
                    .iter()
                    .map(|m| UserId::from_str(Default::default(), m))
                    .collect(),
                self.user_attributes,
                keyflags,
                self.preferred_symmetric_algorithms,
                self.preferred_hash_algorithms,
                self.preferred_compression_algorithms,
                self.revocation_key,
            ),
            Default::default(),
            self.subkeys
                .into_iter()
                .map(|subkey| {
                    let passphrase = subkey.passphrase;
                    let s2k = subkey
                        .s2k
                        .unwrap_or_else(|| S2kParams::new_default(&mut rng));
                    let (public_params, secret_params) =
                        subkey.key_type.generate(passphrase, s2k)?;
                    let mut keyflags = KeyFlags::default();
                    keyflags.set_certify(subkey.can_certify);
                    keyflags.set_encrypt_comms(subkey.can_encrypt);
                    keyflags.set_encrypt_storage(subkey.can_encrypt);
                    keyflags.set_sign(subkey.can_sign);
                    keyflags.set_authentication(subkey.can_authenticate);

                    Ok(SecretSubkey::new(
                        packet::SecretSubkey::new(
                            packet::PublicSubkey::new(
                                subkey.packet_version,
                                subkey.version,
                                subkey.key_type.to_alg(),
                                subkey.created_at,
                                subkey.expiration.map(|v| v.as_secs() as u16),
                                public_params,
                            )?,
                            secret_params,
                        ),
                        keyflags,
                    ))
                })
                .collect::<Result<Vec<_>>>()?,
        ))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KeyType {
    /// Encryption & Signing with RSA and the given bitsize.
    Rsa(u32),
    /// Encrypting with ECDH
    ECDH(ECCCurve),
    /// Signing with Curve25519, legacy format (deprecated in RFC 9580)
    EdDSALegacy,
    /// Signing with ECDSA
    ECDSA(ECCCurve),
    /// Signing with DSA for the given bitsize.
    Dsa(DsaKeySize),
}

#[derive(Clone, Debug, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum DsaKeySize {
    /// DSA parameter size constant: L = 1024, N = 160
    B1024 = 1024,
    /// DSA parameter size constant: L = 2048, N = 256
    B2048 = 2048,
    /// DSA parameter size constant: L = 3072, N = 256
    B3072 = 3072,
}

impl From<DsaKeySize> for dsa::KeySize {
    fn from(value: DsaKeySize) -> Self {
        match value {
            #[allow(deprecated)]
            DsaKeySize::B1024 => dsa::KeySize::DSA_1024_160,
            DsaKeySize::B2048 => dsa::KeySize::DSA_2048_256,
            DsaKeySize::B3072 => dsa::KeySize::DSA_3072_256,
        }
    }
}

impl KeyType {
    pub fn to_alg(&self) -> PublicKeyAlgorithm {
        match self {
            KeyType::Rsa(_) => PublicKeyAlgorithm::RSA,
            KeyType::ECDH(_) => PublicKeyAlgorithm::ECDH,
            KeyType::EdDSALegacy => PublicKeyAlgorithm::EdDSALegacy,
            KeyType::ECDSA(_) => PublicKeyAlgorithm::ECDSA,
            KeyType::Dsa(_) => PublicKeyAlgorithm::DSA,
        }
    }

    pub fn generate(
        &self,
        passphrase: Option<String>,
        s2k: types::S2kParams,
    ) -> Result<(PublicParams, types::SecretParams)> {
        let rng = thread_rng();
        self.generate_with_rng(rng, passphrase, s2k)
    }

    pub fn generate_with_rng<R: Rng + CryptoRng>(
        &self,
        rng: R,
        passphrase: Option<String>,
        s2k: types::S2kParams,
    ) -> Result<(PublicParams, types::SecretParams)> {
        let (pub_params, plain) = match self {
            KeyType::Rsa(bit_size) => rsa::generate_key(rng, *bit_size as usize)?,
            KeyType::ECDH(curve) => ecdh::generate_key(rng, curve)?,
            KeyType::EdDSALegacy => eddsa::generate_key(rng),
            KeyType::ECDSA(curve) => ecdsa::generate_key(rng, curve)?,
            KeyType::Dsa(key_size) => dsa::generate_key(rng, (*key_size).into())?,
        };

        let secret = match passphrase {
            Some(passphrase) => {
                // TODO: derive from key itself
                let version = types::KeyVersion::default();

                types::SecretParams::Encrypted(plain.encrypt(&passphrase, s2k, version)?)
            }
            None => types::SecretParams::Plain(plain),
        };

        Ok((pub_params, secret))
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use rand::SeedableRng;
    use rand_chacha::ChaCha8Rng;
    use smallvec::smallvec;

    use super::*;
    use crate::composed::{Deserializable, SignedPublicKey, SignedSecretKey};
    use crate::types::SecretKeyTrait;

    #[test]
    #[ignore] // slow in debug mode
    fn test_key_gen_rsa_2048() {
        let _ = pretty_env_logger::try_init();
        let mut rng = ChaCha8Rng::seed_from_u64(0);

        for i in 0..50 {
            println!("round {i}");
            gen_rsa_2048(&mut rng);
        }
    }

    fn gen_rsa_2048<R: Rng + CryptoRng>(mut rng: R) {
        let mut key_params = SecretKeyParamsBuilder::default();
        key_params
            .key_type(KeyType::Rsa(2048))
            .can_certify(true)
            .can_sign(true)
            .primary_user_id("Me <me@mail.com>".into())
            .preferred_symmetric_algorithms(smallvec![
                SymmetricKeyAlgorithm::AES256,
                SymmetricKeyAlgorithm::AES192,
                SymmetricKeyAlgorithm::AES128,
            ])
            .preferred_hash_algorithms(smallvec![
                HashAlgorithm::SHA2_256,
                HashAlgorithm::SHA2_384,
                HashAlgorithm::SHA2_512,
                HashAlgorithm::SHA2_224,
                HashAlgorithm::SHA1,
            ])
            .preferred_compression_algorithms(smallvec![
                CompressionAlgorithm::ZLIB,
                CompressionAlgorithm::ZIP,
            ]);

        let key_params_enc = key_params
            .clone()
            .passphrase(Some("hello".into()))
            .subkey(
                SubkeyParamsBuilder::default()
                    .key_type(KeyType::Rsa(2048))
                    .passphrase(Some("hello".into()))
                    .can_encrypt(true)
                    .build()
                    .unwrap(),
            )
            .build()
            .unwrap();
        let key_enc = key_params_enc
            .generate_with_rng(&mut rng)
            .expect("failed to generate secret key, encrypted");

        let key_params_plain = key_params
            .passphrase(None)
            .subkey(
                SubkeyParamsBuilder::default()
                    .key_type(KeyType::Rsa(2048))
                    .can_encrypt(true)
                    .build()
                    .unwrap(),
            )
            .build()
            .unwrap();
        let key_plain = key_params_plain
            .generate_with_rng(&mut rng)
            .expect("failed to generate secret key");

        let signed_key_enc = key_enc.sign(|| "hello".into()).expect("failed to sign key");
        let signed_key_plain = key_plain.sign(|| "".into()).expect("failed to sign key");

        let armor_enc = signed_key_enc
            .to_armored_string(None.into())
            .expect("failed to serialize key");
        let armor_plain = signed_key_plain
            .to_armored_string(None.into())
            .expect("failed to serialize key");

        std::fs::write("sample-rsa-enc.sec.asc", &armor_enc).unwrap();
        std::fs::write("sample-rsa.sec.asc", &armor_plain).unwrap();

        let (signed_key2_enc, _headers) =
            SignedSecretKey::from_string(&armor_enc).expect("failed to parse key (enc)");
        signed_key2_enc.verify().expect("invalid key (enc)");

        let (signed_key2_plain, _headers) =
            SignedSecretKey::from_string(&armor_plain).expect("failed to parse key (plain)");
        signed_key2_plain.verify().expect("invalid key (plain)");

        signed_key2_enc
            .unlock(|| "hello".into(), |_| Ok(()))
            .expect("failed to unlock parsed key (enc)");
        signed_key2_plain
            .unlock(|| "".into(), |_| Ok(()))
            .expect("failed to unlock parsed key (plain)");

        assert_eq!(signed_key_plain, signed_key2_plain);

        let public_key = signed_key_plain.public_key();

        let public_signed_key = public_key
            .sign(&signed_key_plain, || "".into())
            .expect("failed to sign public key");

        public_signed_key.verify().expect("invalid public key");

        let armor = public_signed_key
            .to_armored_string(None.into())
            .expect("failed to serialize public key");

        std::fs::write("sample-rsa.pub.asc", &armor).unwrap();

        let (signed_key2, _headers) =
            SignedPublicKey::from_string(&armor).expect("failed to parse public key");
        signed_key2.verify().expect("invalid public key");
    }

    #[ignore]
    #[test]
    fn key_gen_x25519_long() {
        let mut rng = ChaCha8Rng::seed_from_u64(0);
        for i in 0..10_000 {
            println!("round {i}");
            gen_x25519(&mut rng);
        }
    }

    #[test]
    fn key_gen_x25519_short() {
        let mut rng = ChaCha8Rng::seed_from_u64(0);
        for _ in 0..100 {
            gen_x25519(&mut rng);
        }
    }

    fn gen_x25519<R: Rng + CryptoRng>(rng: R) {
        let _ = pretty_env_logger::try_init();

        let key_params = SecretKeyParamsBuilder::default()
            .key_type(KeyType::EdDSALegacy)
            .can_certify(true)
            .can_sign(true)
            .primary_user_id("Me-X <me-x25519@mail.com>".into())
            .passphrase(None)
            .preferred_symmetric_algorithms(smallvec![
                SymmetricKeyAlgorithm::AES256,
                SymmetricKeyAlgorithm::AES192,
                SymmetricKeyAlgorithm::AES128,
            ])
            .preferred_hash_algorithms(smallvec![
                HashAlgorithm::SHA2_256,
                HashAlgorithm::SHA2_384,
                HashAlgorithm::SHA2_512,
                HashAlgorithm::SHA2_224,
                HashAlgorithm::SHA1,
            ])
            .preferred_compression_algorithms(smallvec![
                CompressionAlgorithm::ZLIB,
                CompressionAlgorithm::ZIP,
            ])
            .subkey(
                SubkeyParamsBuilder::default()
                    .key_type(KeyType::ECDH(ECCCurve::Curve25519))
                    .can_encrypt(true)
                    .passphrase(None)
                    .build()
                    .unwrap(),
            )
            .build()
            .unwrap();

        let key = key_params
            .generate_with_rng(rng)
            .expect("failed to generate secret key");

        let signed_key = key.sign(|| "".into()).expect("failed to sign key");

        let armor = signed_key
            .to_armored_string(None.into())
            .expect("failed to serialize key");

        println!("armor: {armor:?}");
        std::fs::write("sample-x25519.sec.asc", &armor).unwrap();

        let (signed_key2, _headers) =
            SignedSecretKey::from_string(&armor).expect("failed to parse key");
        signed_key2.verify().expect("invalid key");

        assert_eq!(signed_key, signed_key2);

        let public_key = signed_key.public_key();

        let public_signed_key = public_key
            .sign(&signed_key, || "".into())
            .expect("failed to sign public key");

        public_signed_key.verify().expect("invalid public key");

        let armor = public_signed_key
            .to_armored_string(None.into())
            .expect("failed to serialize public key");

        std::fs::write("sample-x25519.pub.asc", &armor).unwrap();

        let (signed_key2, _headers) =
            SignedPublicKey::from_string(&armor).expect("failed to parse public key");
        signed_key2.verify().expect("invalid public key");
    }

    fn gen_ecdsa<R: Rng + CryptoRng>(rng: &mut R, curve: ECCCurve) {
        let _ = pretty_env_logger::try_init();

        let key_params = SecretKeyParamsBuilder::default()
            .key_type(KeyType::ECDSA(curve.clone()))
            .can_certify(true)
            .can_sign(true)
            .primary_user_id("Me-X <me-ecdsa@mail.com>".into())
            .passphrase(None)
            .preferred_symmetric_algorithms(smallvec![
                SymmetricKeyAlgorithm::AES256,
                SymmetricKeyAlgorithm::AES192,
                SymmetricKeyAlgorithm::AES128,
            ])
            .preferred_hash_algorithms(smallvec![
                HashAlgorithm::SHA2_256,
                HashAlgorithm::SHA2_384,
                HashAlgorithm::SHA2_512,
                HashAlgorithm::SHA2_224,
                HashAlgorithm::SHA1,
            ])
            .preferred_compression_algorithms(smallvec![
                CompressionAlgorithm::ZLIB,
                CompressionAlgorithm::ZIP,
            ])
            .subkey(
                SubkeyParamsBuilder::default()
                    .key_type(KeyType::ECDH(ECCCurve::Curve25519))
                    .can_encrypt(true)
                    .passphrase(None)
                    .build()
                    .unwrap(),
            )
            .build()
            .unwrap();

        let key = key_params
            .generate_with_rng(rng)
            .expect("failed to generate secret key");

        let signed_key = key.sign(|| "".into()).expect("failed to sign key");

        let armor = signed_key
            .to_armored_string(None.into())
            .expect("failed to serialize key");

        std::fs::write("sample-ecdsa.sec.asc", &armor).unwrap();

        let (signed_key2, _headers) =
            SignedSecretKey::from_string(&armor).expect("failed to parse key");
        signed_key2.verify().expect("invalid key");

        assert_eq!(signed_key, signed_key2);

        let public_key = signed_key.public_key();

        let public_signed_key = public_key
            .sign(&signed_key, || "".into())
            .expect("failed to sign public key");

        public_signed_key.verify().expect("invalid public key");

        let armor = public_signed_key
            .to_armored_string(None.into())
            .expect("failed to serialize public key");

        std::fs::write(format!("sample-ecdsa-{curve:?}.pub.asc"), &armor).unwrap();

        let (signed_key2, _headers) =
            SignedPublicKey::from_string(&armor).expect("failed to parse public key");
        signed_key2.verify().expect("invalid public key");
    }

    #[test]
    fn key_gen_ecdsa_p256() {
        let rng = &mut ChaCha8Rng::seed_from_u64(0);
        for _ in 0..=175 {
            gen_ecdsa(rng, ECCCurve::P256);
        }
    }

    #[test]
    fn key_gen_ecdsa_p384() {
        let rng = &mut ChaCha8Rng::seed_from_u64(0);
        for _ in 0..100 {
            gen_ecdsa(rng, ECCCurve::P384);
        }
    }

    #[test]
    fn key_gen_ecdsa_p521() {
        let rng = &mut ChaCha8Rng::seed_from_u64(0);
        for _ in 0..100 {
            gen_ecdsa(rng, ECCCurve::P521);
        }
    }

    #[test]
    fn key_gen_ecdsa_secp256k1() {
        let rng = &mut ChaCha8Rng::seed_from_u64(0);
        for _ in 0..100 {
            gen_ecdsa(rng, ECCCurve::Secp256k1);
        }
    }

    fn gen_dsa<R: Rng + CryptoRng>(rng: &mut R, key_size: DsaKeySize) {
        let _ = pretty_env_logger::try_init();

        let key_params = SecretKeyParamsBuilder::default()
            .key_type(KeyType::Dsa(key_size))
            .can_certify(true)
            .can_sign(true)
            .primary_user_id("Me-X <me-dsa@mail.com>".into())
            .passphrase(None)
            .preferred_symmetric_algorithms(smallvec![
                SymmetricKeyAlgorithm::AES256,
                SymmetricKeyAlgorithm::AES192,
                SymmetricKeyAlgorithm::AES128,
            ])
            .preferred_hash_algorithms(smallvec![
                HashAlgorithm::SHA2_256,
                HashAlgorithm::SHA2_384,
                HashAlgorithm::SHA2_512,
                HashAlgorithm::SHA2_224,
                HashAlgorithm::SHA1,
            ])
            .preferred_compression_algorithms(smallvec![
                CompressionAlgorithm::ZLIB,
                CompressionAlgorithm::ZIP,
            ])
            .subkey(
                SubkeyParamsBuilder::default()
                    .key_type(KeyType::ECDH(ECCCurve::Curve25519))
                    .can_encrypt(true)
                    .passphrase(None)
                    .build()
                    .unwrap(),
            )
            .build()
            .unwrap();

        let key = key_params
            .generate_with_rng(rng)
            .expect("failed to generate secret key");

        let signed_key = key.sign(|| "".into()).expect("failed to sign key");

        let armor = signed_key
            .to_armored_string(None.into())
            .expect("failed to serialize key");

        std::fs::write("sample-dsa.sec.asc", &armor).unwrap();

        let (signed_key2, _headers) =
            SignedSecretKey::from_string(&armor).expect("failed to parse key");
        signed_key2.verify().expect("invalid key");

        assert_eq!(signed_key, signed_key2);

        let public_key = signed_key.public_key();

        let public_signed_key = public_key
            .sign(&signed_key, || "".into())
            .expect("failed to sign public key");

        public_signed_key.verify().expect("invalid public key");

        let armor = public_signed_key
            .to_armored_string(None.into())
            .expect("failed to serialize public key");

        std::fs::write(format!("sample-dsa-{key_size:?}.pub.asc"), &armor).unwrap();

        let (signed_key2, _headers) =
            SignedPublicKey::from_string(&armor).expect("failed to parse public key");
        signed_key2.verify().expect("invalid public key");
    }

    // Test is slow in debug mode
    #[test]
    #[ignore]
    fn key_gen_dsa() {
        let rng = &mut ChaCha8Rng::seed_from_u64(0);
        for _ in 0..10 {
            gen_dsa(rng, DsaKeySize::B1024);
            gen_dsa(rng, DsaKeySize::B2048);
            gen_dsa(rng, DsaKeySize::B3072);
        }
    }
}

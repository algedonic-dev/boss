use super::*;
use openssl::{
    asn1::Asn1Time,
    bn::BigNum,
    ec::{EcGroup, EcKey},
    hash::MessageDigest,
    nid::Nid,
    pkey::{PKey, Private},
    x509::{
        X509, X509NameBuilder,
        extension::{BasicConstraints, ExtendedKeyUsage, KeyUsage, SubjectAlternativeName},
        store::X509StoreBuilder,
        verify::{X509CheckFlags, X509VerifyParam},
    },
};

fn key() -> Result<PKey<Private>, openssl::error::ErrorStack> {
    let group = EcGroup::from_curve_name(Nid::X9_62_PRIME256V1)?;
    PKey::from_ec_key(EcKey::generate(&group)?)
}
fn certificate(
    key: &PKey<Private>,
    parent: Option<(&X509, &PKey<Private>)>,
    now: DateTime<Utc>,
    expires: DateTime<Utc>,
) -> Result<X509, openssl::error::ErrorStack> {
    let mut name = X509NameBuilder::new()?;
    name.append_entry_by_text(
        "CN",
        if parent.is_some() {
            HOST
        } else {
            "BOSS private collector trust"
        },
    )?;
    let name = name.build();
    let mut builder = X509::builder()?;
    builder.set_version(2)?;
    let mut random = [0_u8; 16];
    openssl::rand::rand_bytes(&mut random)?;
    random[0] = (random[0] & 0x7f) | 1;
    let serial = BigNum::from_slice(&random)?.to_asn1_integer()?;
    builder.set_serial_number(&serial)?;
    builder.set_subject_name(&name)?;
    builder.set_issuer_name(parent.map(|(ca, _)| ca.subject_name()).unwrap_or(&name))?;
    builder.set_pubkey(key)?;
    let start = Asn1Time::from_unix(now.timestamp())?;
    let end = Asn1Time::from_unix(expires.timestamp())?;
    builder.set_not_before(&start)?;
    builder.set_not_after(&end)?;
    if let Some((ca, _)) = parent {
        builder.append_extension(BasicConstraints::new().critical().build()?)?;
        builder.append_extension(KeyUsage::new().critical().digital_signature().build()?)?;
        builder.append_extension(ExtendedKeyUsage::new().server_auth().build()?)?;
        let names = SubjectAlternativeName::new()
            .dns(HOST)
            .build(&builder.x509v3_context(Some(ca), None))?;
        builder.append_extension(names)?;
    } else {
        builder.append_extension(BasicConstraints::new().critical().ca().pathlen(0).build()?)?;
        builder.append_extension(KeyUsage::new().critical().key_cert_sign().build()?)?;
    }
    builder.sign(
        parent.map(|(_, key)| key).unwrap_or(key),
        MessageDigest::sha256(),
    )?;
    Ok(builder.build())
}
#[async_trait]
impl Issuer for OpenSslIssuer {
    async fn mint(&self, now: DateTime<Utc>) -> Result<Certificate, Refused> {
        tokio::task::spawn_blocking(move || {
            let root_key = key().map_err(|_| Refused)?;
            let server_key = key().map_err(|_| Refused)?;
            let expires = DateTime::from_timestamp(now.timestamp() + CERTIFICATE_SECONDS, 0)
                .ok_or(Refused)?;
            let root = certificate(&root_key, None, now, expires).map_err(|_| Refused)?;
            let leaf = certificate(&server_key, Some((&root, &root_key)), now, expires)
                .map_err(|_| Refused)?;
            let value = Certificate {
                key: String::from_utf8(server_key.private_key_to_pem_pkcs8().map_err(|_| Refused)?)
                    .map_err(|_| Refused)?,
                pem: String::from_utf8(leaf.to_pem().map_err(|_| Refused)?).map_err(|_| Refused)?,
                trust: String::from_utf8(root.to_pem().map_err(|_| Refused)?)
                    .map_err(|_| Refused)?,
                expires,
            };
            OpenSslIssuer.validate(&value, now)?;
            // The root signing key is never persisted; each generation is an
            // independently minted leaf and public trust, not a general CA API.
            Ok(value)
        })
        .await
        .map_err(|_| Refused)?
    }
    fn validate(&self, value: &Certificate, now: DateTime<Utc>) -> Result<(), Refused> {
        let check = || -> Result<bool, openssl::error::ErrorStack> {
            let leaves = X509::stack_from_pem(value.pem.as_bytes())?;
            let roots = X509::stack_from_pem(value.trust.as_bytes())?;
            let (Some(leaf), Some(root)) = (leaves.first(), roots.first()) else {
                return Ok(false);
            };
            if leaves.len() != 1 || roots.len() != 1 {
                return Ok(false);
            }
            let key = PKey::private_key_from_pem(value.key.as_bytes())?;
            let public = leaf.public_key()?;
            let group = key.ec_key()?;
            if group.group().curve_name() != Some(Nid::X9_62_PRIME256V1) || !key.public_eq(&public)
            {
                return Ok(false);
            }
            let Some(names) = leaf.subject_alt_names() else {
                return Ok(false);
            };
            if names.len() != 1 || names[0].dnsname() != Some(HOST) {
                return Ok(false);
            }
            let expiry = Asn1Time::from_unix(value.expires.timestamp())?;
            let root_public = root.public_key()?;
            if leaf.not_after().compare(&expiry)? != std::cmp::Ordering::Equal
                || !root.verify(&root_public)?
            {
                return Ok(false);
            }
            let mut parameters = X509VerifyParam::new()?;
            parameters.set_hostflags(X509CheckFlags::NO_WILDCARDS);
            parameters.set_host(HOST)?;
            parameters.set_purpose(openssl::x509::X509PurposeId::SSL_SERVER)?;
            parameters.set_time(now.timestamp());
            let mut store = X509StoreBuilder::new()?;
            store.add_cert(root.clone())?;
            store.set_param(&parameters)?;
            let chain = openssl::stack::Stack::new()?;
            openssl::x509::X509StoreContext::new()?.init(&store.build(), leaf, &chain, |context| {
                context.verify_cert()
            })
        };
        if value.expires <= now
            || value.expires > now + chrono::Duration::seconds(CERTIFICATE_SECONDS)
            || !check().map_err(|_| Refused)?
        {
            return Err(Refused);
        }
        Ok(())
    }
}

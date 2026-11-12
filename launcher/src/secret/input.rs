use super::sensitive::SecretString;
use anyhow::{bail, Context};
use std::io::Read;
use zeroize::Zeroizing;

pub const MAX_VALUE_BYTES: usize = 64 * 1024;

/// Only explicit namespaces can select a value; names never fall back across audiences.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reference {
    Personal(String),
    Organization(String),
    Id(uuid::Uuid),
}
impl std::str::FromStr for Reference {
    type Err = String;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let (namespace, name) = value
            .split_once('/')
            .ok_or("Use personal/NAME, org/NAME, or id/UUID")?;
        if name.is_empty() || name.contains('/') || name.chars().any(char::is_control) {
            return Err("Invalid secret reference".into());
        }
        match namespace {
            "personal" => Ok(Self::Personal(name.into())),
            "org" => Ok(Self::Organization(name.into())),
            "id" => name
                .parse()
                .map(Self::Id)
                .map_err(|_| "Invalid secret UUID".into()),
            _ => Err("Use personal/NAME, org/NAME, or id/UUID".into()),
        }
    }
}
impl std::fmt::Display for Reference {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Personal(name) => write!(f, "personal/{name}"),
            Self::Organization(name) => write!(f, "org/{name}"),
            Self::Id(id) => write!(f, "id/{id}"),
        }
    }
}

pub fn read_value(mut reader: impl Read, trim_newline: bool) -> anyhow::Result<SecretString> {
    let mut bytes = Zeroizing::new(Vec::new());
    reader
        .by_ref()
        .take((MAX_VALUE_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .context("Could not read secret input")?;
    if bytes.len() > MAX_VALUE_BYTES {
        bail!("Secret exceeds 64 KiB limit");
    }
    if trim_newline && bytes.last() == Some(&b'\n') {
        bytes.pop();
        if bytes.last() == Some(&b'\r') {
            bytes.pop();
        }
    }
    if bytes.is_empty() {
        bail!("Secret must not be empty");
    }
    let text = std::str::from_utf8(&bytes)
        .map_err(|_| anyhow::anyhow!("Secret input must be UTF-8 text; no bytes were uploaded"))?;
    Ok(SecretString::new(text.to_owned()))
}

pub fn env_name(value: &str) -> Result<String, String> {
    let mut chars = value.chars();
    if !chars
        .next()
        .is_some_and(|c| c == '_' || c.is_ascii_alphabetic())
        || !chars.all(|c| c == '_' || c.is_ascii_alphanumeric())
    {
        return Err("Environment names must match [A-Za-z_][A-Za-z0-9_]*".into());
    }
    Ok(value.into())
}

#[derive(Debug, Clone)]
pub struct EnvMapping {
    pub name: String,
    pub reference: Reference,
}
impl std::str::FromStr for EnvMapping {
    type Err = String;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let (name, reference) = value
            .split_once('=')
            .ok_or("Use --env NAME=personal/SECRET")?;
        let name = env_name(name)?;
        Ok(Self {
            name,
            reference: reference.parse()?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn stdin_is_byte_preserving_and_trimming_is_explicit() {
        assert_eq!(
            &*read_value(&b" \tvalue\r\n"[..], false).unwrap(),
            " \tvalue\r\n"
        );
        assert_eq!(&*read_value(&b"value\n\n"[..], true).unwrap(), "value\n");
        assert_eq!(&*read_value(&b"value\r\n"[..], true).unwrap(), "value");
        assert!(read_value(&b"\xff"[..], false).is_err());
        assert!(read_value(&b""[..], false).is_err());
        assert!(read_value(vec![b'a'; MAX_VALUE_BYTES + 1].as_slice(), false).is_err());
        assert_eq!(
            read_value(vec![b'a'; MAX_VALUE_BYTES].as_slice(), false)
                .unwrap()
                .len(),
            MAX_VALUE_BYTES
        );
    }
    #[test]
    fn references_require_exact_explicit_namespaces() {
        for bad in [
            "TOKEN",
            "shared/TOKEN",
            "personal/",
            "personal/a/b",
            "id/TOKEN",
            "org/a\n",
        ] {
            assert!(bad.parse::<Reference>().is_err(), "{bad:?}");
        }
        for good in [
            "personal/TOKEN",
            "org/TOKEN",
            "id/00000000-0000-0000-0000-000000000001",
        ] {
            assert_eq!(good.parse::<Reference>().unwrap().to_string(), good);
        }
    }
    #[test]
    fn environment_mapping_rejects_shell_and_invalid_names() {
        for bad in [
            "1KEY=personal/T",
            "A-B=org/T",
            "A;echo=org/T",
            "A=bare",
            "=org/T",
        ] {
            assert!(bad.parse::<EnvMapping>().is_err());
        }
        assert!("_KEY9=personal/T".parse::<EnvMapping>().is_ok());
    }
}

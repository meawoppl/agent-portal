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
        let value = value.strip_prefix("secret://").unwrap_or(value);
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

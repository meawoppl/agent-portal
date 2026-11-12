//! Keep erasure and diagnostic redaction together. `Zeroizing<String>` alone
//! still exposes the plaintext through Debug, so it must not be our value type.
use std::{fmt, ops::Deref};
use zeroize::{Zeroize, Zeroizing};

pub(super) struct SecretString(Zeroizing<String>);
impl SecretString {
    pub(super) fn new(value: String) -> Self {
        Self(Zeroizing::new(value))
    }
    /// Explicit exposure only at the approved HTTP or child-environment boundary.
    pub(super) fn as_str(&self) -> &str {
        self.0.as_str()
    }
}
impl Deref for SecretString {
    type Target = str;
    fn deref(&self) -> &str {
        self.as_str()
    }
}
impl Zeroize for SecretString {
    fn zeroize(&mut self) {
        self.0.zeroize();
    }
}
impl fmt::Debug for SecretString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<redacted>")
    }
}
impl fmt::Display for SecretString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<redacted>")
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn diagnostic_formatting_cannot_expose_values_tickets_or_credentials() {
        let value = SecretString::new("NEVER-LOG-THIS".into());
        assert_eq!(format!("{value:?}"), "<redacted>");
        assert_eq!(format!("{value}"), "<redacted>");
        assert_eq!(value.as_str(), "NEVER-LOG-THIS");
    }
}

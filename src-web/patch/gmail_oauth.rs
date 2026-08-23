//! Ensure Google issues a refresh token for Gmail OAuth accounts.
//!
//! The shared OAuth request only asks for scopes and PKCE. Google treats that
//! as online access and can therefore return an access token without a refresh
//! token, leaving the account unable to authenticate after the first token
//! expires.

const GOOGLE_OFFLINE_PARAMS: [(&str, &str); 2] =
    [("access_type", "offline"), ("prompt", "consent")];

pub(crate) fn authorization_extra_params(
    provider: &str,
) -> &'static [(&'static str, &'static str)] {
    if provider.eq_ignore_ascii_case("gmail") {
        &GOOGLE_OFFLINE_PARAMS
    } else {
        &[]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gmail_requests_offline_access_with_fresh_consent() {
        assert_eq!(
            authorization_extra_params("gmail"),
            &[("access_type", "offline"), ("prompt", "consent")]
        );
        assert!(authorization_extra_params("outlook").is_empty());
    }
}

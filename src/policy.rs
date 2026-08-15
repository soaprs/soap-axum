//! Translation of portable response policies into concrete HTTP headers.

use http::{
    HeaderMap, HeaderValue,
    header::{CACHE_CONTROL, STRICT_TRANSPORT_SECURITY, VARY},
};
use soaprs_core::{SoapError, SoapResult};
use soaprs_http::{
    CacheVisibility, EndpointMetadata, FrameOptions, ReferrerPolicy, ResponseCachePolicy,
    SecurityHeadersPolicy,
};

const X_CONTENT_TYPE_OPTIONS: &str = "x-content-type-options";
const X_FRAME_OPTIONS: &str = "x-frame-options";
const REFERRER_POLICY: &str = "referrer-policy";
const CONTENT_SECURITY_POLICY: &str = "content-security-policy";

pub(crate) fn apply_response_policies(
    endpoint: &EndpointMetadata,
    headers: &mut HeaderMap,
) -> SoapResult<()> {
    if let Some(policy) = &endpoint.security_headers {
        apply_security_headers(policy, headers)?;
    }
    if let Some(policy) = &endpoint.response_cache {
        apply_cache_headers(policy, headers)?;
    }
    Ok(())
}

fn apply_security_headers(
    policy: &SecurityHeadersPolicy,
    headers: &mut HeaderMap,
) -> SoapResult<()> {
    policy.validate()?;
    if policy.no_sniff {
        headers.insert(X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    }
    if let Some(frame_options) = policy.frame_options {
        let value = match frame_options {
            FrameOptions::Deny => "DENY",
            FrameOptions::SameOrigin => "SAMEORIGIN",
        };
        headers.insert(X_FRAME_OPTIONS, HeaderValue::from_static(value));
    }
    if let Some(referrer_policy) = policy.referrer_policy {
        let value = match referrer_policy {
            ReferrerPolicy::NoReferrer => "no-referrer",
            ReferrerPolicy::StrictOriginWhenCrossOrigin => "strict-origin-when-cross-origin",
        };
        headers.insert(REFERRER_POLICY, HeaderValue::from_static(value));
    }
    if let Some(content_security_policy) = &policy.content_security_policy {
        headers.insert(
            CONTENT_SECURITY_POLICY,
            header_value("content security policy", content_security_policy)?,
        );
    }
    if let Some(hsts) = policy.hsts {
        let mut value = format!("max-age={}", hsts.max_age.as_secs());
        if hsts.include_subdomains {
            value.push_str("; includeSubDomains");
        }
        if hsts.preload {
            value.push_str("; preload");
        }
        headers.insert(
            STRICT_TRANSPORT_SECURITY,
            header_value("HSTS policy", &value)?,
        );
    }
    Ok(())
}

fn apply_cache_headers(policy: &ResponseCachePolicy, headers: &mut HeaderMap) -> SoapResult<()> {
    policy.validate()?;
    let value = match (policy.visibility, policy.max_age) {
        (CacheVisibility::NoStore, _) => "no-store".to_owned(),
        (CacheVisibility::Private, Some(max_age)) => {
            format!("private, max-age={}", max_age.as_secs())
        }
        (CacheVisibility::Public, Some(max_age)) => {
            format!("public, max-age={}", max_age.as_secs())
        }
        _ => {
            return Err(SoapError::validation(
                "invalid response cache policy reached Axum translation",
            ));
        }
    };
    headers.insert(
        CACHE_CONTROL,
        header_value("response cache policy", &value)?,
    );
    merge_vary(headers, policy)
}

fn merge_vary(headers: &mut HeaderMap, policy: &ResponseCachePolicy) -> SoapResult<()> {
    if policy.vary.is_empty() {
        return Ok(());
    }
    let mut values = Vec::<String>::new();
    for value in headers.get_all(VARY) {
        let value = value
            .to_str()
            .map_err(|error| SoapError::validation("invalid Vary header").with_source(error))?;
        for name in value
            .split(',')
            .map(str::trim)
            .filter(|name| !name.is_empty())
        {
            if name == "*" {
                headers.insert(VARY, HeaderValue::from_static("*"));
                return Ok(());
            }
            push_unique(&mut values, name);
        }
    }
    for name in &policy.vary {
        push_unique(&mut values, name.as_str());
    }
    headers.insert(VARY, header_value("Vary policy", &values.join(", "))?);
    Ok(())
}

fn push_unique(values: &mut Vec<String>, candidate: &str) {
    if !values
        .iter()
        .any(|existing| existing.eq_ignore_ascii_case(candidate))
    {
        values.push(candidate.to_owned());
    }
}

fn header_value(kind: &str, value: &str) -> SoapResult<HeaderValue> {
    HeaderValue::from_str(value).map_err(|error| {
        SoapError::validation(format!("{kind} cannot be encoded as an HTTP header"))
            .with_source(error)
    })
}

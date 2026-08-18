//! Optional composition bridge for `soaprs-rate-limit`.

use std::{fmt, sync::Arc, time::Duration};

use http::{HeaderValue, header::RETRY_AFTER};
use soaprs_core::{BoxFuture, SoapError, SoapResult};
use soaprs_http::{HttpRequestView, RateLimitPolicy, RateLimitScope};
use soaprs_rate_limit::{
    RateLimitDecision, RateLimitKey, RateLimitRequest, RateLimitRule, RateLimitService, RateLimiter,
};

use crate::{EndpointGuard, EndpointGuardRejection, EndpointGuardResult, RouteRequestHead};

/// Derives an opaque limiter key from normalized HTTP and application context.
pub trait HttpRateLimitKeyResolver: Send + Sync {
    /// Resolves one key for the endpoint's declared scope.
    fn resolve<'a>(
        &'a self,
        policy: &'a RateLimitPolicy,
        request: &'a RouteRequestHead,
    ) -> BoxFuture<'a, SoapResult<RateLimitKey>>;
}

/// Resolves global and trusted client-IP scopes without application state.
///
/// Principal, API-key, and custom scopes require an application resolver that
/// can inspect authentication or other typed request extensions.
#[derive(Debug, Clone, Copy, Default)]
pub struct BuiltInRateLimitKeyResolver;

impl HttpRateLimitKeyResolver for BuiltInRateLimitKeyResolver {
    fn resolve<'a>(
        &'a self,
        policy: &'a RateLimitPolicy,
        request: &'a RouteRequestHead,
    ) -> BoxFuture<'a, SoapResult<RateLimitKey>> {
        Box::pin(async move {
            let endpoint = &request.endpoint().id;
            match &policy.scope {
                RateLimitScope::Global => {
                    RateLimitKey::new(format!("http:endpoint={endpoint}:scope=global"))
                }
                RateLimitScope::ClientIp => {
                    let client_ip = request.client_ip().ok_or_else(|| {
                        SoapError::infrastructure(
                            "client-IP rate limit requires trusted client IP normalization",
                        )
                    })?;
                    RateLimitKey::new(format!(
                        "http:endpoint={endpoint}:scope=client-ip:value={client_ip}"
                    ))
                }
                RateLimitScope::Principal | RateLimitScope::ApiKey | RateLimitScope::Custom(_) => {
                    Err(SoapError::unsupported(
                        "rate-limit scope requires an application HTTP key resolver",
                    ))
                }
            }
        })
    }
}

/// Checks endpoint quota through a neutral limiter before reading the body.
pub struct RateLimitGuard<L> {
    service: Arc<RateLimitService<L>>,
    key_resolver: Arc<dyn HttpRateLimitKeyResolver>,
}

impl<L> RateLimitGuard<L> {
    /// Creates a guard with global/client-IP key resolution.
    pub fn new(service: RateLimitService<L>) -> Self {
        Self {
            service: Arc::new(service),
            key_resolver: Arc::new(BuiltInRateLimitKeyResolver),
        }
    }

    /// Replaces built-in key resolution for principal, API-key, or custom scope.
    #[must_use]
    pub fn key_resolver<R>(mut self, key_resolver: R) -> Self
    where
        R: HttpRateLimitKeyResolver + 'static,
    {
        self.key_resolver = Arc::new(key_resolver);
        self
    }
}

impl<L> Clone for RateLimitGuard<L> {
    fn clone(&self) -> Self {
        Self {
            service: Arc::clone(&self.service),
            key_resolver: Arc::clone(&self.key_resolver),
        }
    }
}

impl<L> fmt::Debug for RateLimitGuard<L> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RateLimitGuard")
            .finish_non_exhaustive()
    }
}

impl<L> EndpointGuard for RateLimitGuard<L>
where
    L: RateLimiter + 'static,
{
    fn enforcement_capabilities(&self) -> &'static [soaprs_http::HttpEnforcementCapability] {
        &[soaprs_http::HttpEnforcementCapability::RateLimit]
    }

    fn check<'a>(
        &'a self,
        request: &'a mut RouteRequestHead,
    ) -> BoxFuture<'a, EndpointGuardResult> {
        Box::pin(async move {
            let Some(policy) = request.endpoint().rate_limit.as_ref() else {
                return Ok(());
            };
            let rule = match rule(policy) {
                Ok(rule) => rule,
                Err(error) => return Err(EndpointGuardRejection::new(error)),
            };
            let key = match self.key_resolver.resolve(policy, request).await {
                Ok(key) => key,
                Err(error) => return Err(EndpointGuardRejection::new(error)),
            };
            let decision = match self.service.check(RateLimitRequest::new(&key, &rule)).await {
                Ok(decision) => decision,
                Err(error) => return Err(EndpointGuardRejection::new(error)),
            };
            match decision {
                RateLimitDecision::Allowed { .. } => Ok(()),
                RateLimitDecision::Rejected { retry_after } => {
                    let retry_after = match retry_after_header(retry_after) {
                        Ok(retry_after) => retry_after,
                        Err(error) => return Err(EndpointGuardRejection::new(error)),
                    };
                    let mut rejection = EndpointGuardRejection::new(SoapError::rate_limited());
                    rejection
                        .effects_mut()
                        .headers
                        .insert(RETRY_AFTER, retry_after);
                    Err(rejection)
                }
            }
        })
    }
}

fn rule(policy: &RateLimitPolicy) -> SoapResult<RateLimitRule> {
    let mut rule = RateLimitRule::new(policy.requests, policy.period)?;
    if let Some(burst) = policy.burst {
        rule = rule.burst(burst);
    }
    Ok(rule)
}

fn retry_after_header(duration: Duration) -> SoapResult<HeaderValue> {
    let seconds = duration
        .as_secs()
        .saturating_add(u64::from(duration.subsec_nanos() > 0))
        .max(1);
    HeaderValue::from_str(&seconds.to_string()).map_err(|error| {
        SoapError::infrastructure("rate-limit retry delay cannot be encoded").with_source(error)
    })
}

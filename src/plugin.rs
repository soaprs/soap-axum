//! Build-time router plugins.

use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

use axum::Router;
use soaprs_core::{SoapError, SoapResult};
use soaprs_http::{EndpointCatalog, EndpointId, HttpEnforcementCapability};

use crate::{EndpointHook, EndpointMiddleware};

pub(crate) type RouterTransform = Box<dyn Fn(Router) -> SoapResult<Router> + Send + Sync + 'static>;

/// Build-time extension that installs middleware and hooks without owning the
/// Axum server lifecycle.
pub trait RouterPlugin: Send + Sync {
    /// Stable plugin name used for duplicate detection.
    fn name(&self) -> &'static str;

    /// Installs the plugin into a router builder.
    fn install(&self, context: &mut PluginContext<'_>) -> SoapResult<()>;
}

/// Restricted composition surface exposed to router plugins.
pub struct PluginContext<'a> {
    pub(crate) catalog: &'a EndpointCatalog,
    pub(crate) global_middleware: &'a mut Vec<Arc<dyn EndpointMiddleware>>,
    pub(crate) global_hooks: &'a mut Vec<Arc<dyn EndpointHook>>,
    pub(crate) endpoint_middleware: &'a mut HashMap<EndpointId, Vec<Arc<dyn EndpointMiddleware>>>,
    pub(crate) endpoint_hooks: &'a mut HashMap<EndpointId, Vec<Arc<dyn EndpointHook>>>,
    pub(crate) router_transforms: &'a mut Vec<RouterTransform>,
    pub(crate) router_enforcement_capabilities: &'a mut HashSet<HttpEnforcementCapability>,
}

impl PluginContext<'_> {
    /// Returns the catalog being composed.
    pub fn catalog(&self) -> &EndpointCatalog {
        self.catalog
    }

    /// Appends global middleware.
    pub fn middleware<M>(&mut self, middleware: M)
    where
        M: EndpointMiddleware + 'static,
    {
        self.global_middleware.push(Arc::new(middleware));
    }

    /// Appends a global observational hook.
    pub fn hook<H>(&mut self, hook: H)
    where
        H: EndpointHook + 'static,
    {
        self.global_hooks.push(Arc::new(hook));
    }

    /// Applies one framework-level transformation after catalog routes have
    /// been built.
    ///
    /// This extension point is intended for behavior that cannot run inside a
    /// matched endpoint pipeline, including CORS preflight routes and outer
    /// telemetry layers.
    pub fn transform_router<F>(&mut self, transform: F)
    where
        F: Fn(Router) -> SoapResult<Router> + Send + Sync + 'static,
    {
        self.router_transforms.push(Box::new(transform));
    }

    /// Declares enforcement provided at the framework-router level.
    ///
    /// CORS coverage must be declared here because endpoint middleware cannot
    /// serve unmatched preflight `OPTIONS` requests.
    pub fn router_enforcement_capability(&mut self, capability: HttpEnforcementCapability) {
        self.router_enforcement_capabilities.insert(capability);
    }

    /// Appends middleware to one declared endpoint.
    pub fn endpoint_middleware<M>(&mut self, endpoint_id: &str, middleware: M) -> SoapResult<()>
    where
        M: EndpointMiddleware + 'static,
    {
        let id = self.require_endpoint(endpoint_id)?;
        self.endpoint_middleware
            .entry(id)
            .or_default()
            .push(Arc::new(middleware));
        Ok(())
    }

    /// Appends a hook to one declared endpoint.
    pub fn endpoint_hook<H>(&mut self, endpoint_id: &str, hook: H) -> SoapResult<()>
    where
        H: EndpointHook + 'static,
    {
        let id = self.require_endpoint(endpoint_id)?;
        self.endpoint_hooks
            .entry(id)
            .or_default()
            .push(Arc::new(hook));
        Ok(())
    }

    fn require_endpoint(&self, endpoint_id: &str) -> SoapResult<EndpointId> {
        let id = EndpointId::new(endpoint_id)?;
        if self.catalog.endpoint(&id).is_none() {
            return Err(SoapError::not_found(format!(
                "endpoint `{endpoint_id}` is not declared"
            )));
        }
        Ok(id)
    }
}

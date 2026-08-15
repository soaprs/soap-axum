//! Build-time router plugins.

use std::{collections::HashMap, sync::Arc};

use soaprs_core::{SoapError, SoapResult};
use soaprs_http::{EndpointCatalog, EndpointId};

use crate::{EndpointHook, EndpointMiddleware};

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

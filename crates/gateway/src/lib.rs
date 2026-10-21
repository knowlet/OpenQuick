//! Integration boundary for the external AgentGateway component.
//!
//! This slice deliberately has no transport, protocol implementation, credential
//! store, or network client. A core execution permit alone cannot enable it.

use openquick_core::ExecutionPermit;
use std::fmt;

/// No external component is connected in this slice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IntegrationUnavailable;

impl fmt::Display for IntegrationUnavailable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("AgentGateway integration unavailable; no tool was dispatched")
    }
}

impl std::error::Error for IntegrationUnavailable {}

/// The adapter remains closed until an actual upstream integration is built.
#[derive(Debug, Default)]
pub struct Gateway;

impl Gateway {
    pub fn availability(&self) -> Result<(), IntegrationUnavailable> {
        Err(IntegrationUnavailable)
    }

    pub fn dispatch(&self, _permit: &ExecutionPermit) -> Result<(), IntegrationUnavailable> {
        Err(IntegrationUnavailable)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gateway_reports_unavailable() {
        assert_eq!(Gateway.availability(), Err(IntegrationUnavailable));
    }
}

//! Synchronous facade over the async [`IsoTp`] transport.
//! [`BlockingIsoTp`] wraps an [`IsoTp`] and exposes the synchronous API: the
//! async request/response exchange is driven to completion on the calling
//! thread via [`autors_runtime::block_on`], while the inherently synchronous
//! parts (frame feeding, read-only accessors) delegate directly.
//! Do not call [`BlockingIsoTp::send_request`] from within async code running
//! on the shared runtime: [`autors_runtime::block_on`] panics on its executor
//! threads, the same restriction as `tokio::runtime::Runtime::block_on`.

use autors_can::device::CanDevice;
use autors_can::frame::CanFrame;
use autors_lin::device::LinDevice;

use crate::isotp::{IsoTpType, MsgState};
use crate::lin_transport::LinTp;
use crate::transport::IsoTp;
use crate::Result;

/// Synchronous wrapper around an [`IsoTp`] transport.
/// The wrapped transport is publicly accessible (`.0`), so the public
/// configuration fields (`p2_client`, `use_fill_byte`, `fill_byte`, ...)
/// remain directly reachable.
pub struct BlockingIsoTp(pub IsoTp);

impl BlockingIsoTp {
    /// Wraps `transport` in the synchronous facade.
    pub fn new(transport: IsoTp) -> Self {
        Self(transport)
    }

    /// Unwraps the facade, returning the inner transport.
    pub fn into_inner(self) -> IsoTp {
        self.0
    }

    /// Drives [`IsoTp::send_request`] to completion on the calling thread.
    pub fn send_request<D: CanDevice + Send>(
        &mut self,
        device: &mut D,
        req_data: Vec<u8>,
        res_data: Option<&mut Vec<u8>>,
        await_response: bool,
    ) -> Result<MsgState> {
        autors_runtime::block_on(
            self.0
                .send_request(device, req_data, res_data, await_response),
        )
    }

    /// Delegates to [`IsoTp::on_received`] (inherently synchronous).
    pub fn on_received(&mut self, frame: &CanFrame) -> Result<bool> {
        self.0.on_received(frame)
    }

    /// Delegates to [`IsoTp::tp_type`].
    pub fn tp_type(&self) -> IsoTpType {
        self.0.tp_type()
    }

    /// Delegates to [`IsoTp::max_msg_len`].
    pub fn max_msg_len(&self) -> u32 {
        self.0.max_msg_len()
    }
}

/// Synchronous wrapper around a [`LinTp`] transport.
pub struct BlockingLinTp(pub LinTp);

impl BlockingLinTp {
    /// Wraps `transport` in the synchronous facade.
    pub fn new(transport: LinTp) -> Self {
        Self(transport)
    }

    /// Unwraps the facade, returning the inner transport.
    pub fn into_inner(self) -> LinTp {
        self.0
    }

    /// Drives [`LinTp::send_request`] to completion on the calling thread.
    pub fn send_request<D: LinDevice + Send>(
        &mut self,
        device: &mut D,
        request: Vec<u8>,
        response: Option<&mut Vec<u8>>,
        await_response: bool,
    ) -> Result<MsgState> {
        autors_runtime::block_on(
            self.0
                .send_request(device, request, response, await_response),
        )
    }

    /// Delegates to [`LinTp::tp_type`].
    pub fn tp_type(&self) -> IsoTpType {
        self.0.tp_type()
    }

    /// Delegates to [`LinTp::max_msg_len`].
    pub fn max_msg_len(&self) -> u32 {
        self.0.max_msg_len()
    }
}

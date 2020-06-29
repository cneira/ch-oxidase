// Copyright © 2019 Intel Corporation
//
// SPDX-License-Identifier: Apache-2.0
//

//! The internal VMM API for Cloud Hypervisor.
//!
//! This API is a synchronous, [mpsc](https://doc.rust-lang.org/std/sync/mpsc/)
//! based IPC for sending commands to the VMM thread, from other
//! Cloud Hypervisor threads. The IPC follows a command-response protocol, i.e.
//! each command will receive a response back.
//!
//! The main Cloud Hypervisor thread creates an API event file descriptor
//! to notify the VMM thread about pending API commands, together with an
//! API mpsc channel. The former is the IPC control plane, the latter is the
//! IPC data plane.
//! In order to use the IPC, a Cloud Hypervisor thread needs to have a clone
//! of both the API event file descriptor and the channel Sender. Then it must
//! go through the following steps:
//!
//! 1. The thread creates an mpsc channel for receiving the command response.
//! 2. The thread sends an ApiRequest to the Sender endpoint. The ApiRequest
//!    contains the response channel Sender, for the VMM API server to be able
//!    to send the response back.
//! 3. The thread writes to the API event file descriptor to notify the VMM
//!    API server about a pending command.
//! 4. The thread reads the response back from the VMM API server, from the
//!    response channel Receiver.
//! 5. The thread handles the response and forwards potential errors.

extern crate micro_http;
extern crate vm_device;
extern crate vmm_sys_util;

pub use self::http::start_http_thread;

pub mod http;
pub mod http_endpoint;

use crate::config::{
    DeviceConfig, DiskConfig, FsConfig, NetConfig, PmemConfig, VmConfig, VsockConfig,
};
use crate::vm::{Error as VmError, VmState};
use std::io;
use std::sync::mpsc::{channel, RecvError, SendError, Sender};
use std::sync::{Arc, Mutex};
use vmm_sys_util::eventfd::EventFd;

/// API errors are sent back from the VMM API server through the ApiResponse.
#[derive(Debug)]
pub enum ApiError {
    /// Cannot write to EventFd.
    EventFdWrite(io::Error),

    /// API request send error
    RequestSend(SendError<ApiRequest>),

    /// Wrong reponse payload type
    ResponsePayloadType,

    /// API response receive error
    ResponseRecv(RecvError),

    /// The VM could not boot.
    VmBoot(VmError),

    /// The VM is already created.
    VmAlreadyCreated,

    /// The VM could not be created.
    VmCreate(VmError),

    /// The VM could not be deleted.
    VmDelete(VmError),

    /// The VM info is not available.
    VmInfo(VmError),

    /// The VM config is missing.
    VmMissingConfig,

    /// The VM is not booted.
    VmNotBooted,

    /// The VM is not created.
    VmNotCreated,

    /// The VM could not shutdown.
    VmShutdown(VmError),

    /// The VM could not reboot.
    VmReboot(VmError),

    /// The VMM could not shutdown.
    VmmShutdown(VmError),

    /// The device could not be added to the VM.
    VmAddDevice(VmError),

    /// The device could not be removed from the VM.
    VmRemoveDevice(VmError),

    /// The disk could not be added to the VM.
    VmAddDisk(VmError),

    /// The fs could not be added to the VM.
    VmAddFs(VmError),

    /// The pmem device could not be added to the VM.
    VmAddPmem(VmError),

    /// The network device could not be added to the VM.
    VmAddNet(VmError),

    /// The vsock device could not be added to the VM.
    VmAddVsock(VmError),
}
pub type ApiResult<T> = std::result::Result<T, ApiError>;

#[derive(Clone, Deserialize, Serialize)]
pub struct VmInfo {
    pub config: Arc<Mutex<VmConfig>>,
    pub state: VmState,
}

#[derive(Clone, Deserialize, Serialize)]
pub struct VmmPingResponse {
    pub version: String,
}

#[derive(Clone, Deserialize, Serialize)]
pub struct VmRemoveDeviceData {
    pub id: String,
}

pub enum ApiResponsePayload {
    /// No data is sent on the channel.
    Empty,

    /// Virtual machine information
    VmInfo(VmInfo),

    /// Vmm ping response
    VmmPing(VmmPingResponse),
}

/// This is the response sent by the VMM API server through the mpsc channel.
pub type ApiResponse = std::result::Result<ApiResponsePayload, ApiError>;

#[allow(clippy::large_enum_variant)]
pub enum ApiRequest {
    /// Create the virtual machine. This request payload is a VM configuration
    /// (VmConfig).
    /// If the VMM API server could not create the VM, it will send a VmCreate
    /// error back.
    VmCreate(Arc<Mutex<VmConfig>>, Sender<ApiResponse>),

    /// Boot the previously created virtual machine.
    /// If the VM was not previously created, the VMM API server will send a
    /// VmBoot error back.
    VmBoot(Sender<ApiResponse>),

    /// Delete the previously created virtual machine.
    /// If the VM was not previously created, the VMM API server will send a
    /// VmDelete error back.
    /// If the VM is booted, we shut it down first.
    VmDelete(Sender<ApiResponse>),

    /// Request the VM information.
    VmInfo(Sender<ApiResponse>),

    /// Request the VMM API server status
    VmmPing(Sender<ApiResponse>),

    /// Shut the previously booted virtual machine down.
    /// If the VM was not previously booted or created, the VMM API server
    /// will send a VmShutdown error back.
    VmShutdown(Sender<ApiResponse>),

    /// Reboot the previously booted virtual machine.
    /// If the VM was not previously booted or created, the VMM API server
    /// will send a VmReboot error back.
    VmReboot(Sender<ApiResponse>),

    /// Shut the VMM down.
    /// This will shutdown and delete the current VM, if any, and then exit the
    /// VMM process.
    VmmShutdown(Sender<ApiResponse>),

    /// Add a device to the VM.
    VmAddDevice(Arc<DeviceConfig>, Sender<ApiResponse>),

    /// Remove a device from the VM.
    VmRemoveDevice(Arc<VmRemoveDeviceData>, Sender<ApiResponse>),

    /// Add a disk to the VM.
    VmAddDisk(Arc<DiskConfig>, Sender<ApiResponse>),

    /// Add a fs to the VM.
    VmAddFs(Arc<FsConfig>, Sender<ApiResponse>),

    /// Add a pmem device to the VM.
    VmAddPmem(Arc<PmemConfig>, Sender<ApiResponse>),

    /// Add a network device to the VM.
    VmAddNet(Arc<NetConfig>, Sender<ApiResponse>),

    /// Add a vsock device to the VM.
    VmAddVsock(Arc<VsockConfig>, Sender<ApiResponse>),
}

pub fn vm_create(
    api_evt: EventFd,
    api_sender: Sender<ApiRequest>,
    config: Arc<Mutex<VmConfig>>,
) -> ApiResult<()> {
    let (response_sender, response_receiver) = channel();

    // Send the VM creation request.
    api_sender
        .send(ApiRequest::VmCreate(config, response_sender))
        .map_err(ApiError::RequestSend)?;
    api_evt.write(1).map_err(ApiError::EventFdWrite)?;

    response_receiver.recv().map_err(ApiError::ResponseRecv)??;

    Ok(())
}

/// Represents a VM related action.
/// This is mostly used to factorize code between VM routines
/// that only differ by the IPC command they send.
pub enum VmAction {
    /// Boot a VM
    Boot,

    /// Delete a VM
    Delete,

    /// Shut a VM down
    Shutdown,

    /// Reboot a VM
    Reboot,
}

fn vm_action(api_evt: EventFd, api_sender: Sender<ApiRequest>, action: VmAction) -> ApiResult<()> {
    let (response_sender, response_receiver) = channel();

    let request = match action {
        VmAction::Boot => ApiRequest::VmBoot(response_sender),
        VmAction::Delete => ApiRequest::VmDelete(response_sender),
        VmAction::Shutdown => ApiRequest::VmShutdown(response_sender),
        VmAction::Reboot => ApiRequest::VmReboot(response_sender),
    };

    // Send the VM request.
    api_sender.send(request).map_err(ApiError::RequestSend)?;
    api_evt.write(1).map_err(ApiError::EventFdWrite)?;

    response_receiver.recv().map_err(ApiError::ResponseRecv)??;

    Ok(())
}

pub fn vm_boot(api_evt: EventFd, api_sender: Sender<ApiRequest>) -> ApiResult<()> {
    vm_action(api_evt, api_sender, VmAction::Boot)
}

pub fn vm_delete(api_evt: EventFd, api_sender: Sender<ApiRequest>) -> ApiResult<()> {
    vm_action(api_evt, api_sender, VmAction::Delete)
}

pub fn vm_shutdown(api_evt: EventFd, api_sender: Sender<ApiRequest>) -> ApiResult<()> {
    vm_action(api_evt, api_sender, VmAction::Shutdown)
}

pub fn vm_reboot(api_evt: EventFd, api_sender: Sender<ApiRequest>) -> ApiResult<()> {
    vm_action(api_evt, api_sender, VmAction::Reboot)
}

pub fn vm_info(api_evt: EventFd, api_sender: Sender<ApiRequest>) -> ApiResult<VmInfo> {
    let (response_sender, response_receiver) = channel();

    // Send the VM request.
    api_sender
        .send(ApiRequest::VmInfo(response_sender))
        .map_err(ApiError::RequestSend)?;
    api_evt.write(1).map_err(ApiError::EventFdWrite)?;

    let vm_info = response_receiver.recv().map_err(ApiError::ResponseRecv)??;

    match vm_info {
        ApiResponsePayload::VmInfo(info) => Ok(info),
        _ => Err(ApiError::ResponsePayloadType),
    }
}

pub fn vmm_ping(api_evt: EventFd, api_sender: Sender<ApiRequest>) -> ApiResult<VmmPingResponse> {
    let (response_sender, response_receiver) = channel();

    api_sender
        .send(ApiRequest::VmmPing(response_sender))
        .map_err(ApiError::RequestSend)?;
    api_evt.write(1).map_err(ApiError::EventFdWrite)?;

    let vmm_pong = response_receiver.recv().map_err(ApiError::ResponseRecv)??;

    match vmm_pong {
        ApiResponsePayload::VmmPing(pong) => Ok(pong),
        _ => Err(ApiError::ResponsePayloadType),
    }
}

pub fn vmm_shutdown(api_evt: EventFd, api_sender: Sender<ApiRequest>) -> ApiResult<()> {
    let (response_sender, response_receiver) = channel();

    // Send the VMM shutdown request.
    api_sender
        .send(ApiRequest::VmmShutdown(response_sender))
        .map_err(ApiError::RequestSend)?;
    api_evt.write(1).map_err(ApiError::EventFdWrite)?;

    response_receiver.recv().map_err(ApiError::ResponseRecv)??;

    Ok(())
}

pub fn vm_add_device(
    api_evt: EventFd,
    api_sender: Sender<ApiRequest>,
    data: Arc<DeviceConfig>,
) -> ApiResult<()> {
    let (response_sender, response_receiver) = channel();

    // Send the VM add-device request.
    api_sender
        .send(ApiRequest::VmAddDevice(data, response_sender))
        .map_err(ApiError::RequestSend)?;
    api_evt.write(1).map_err(ApiError::EventFdWrite)?;

    response_receiver.recv().map_err(ApiError::ResponseRecv)??;

    Ok(())
}

pub fn vm_remove_device(
    api_evt: EventFd,
    api_sender: Sender<ApiRequest>,
    data: Arc<VmRemoveDeviceData>,
) -> ApiResult<()> {
    let (response_sender, response_receiver) = channel();

    // Send the VM remove-device request.
    api_sender
        .send(ApiRequest::VmRemoveDevice(data, response_sender))
        .map_err(ApiError::RequestSend)?;
    api_evt.write(1).map_err(ApiError::EventFdWrite)?;

    response_receiver.recv().map_err(ApiError::ResponseRecv)??;

    Ok(())
}

pub fn vm_add_disk(
    api_evt: EventFd,
    api_sender: Sender<ApiRequest>,
    data: Arc<DiskConfig>,
) -> ApiResult<()> {
    let (response_sender, response_receiver) = channel();

    // Send the VM add-disk request.
    api_sender
        .send(ApiRequest::VmAddDisk(data, response_sender))
        .map_err(ApiError::RequestSend)?;
    api_evt.write(1).map_err(ApiError::EventFdWrite)?;

    response_receiver.recv().map_err(ApiError::ResponseRecv)??;

    Ok(())
}

pub fn vm_add_fs(
    api_evt: EventFd,
    api_sender: Sender<ApiRequest>,
    data: Arc<FsConfig>,
) -> ApiResult<()> {
    let (response_sender, response_receiver) = channel();

    // Send the VM add-fs request.
    api_sender
        .send(ApiRequest::VmAddFs(data, response_sender))
        .map_err(ApiError::RequestSend)?;
    api_evt.write(1).map_err(ApiError::EventFdWrite)?;

    response_receiver.recv().map_err(ApiError::ResponseRecv)??;

    Ok(())
}

pub fn vm_add_pmem(
    api_evt: EventFd,
    api_sender: Sender<ApiRequest>,
    data: Arc<PmemConfig>,
) -> ApiResult<()> {
    let (response_sender, response_receiver) = channel();

    // Send the VM add-pmem request.
    api_sender
        .send(ApiRequest::VmAddPmem(data, response_sender))
        .map_err(ApiError::RequestSend)?;
    api_evt.write(1).map_err(ApiError::EventFdWrite)?;

    response_receiver.recv().map_err(ApiError::ResponseRecv)??;

    Ok(())
}

pub fn vm_add_net(
    api_evt: EventFd,
    api_sender: Sender<ApiRequest>,
    data: Arc<NetConfig>,
) -> ApiResult<()> {
    let (response_sender, response_receiver) = channel();

    // Send the VM add-net request.
    api_sender
        .send(ApiRequest::VmAddNet(data, response_sender))
        .map_err(ApiError::RequestSend)?;
    api_evt.write(1).map_err(ApiError::EventFdWrite)?;

    response_receiver.recv().map_err(ApiError::ResponseRecv)??;

    Ok(())
}

pub fn vm_add_vsock(
    api_evt: EventFd,
    api_sender: Sender<ApiRequest>,
    data: Arc<VsockConfig>,
) -> ApiResult<()> {
    let (response_sender, response_receiver) = channel();

    // Send the VM add-vsock request.
    api_sender
        .send(ApiRequest::VmAddVsock(data, response_sender))
        .map_err(ApiError::RequestSend)?;
    api_evt.write(1).map_err(ApiError::EventFdWrite)?;

    response_receiver.recv().map_err(ApiError::ResponseRecv)??;

    Ok(())
}
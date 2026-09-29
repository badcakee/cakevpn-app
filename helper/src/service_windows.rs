//! Windows service glue. The installer runs `cakevpn-helper install`, which
//! registers the service to start with Windows under the SYSTEM account.

use cakevpn_proto::WINDOWS_SERVICE;
use std::ffi::OsString;
use std::time::Duration;
use windows_service::service::{
    ServiceAccess, ServiceControl, ServiceControlAccept, ServiceErrorControl, ServiceExitCode, ServiceInfo,
    ServiceStartType, ServiceState, ServiceStatus, ServiceType,
};
use windows_service::service_control_handler::{self, ServiceControlHandlerResult};
use windows_service::service_manager::{ServiceManager, ServiceManagerAccess};
use windows_service::{define_windows_service, service_dispatcher};

define_windows_service!(ffi_service_main, service_main);

pub fn exit_with(result: windows_service::Result<()>) {
    if let Err(e) = result {
        eprintln!("cakevpn-helper: {e}");
        std::process::exit(1);
    }
}

pub fn run_service() {
    if let Err(e) = service_dispatcher::start(WINDOWS_SERVICE, ffi_service_main) {
        eprintln!("cakevpn-helper: not started by Windows as a service: {e}");
        std::process::exit(1);
    }
}

fn service_main(_args: Vec<OsString>) {
    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();
    let stop_tx = std::sync::Mutex::new(Some(stop_tx));
    let handler = move |control| match control {
        ServiceControl::Stop | ServiceControl::Shutdown => {
            if let Some(tx) = stop_tx.lock().unwrap().take() {
                let _ = tx.send(());
            }
            ServiceControlHandlerResult::NoError
        }
        ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
        _ => ServiceControlHandlerResult::NotImplemented,
    };
    let Ok(status) = service_control_handler::register(WINDOWS_SERVICE, handler) else { return };
    let report = |state, accept| {
        let _ = status.set_service_status(ServiceStatus {
            service_type: ServiceType::OWN_PROCESS,
            current_state: state,
            controls_accepted: accept,
            exit_code: ServiceExitCode::Win32(0),
            checkpoint: 0,
            wait_hint: Duration::from_secs(10),
            process_id: None,
        });
    };
    report(ServiceState::Running, ServiceControlAccept::STOP | ServiceControlAccept::SHUTDOWN);
    if let Ok(runtime) = tokio::runtime::Runtime::new() {
        let _ = runtime.block_on(crate::serve(async {
            let _ = stop_rx.await;
        }));
    }
    report(ServiceState::Stopped, ServiceControlAccept::empty());
}

pub fn install() -> windows_service::Result<()> {
    let manager = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT | ServiceManagerAccess::CREATE_SERVICE)?;
    let access = ServiceAccess::QUERY_STATUS | ServiceAccess::START | ServiceAccess::STOP | ServiceAccess::CHANGE_CONFIG;
    let service = match manager.open_service(WINDOWS_SERVICE, access) {
        Ok(service) => service,
        Err(_) => {
            let info = ServiceInfo {
                name: OsString::from(WINDOWS_SERVICE),
                display_name: OsString::from("CakeVPN Helper"),
                service_type: ServiceType::OWN_PROCESS,
                start_type: ServiceStartType::AutoStart,
                error_control: ServiceErrorControl::Normal,
                executable_path: std::env::current_exe().map_err(windows_service::Error::Winapi)?,
                launch_arguments: vec![OsString::from("service")],
                dependencies: vec![],
                account_name: None, // LocalSystem
                account_password: None,
            };
            let service = manager.create_service(&info, access)?;
            service.set_description("Runs the CakeVPN tunnel for the CakeVPN app.")?;
            service
        }
    };
    if service.query_status()?.current_state != ServiceState::Running {
        service.start::<&str>(&[])?;
    }
    Ok(())
}

pub fn uninstall() -> windows_service::Result<()> {
    let manager = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT)?;
    let service = manager.open_service(WINDOWS_SERVICE, ServiceAccess::QUERY_STATUS | ServiceAccess::STOP | ServiceAccess::DELETE)?;
    if service.query_status()?.current_state != ServiceState::Stopped {
        let _ = service.stop();
        for _ in 0..20 {
            if service.query_status()?.current_state == ServiceState::Stopped {
                break;
            }
            std::thread::sleep(Duration::from_millis(250));
        }
    }
    service.delete()
}

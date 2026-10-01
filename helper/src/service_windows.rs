//! Windows service glue. The installer runs `cakevpn-helper install`, which
//! registers the service to start with Windows under the SYSTEM account.

use cakevpn_proto::WINDOWS_SERVICE;
use std::ffi::OsString;
use std::time::{Duration, Instant};
use windows_service::service::{
    Service, ServiceAccess, ServiceAction, ServiceActionType, ServiceControl, ServiceControlAccept, ServiceErrorControl,
    ServiceExitCode, ServiceFailureActions, ServiceFailureResetPeriod, ServiceInfo, ServiceStartType, ServiceState,
    ServiceStatus, ServiceType,
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
    let report = |state, accept, exit_code| {
        let _ = status.set_service_status(ServiceStatus {
            service_type: ServiceType::OWN_PROCESS,
            current_state: state,
            controls_accepted: accept,
            exit_code: ServiceExitCode::Win32(exit_code),
            checkpoint: 0,
            wait_hint: Duration::from_secs(10),
            process_id: None,
        });
    };
    report(ServiceState::Running, ServiceControlAccept::STOP | ServiceControlAccept::SHUTDOWN, 0);
    // On a thread with a big stack: Windows gives this one only 1 MB.
    let served = crate::on_big_stack(move || match crate::runtime() {
        Ok(runtime) => {
            let served = runtime.block_on(crate::serve(async {
                let _ = stop_rx.await;
            }));
            // Work still in flight must not keep a stopped service's process around.
            runtime.shutdown_timeout(Duration::from_secs(2));
            served.is_ok()
        }
        Err(_) => false,
    })
    .unwrap_or(false);
    // Stopping with an error has Windows start the helper again (see restart_after_failures).
    report(ServiceState::Stopped, ServiceControlAccept::empty(), if served { 0 } else { 1 });
}

/// How long the helper gets to stop by itself before it is ended.
const STOP_WAIT: Duration = Duration::from_secs(10);
/// How long a started helper gets to report that it runs.
const START_WAIT: Duration = Duration::from_secs(15);

const ERROR_SERVICE_ALREADY_RUNNING: i32 = 1056;
const ERROR_SERVICE_MARKED_FOR_DELETE: i32 = 1072;
const ERROR_SERVICE_EXISTS: i32 = 1073;

const ACCESS: ServiceAccess = ServiceAccess::QUERY_STATUS
    .union(ServiceAccess::START)
    .union(ServiceAccess::STOP)
    .union(ServiceAccess::CHANGE_CONFIG);

fn os_code(error: &windows_service::Error) -> Option<i32> {
    match error {
        windows_service::Error::Winapi(e) => e.raw_os_error(),
        _ => None,
    }
}

fn failed(message: &str) -> windows_service::Error {
    windows_service::Error::Winapi(std::io::Error::other(message.to_string()))
}

fn service_info() -> windows_service::Result<ServiceInfo> {
    Ok(ServiceInfo {
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
    })
}

/// Waits until the service is in `wanted`. False when it isn't after `limit`.
fn wait_for(service: &Service, wanted: ServiceState, limit: Duration) -> bool {
    let end = Instant::now() + limit;
    loop {
        if service.query_status().is_ok_and(|s| s.current_state == wanted) {
            return true;
        }
        if Instant::now() >= end {
            return false;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// Stops the service and makes sure its process is gone. A helper that does
/// not stop by itself is ended together with the sing-box it started; Windows
/// removes their network adapter and firewall rules with them.
fn stop_fully(service: &Service) {
    let Ok(status) = service.query_status() else { return };
    if status.current_state == ServiceState::Stopped {
        return;
    }
    let _ = service.stop();
    if wait_for(service, ServiceState::Stopped, STOP_WAIT) {
        return;
    }
    // Ending it must not make Windows start it again (see restart_after_failures).
    let _ = service.update_failure_actions(ServiceFailureActions {
        reset_period: ServiceFailureResetPeriod::Never,
        reboot_msg: None,
        command: None,
        actions: Some(vec![]),
    });
    if let Some(pid) = status.process_id {
        let _ = std::process::Command::new("taskkill")
            .args(["/F", "/T", "/PID", &pid.to_string()])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    }
    wait_for(service, ServiceState::Stopped, Duration::from_secs(5));
}

/// Registers the service. While Windows is still removing an older copy of
/// it, registering is refused for a moment, so it is tried again.
fn create(manager: &ServiceManager, info: &ServiceInfo) -> windows_service::Result<Service> {
    let mut tries = 0;
    loop {
        match manager.create_service(info, ACCESS) {
            Ok(service) => return Ok(service),
            Err(e) if os_code(&e) == Some(ERROR_SERVICE_EXISTS) => return manager.open_service(WINDOWS_SERVICE, ACCESS),
            Err(e) if os_code(&e) == Some(ERROR_SERVICE_MARKED_FOR_DELETE) && tries < 40 => {
                tries += 1;
                std::thread::sleep(Duration::from_millis(500));
            }
            Err(e) => return Err(e),
        }
    }
}

/// Has Windows start the helper again if it ever stops by itself.
fn restart_after_failures(service: &Service) -> windows_service::Result<()> {
    let restart = ServiceAction { action_type: ServiceActionType::Restart, delay: Duration::from_secs(2) };
    service.update_failure_actions(ServiceFailureActions {
        reset_period: ServiceFailureResetPeriod::After(Duration::from_secs(3600)),
        reboot_msg: None,
        command: None,
        actions: Some(vec![restart.clone(), restart.clone(), restart]),
    })?;
    service.set_failure_actions_on_non_crash_failures(true)
}

fn start(service: &Service) -> windows_service::Result<()> {
    let mut result = Ok(());
    for _ in 0..5 {
        result = match service.start::<&str>(&[]) {
            Err(e) if os_code(&e) == Some(ERROR_SERVICE_ALREADY_RUNNING) => Ok(()),
            other => other,
        };
        if result.is_ok() {
            break;
        }
        std::thread::sleep(Duration::from_secs(1));
    }
    result?;
    if wait_for(service, ServiceState::Running, START_WAIT) {
        Ok(())
    } else {
        Err(failed("the CakeVPN Helper service did not start"))
    }
}

/// Registers the service if Windows doesn't have it, points it at this file,
/// and (re)starts it. Run after every install and update, and by the app's
/// "Fix it" button. The service is never removed for an update: removing and
/// re-adding it can leave Windows without it when something still holds the
/// old one.
pub fn install() -> windows_service::Result<()> {
    let manager = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT | ServiceManagerAccess::CREATE_SERVICE)?;
    let info = service_info()?;
    let existing = manager.open_service(WINDOWS_SERVICE, ACCESS).ok().filter(|service| {
        // An older helper may still run: stop it before starting this one.
        stop_fully(service);
        // Refused when Windows is removing this copy of the service; a new one is made below.
        service.change_config(&info).is_ok()
    });
    let service = match existing {
        Some(service) => service,
        None => create(&manager, &info)?,
    };
    let _ = service.set_description("Runs the CakeVPN tunnel for the CakeVPN app.");
    let _ = restart_after_failures(&service);
    start(&service)
}

pub fn uninstall() -> windows_service::Result<()> {
    let manager = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT)?;
    let service = manager.open_service(WINDOWS_SERVICE, ACCESS | ServiceAccess::DELETE)?;
    stop_fully(&service);
    service.delete()
}

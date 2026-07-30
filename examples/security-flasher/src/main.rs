mod config;
mod device;
mod error;
mod plugins;
mod runtime;

use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};

use config::VehicleFlashConfig;
use device::{close_device, open_device, scan_devices, AdapterKind, DeviceChoice, DynCanDevice};
use runtime::{execute_flow, EventSink, RuntimeEvent};
use slint::{ModelRc, SharedString, VecModel};

slint::include_modules!();

enum Command {
    Scan(AdapterKind),
    Connect {
        choice: DeviceChoice,
        config: VehicleFlashConfig,
    },
    Disconnect,
    Flash {
        config: VehicleFlashConfig,
        application: PathBuf,
    },
    Shutdown,
}

enum UiEvent {
    Busy { busy: bool, message: String },
    Devices(Vec<String>),
    Connected(String),
    Disconnected,
    Progress { percent: i32, message: String },
    Log(String),
    FlashFinished(std::result::Result<(), String>),
}

struct Connection {
    device: Option<DynCanDevice>,
    config_path: PathBuf,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let conf_root = locate_conf_root();
    let (configs, config_errors) = VehicleFlashConfig::discover(&conf_root);
    let configs = Arc::new(configs);
    let choices = Arc::new(Mutex::new(Vec::<DeviceChoice>::new()));
    let cancelled = Arc::new(AtomicBool::new(false));
    let window = AppWindow::new()?;
    initialize_window(&window, &configs, &config_errors);
    install_window_chrome(&window);

    let (sender, receiver) = mpsc::channel::<Command>();
    let worker_window = window.as_weak();
    let worker_choices = Arc::clone(&choices);
    let worker_cancelled = Arc::clone(&cancelled);
    let worker = std::thread::spawn(move || {
        worker_loop(receiver, worker_window, worker_choices, worker_cancelled)
    });

    install_callbacks(
        &window,
        Arc::clone(&configs),
        Arc::clone(&choices),
        Arc::clone(&cancelled),
        sender.clone(),
    );
    let _ = sender.send(Command::Scan(AdapterKind::Demo));
    window.run()?;

    cancelled.store(true, Ordering::Relaxed);
    let _ = sender.send(Command::Shutdown);
    let _ = worker.join();
    Ok(())
}

fn install_window_chrome(window: &AppWindow) {
    let weak = window.as_weak();
    window.on_minimize_window(move || {
        if let Some(window) = weak.upgrade() {
            window.window().set_minimized(true);
        }
    });

    window.on_close_window(move || {
        let _ = slint::quit_event_loop();
    });

    install_window_drag(window);
}

#[cfg(windows)]
fn install_window_drag(window: &AppWindow) {
    let drag_origin = Rc::new(Cell::new(None::<(i32, i32, slint::PhysicalPosition)>));
    let weak = window.as_weak();
    let start_origin = Rc::clone(&drag_origin);
    window.on_start_window_drag(move || {
        if let (Some(window), Some((cursor_x, cursor_y))) =
            (weak.upgrade(), windows_cursor_position())
        {
            start_origin.set(Some((cursor_x, cursor_y, window.window().position())));
        }
    });

    let weak = window.as_weak();
    window.on_drag_window(move |_, _| {
        let (Some(window), Some((start_x, start_y, origin)), Some((cursor_x, cursor_y))) =
            (weak.upgrade(), drag_origin.get(), windows_cursor_position())
        else {
            return;
        };
        window.window().set_position(slint::PhysicalPosition::new(
            origin.x + cursor_x - start_x,
            origin.y + cursor_y - start_y,
        ));
    });
}

#[cfg(windows)]
fn windows_cursor_position() -> Option<(i32, i32)> {
    use windows_sys::Win32::Foundation::POINT;
    use windows_sys::Win32::UI::WindowsAndMessaging::GetCursorPos;

    let mut point = POINT { x: 0, y: 0 };
    // SAFETY: `point` is a valid writable pointer for the duration of the call.
    let succeeded = unsafe { GetCursorPos(&mut point) };
    (succeeded != 0).then_some((point.x, point.y))
}

#[cfg(not(windows))]
fn install_window_drag(window: &AppWindow) {
    let drag_origin = Rc::new(Cell::new(None::<slint::PhysicalPosition>));
    let weak = window.as_weak();
    let start_origin = Rc::clone(&drag_origin);
    window.on_start_window_drag(move || {
        if let Some(window) = weak.upgrade() {
            start_origin.set(Some(window.window().position()));
        }
    });

    let weak = window.as_weak();
    window.on_drag_window(move |delta_x, delta_y| {
        let (Some(window), Some(origin)) = (weak.upgrade(), drag_origin.get()) else {
            return;
        };
        let scale = window.window().scale_factor();
        window.window().set_position(slint::PhysicalPosition::new(
            origin.x + (delta_x * scale).round() as i32,
            origin.y + (delta_y * scale).round() as i32,
        ));
    });
}

fn initialize_window(window: &AppWindow, configs: &[VehicleFlashConfig], config_errors: &[String]) {
    window.set_adapter_model(string_model(AdapterKind::LABELS));
    window.set_config_model(string_model(
        configs.iter().map(|config| config.vehicle.name.as_str()),
    ));
    window.set_device_model(string_model(["Scanning virtual interface…"]));
    window.set_device_index(-1);
    if let Some(config) = configs.first() {
        window.set_config_index(0);
        show_config(window, config);
        let sample = config.source_dir.join("SampleApplication.hex");
        if sample.is_file() {
            window.set_image_path(sample.to_string_lossy().into_owned().into());
        }
    }
    for error in config_errors {
        append_log(window, &format!("Configuration error: {error}"));
    }
    append_log(
        window,
        &format!("Configuration directory: {}", locate_conf_root().display()),
    );
}

fn install_callbacks(
    window: &AppWindow,
    configs: Arc<Vec<VehicleFlashConfig>>,
    choices: Arc<Mutex<Vec<DeviceChoice>>>,
    cancelled: Arc<AtomicBool>,
    sender: mpsc::Sender<Command>,
) {
    let weak = window.as_weak();
    let callback_configs = Arc::clone(&configs);
    let disconnect_sender = sender.clone();
    window.on_config_changed(move |index| {
        if let (Some(window), Some(config)) =
            (weak.upgrade(), callback_configs.get(index.max(0) as usize))
        {
            show_config(&window, config);
            window.set_connected(false);
            window.set_connection_status("Disconnected".into());
            let _ = disconnect_sender.send(Command::Disconnect);
        }
    });

    let scan_sender = sender.clone();
    window.on_scan_devices(move |adapter_index| {
        if let Some(adapter) = AdapterKind::from_index(adapter_index) {
            let _ = scan_sender.send(Command::Scan(adapter));
        }
    });

    let connect_sender = sender.clone();
    let connect_configs = Arc::clone(&configs);
    let connect_choices = Arc::clone(&choices);
    window.on_connect_requested(move |config_index, _adapter_index, device_index| {
        let config = connect_configs.get(config_index.max(0) as usize).cloned();
        let choice = connect_choices
            .lock()
            .ok()
            .and_then(|items| items.get(device_index.max(0) as usize).cloned());
        if let (Some(config), Some(choice)) = (config, choice) {
            let _ = connect_sender.send(Command::Connect { choice, config });
        }
    });

    let disconnect_sender = sender.clone();
    window.on_disconnect_requested(move || {
        let _ = disconnect_sender.send(Command::Disconnect);
    });

    let weak = window.as_weak();
    window.on_browse_image(move || {
        if let Some(path) = rfd::FileDialog::new()
            .add_filter(
                "Firmware images",
                &["hex", "s19", "s28", "s37", "srec", "bin", "zip"],
            )
            .pick_file()
        {
            if let Some(window) = weak.upgrade() {
                window.set_image_path(path.to_string_lossy().into_owned().into());
            }
        }
    });

    let flash_sender = sender;
    let flash_configs = configs;
    let flash_cancelled = Arc::clone(&cancelled);
    window.on_flash_requested(move |config_index, application| {
        let Some(config) = flash_configs.get(config_index.max(0) as usize).cloned() else {
            return;
        };
        flash_cancelled.store(false, Ordering::Relaxed);
        let _ = flash_sender.send(Command::Flash {
            config,
            application: PathBuf::from(application.as_str()),
        });
    });

    let weak = window.as_weak();
    window.on_cancel_requested(move || {
        cancelled.store(true, Ordering::Relaxed);
        if let Some(window) = weak.upgrade() {
            window.set_task_status("Cancellation requested…".into());
            append_log(&window, "Cancellation requested");
        }
    });
}

fn worker_loop(
    receiver: mpsc::Receiver<Command>,
    window: slint::Weak<AppWindow>,
    choices: Arc<Mutex<Vec<DeviceChoice>>>,
    cancelled: Arc<AtomicBool>,
) {
    let mut connection: Option<Connection> = None;
    while let Ok(command) = receiver.recv() {
        match command {
            Command::Scan(adapter) => {
                post(
                    &window,
                    UiEvent::Busy {
                        busy: true,
                        message: "Scanning CAN interfaces…".to_string(),
                    },
                );
                match scan_devices(adapter) {
                    Ok(found) => {
                        let labels = found.iter().map(|item| item.label.clone()).collect();
                        if let Ok(mut current) = choices.lock() {
                            *current = found;
                        }
                        post(&window, UiEvent::Devices(labels));
                    }
                    Err(error) => {
                        if let Ok(mut current) = choices.lock() {
                            current.clear();
                        }
                        post(&window, UiEvent::Devices(Vec::new()));
                        post(&window, UiEvent::Log(error.to_string()));
                    }
                }
                post(
                    &window,
                    UiEvent::Busy {
                        busy: false,
                        message: "Ready".to_string(),
                    },
                );
            }
            Command::Connect { choice, config } => {
                close_connection(connection.take());
                post(
                    &window,
                    UiEvent::Busy {
                        busy: true,
                        message: format!("Opening {}…", choice.label),
                    },
                );
                let result = if config.vehicle.demo_only && choice.adapter != AdapterKind::Demo {
                    Err(error::Error::Config(
                        "this bundled public configuration is restricted to the virtual ECU"
                            .to_string(),
                    ))
                } else {
                    open_device(&choice, &config.can, config.can.response_id)
                };
                match result {
                    Ok(device) => {
                        post(&window, UiEvent::Connected(choice.label.clone()));
                        connection = Some(Connection {
                            device: Some(device),
                            config_path: config.source_path,
                        });
                    }
                    Err(error) => {
                        post(&window, UiEvent::Disconnected);
                        post(&window, UiEvent::Log(error.to_string()));
                    }
                }
                post(
                    &window,
                    UiEvent::Busy {
                        busy: false,
                        message: "Ready".to_string(),
                    },
                );
            }
            Command::Disconnect => {
                close_connection(connection.take());
                post(&window, UiEvent::Disconnected);
            }
            Command::Flash {
                config,
                application,
            } => {
                let Some(active) = connection.as_mut() else {
                    post(
                        &window,
                        UiEvent::FlashFinished(Err("no CAN interface is connected".to_string())),
                    );
                    continue;
                };
                if active.config_path != config.source_path {
                    post(
                        &window,
                        UiEvent::FlashFinished(Err(
                            "the connected interface belongs to another configuration".to_string(),
                        )),
                    );
                    continue;
                }
                if !application.is_file() {
                    post(
                        &window,
                        UiEvent::FlashFinished(Err(format!(
                            "application image not found: {}",
                            application.display()
                        ))),
                    );
                    continue;
                }
                let Some(device) = active.device.take() else {
                    post(
                        &window,
                        UiEvent::FlashFinished(Err("connected device is busy".to_string())),
                    );
                    continue;
                };
                post(
                    &window,
                    UiEvent::Busy {
                        busy: true,
                        message: format!("Running {}…", config.vehicle.name),
                    },
                );
                let event_window = window.clone();
                let sink: EventSink = Arc::new(move |event| match event {
                    RuntimeEvent::Progress { percent, message } => {
                        post(&event_window, UiEvent::Progress { percent, message })
                    }
                    RuntimeEvent::Log(message) => post(&event_window, UiEvent::Log(message)),
                });
                let (device, result) =
                    execute_flow(device, &config, &application, Arc::clone(&cancelled), sink);
                active.device = Some(device);
                post(
                    &window,
                    UiEvent::FlashFinished(result.map_err(|error| error.to_string())),
                );
                post(
                    &window,
                    UiEvent::Busy {
                        busy: false,
                        message: "Ready".to_string(),
                    },
                );
            }
            Command::Shutdown => {
                close_connection(connection.take());
                break;
            }
        }
    }
}

fn close_connection(connection: Option<Connection>) {
    if let Some(mut connection) = connection {
        if let Some(device) = connection.device.take() {
            close_device(device);
        }
    }
}

fn post(window: &slint::Weak<AppWindow>, event: UiEvent) {
    let window = window.clone();
    let _ = slint::invoke_from_event_loop(move || {
        let Some(window) = window.upgrade() else {
            return;
        };
        match event {
            UiEvent::Busy { busy, message } => {
                window.set_busy(busy);
                if !window.get_flashing() || !busy {
                    window.set_task_status(message.into());
                }
            }
            UiEvent::Devices(labels) => {
                if labels.is_empty() {
                    window.set_device_model(string_model(["No devices found"]));
                    window.set_device_index(-1);
                } else {
                    window.set_device_model(string_model(labels));
                    window.set_device_index(0);
                }
            }
            UiEvent::Connected(label) => {
                window.set_connected(true);
                window.set_connection_status(label.into());
                append_log(&window, "CAN interface connected");
            }
            UiEvent::Disconnected => {
                window.set_connected(false);
                window.set_connection_status("Disconnected".into());
                append_log(&window, "CAN interface disconnected");
            }
            UiEvent::Progress { percent, message } => {
                window.set_flashing(percent < 100);
                window.set_progress(percent);
                window.set_task_status(message.into());
            }
            UiEvent::Log(message) => append_log(&window, &message),
            UiEvent::FlashFinished(result) => {
                window.set_flashing(false);
                match result {
                    Ok(()) => {
                        window.set_progress(100);
                        window.set_task_status("Flashing completed".into());
                        append_log(&window, "Flashing completed successfully");
                    }
                    Err(error) => {
                        window.set_task_status(error.clone().into());
                        append_log(&window, &format!("Flashing stopped: {error}"));
                    }
                }
            }
        }
    });
}

fn show_config(window: &AppWindow, config: &VehicleFlashConfig) {
    window.set_config_name(config.vehicle.name.clone().into());
    window.set_config_description(config.vehicle.description.clone().into());
    window.set_config_summary(config.summary().into());
    window.set_plugin_summary(
        format!(
            "Flow: {}\nFileLoader: {} ({:?})\nSeedKey: {}",
            config.flow.dll_path.display(),
            config.loader.dll_path.display(),
            config.loader.mode,
            config.seed_key.dll_path.display()
        )
        .into(),
    );
}

fn append_log(window: &AppWindow, message: &str) {
    let mut text = window.get_log_text().to_string();
    if !text.is_empty() {
        text.push('\n');
    }
    text.push_str("• ");
    text.push_str(message);
    if text.len() > 50_000 {
        let split = text.len() - 40_000;
        let split = text[split..]
            .find('\n')
            .map(|offset| split + offset + 1)
            .unwrap_or(split);
        text.drain(..split);
    }
    window.set_log_text(text.into());
}

fn string_model<I, S>(items: I) -> ModelRc<SharedString>
where
    I: IntoIterator<Item = S>,
    S: Into<SharedString>,
{
    ModelRc::new(VecModel::from(
        items.into_iter().map(Into::into).collect::<Vec<_>>(),
    ))
}

fn locate_conf_root() -> PathBuf {
    if let Ok(executable) = std::env::current_exe() {
        if let Some(parent) = executable.parent() {
            let candidate = parent.join("Conf");
            if candidate.is_dir() {
                return candidate;
            }
        }
    }
    Path::new(env!("CARGO_MANIFEST_DIR")).join("Conf")
}

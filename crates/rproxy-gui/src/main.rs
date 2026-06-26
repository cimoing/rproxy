#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

use std::{env, net::SocketAddr, path::PathBuf, sync::Arc};

use anyhow::Context;
use rproxy_core::{
    config::{
        GeositeConfig, NodeConfig, NodeOptions, Protocol, RouteAction, RouteProfileConfig,
        RouteRule, RouteRuleType, RoutingConfig, RoutingMode, Transport, WebSocketOptions,
    },
    AppService,
};
use slint::{CloseRequestResponse, ComponentHandle, ModelRc, SharedString, VecModel};
use tokio::runtime::Runtime;
use tracing_subscriber::EnvFilter;

mod tray;

slint::include_modules!();

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    let runtime = Arc::new(Runtime::new().context("failed to create tokio runtime")?);
    let service = AppService::new();
    let ui = MainWindow::new().context("failed to create main window")?;
    let config_path = default_config_path();

    ui.set_status("Ready".into());
    ui.set_log_text("Ready".into());
    ui.set_pac_url("http://127.0.0.1:7892/proxy.pac".into());
    ui.set_running(false);
    ui.set_editor_open(false);
    ui.set_route_settings_open(false);
    ui.set_route_editor_open(false);
    ui.set_settings_open(false);
    ui.set_context_menu_open(false);
    ui.set_route_context_menu_open(false);
    ui.set_context_route_index(-1);
    ui.set_context_route_active(false);
    ui.set_nodes(ModelRc::new(VecModel::from(Vec::<NodeRow>::new())));
    ui.set_routes(ModelRc::new(VecModel::from(Vec::<RouteRow>::new())));
    ui.set_route_names(ModelRc::new(VecModel::from(Vec::<SharedString>::new())));

    let _tray = install_tray(&ui, service.clone(), Arc::clone(&runtime))?;
    {
        let ui_weak = ui.as_weak();
        ui.window().on_close_requested(move || {
            if let Some(ui) = ui_weak.upgrade() {
                let _ = ui.hide();
                update_status(
                    ui_weak.clone(),
                    UiStatus::message("Window hidden to tray".into()),
                );
            }
            CloseRequestResponse::KeepWindowShown
        });
    }

    match runtime.block_on(service.load_or_create_config(&config_path)) {
        Ok(status) => {
            update_status(ui.as_weak(), UiStatus::from(status));
            update_settings(ui.as_weak(), service.clone(), Arc::clone(&runtime));
            refresh_nodes(ui.as_weak(), service.clone(), Arc::clone(&runtime));
            refresh_routes(ui.as_weak(), service.clone(), Arc::clone(&runtime));
        }
        Err(error) => {
            update_status(
                ui.as_weak(),
                UiStatus::message(format!("Config error: {error}")),
            );
        }
    }

    {
        let ui_weak = ui.as_weak();
        let service = service.clone();
        let runtime = Arc::clone(&runtime);
        ui.on_toggle_proxy(move || {
            toggle_proxy(ui_weak.clone(), service.clone(), Arc::clone(&runtime));
        });
    }

    {
        let ui_weak = ui.as_weak();
        let service = service.clone();
        let runtime = Arc::clone(&runtime);
        ui.on_restart_proxy(move || {
            let service = service.clone();
            let ui_weak = ui_weak.clone();
            runtime.spawn(async move {
                let status = match service.restart().await {
                    Ok(status) => UiStatus::from(status),
                    Err(error) => UiStatus::message(format!("Restart error: {error}")),
                };
                update_status(ui_weak, status);
            });
        });
    }

    ui.on_open_pac(move |url| {
        let _ = webbrowser::open(url.as_str());
    });

    {
        let ui_weak = ui.as_weak();
        ui.on_clear_log(move || {
            let ui_weak = ui_weak.clone();
            let _ = slint::invoke_from_event_loop(move || {
                if let Some(ui) = ui_weak.upgrade() {
                    ui.set_log_text("".into());
                }
            });
        });
    }

    {
        let ui_weak = ui.as_weak();
        ui.on_add_node(move || {
            let ui_weak = ui_weak.clone();
            let _ = slint::invoke_from_event_loop(move || {
                if let Some(ui) = ui_weak.upgrade() {
                    open_editor_for_new_node(&ui);
                }
            });
        });
    }

    {
        let ui_weak = ui.as_weak();
        let service = service.clone();
        let runtime = Arc::clone(&runtime);
        ui.on_edit_node(move |index| {
            let ui_weak = ui_weak.clone();
            let service = service.clone();
            runtime.spawn(async move {
                let nodes = service.nodes().await;
                let Some(entry) = nodes.get(index as usize).cloned() else {
                    update_status(ui_weak, UiStatus::message("Node not found".into()));
                    return;
                };
                open_editor(ui_weak, index, entry.node);
            });
        });
    }

    {
        let ui_weak = ui.as_weak();
        let service = service.clone();
        let runtime = Arc::clone(&runtime);
        ui.on_save_node(
            move |index,
                  name,
                  server,
                  port,
                  protocol,
                  username,
                  password,
                  uuid,
                  tls,
                  transport,
                  ws_path| {
                let ui_weak = ui_weak.clone();
                let service = service.clone();
                let runtime_for_refresh = Arc::clone(&runtime);
                runtime.spawn(async move {
                    let node = match build_node(NodeDraft {
                        index,
                        name: name.to_string(),
                        server: server.to_string(),
                        port: port.to_string(),
                        protocol: protocol.to_string(),
                        username: username.to_string(),
                        password: password.to_string(),
                        uuid: uuid.to_string(),
                        tls,
                        transport: transport.to_string(),
                        ws_path: ws_path.to_string(),
                    }) {
                        Ok(node) => node,
                        Err(error) => {
                            update_status(ui_weak, UiStatus::message(error));
                            return;
                        }
                    };

                    let target_index = (index >= 0).then_some(index as usize);
                    match service.save_node(target_index, node).await {
                        Ok(status) => {
                            close_editor(ui_weak.clone());
                            update_status(ui_weak.clone(), UiStatus::from(status));
                            refresh_nodes(ui_weak, service, runtime_for_refresh);
                        }
                        Err(error) => {
                            update_status(
                                ui_weak,
                                UiStatus::message(format!("Save error: {error}")),
                            );
                        }
                    }
                });
            },
        );
    }

    {
        let ui_weak = ui.as_weak();
        ui.on_cancel_edit(move || {
            close_editor(ui_weak.clone());
        });
    }

    {
        let ui_weak = ui.as_weak();
        ui.on_add_route(move || {
            let ui_weak = ui_weak.clone();
            let _ = slint::invoke_from_event_loop(move || {
                if let Some(ui) = ui_weak.upgrade() {
                    open_route_editor_for_new_route(&ui);
                }
            });
        });
    }

    {
        let ui_weak = ui.as_weak();
        let service = service.clone();
        let runtime = Arc::clone(&runtime);
        ui.on_edit_route(move |index| {
            let ui_weak = ui_weak.clone();
            let service = service.clone();
            runtime.spawn(async move {
                let routes = service.routes().await;
                let Some(entry) = routes.get(index as usize).cloned() else {
                    update_status(ui_weak, UiStatus::message("Route not found".into()));
                    return;
                };
                open_route_editor(ui_weak, index, entry.route);
            });
        });
    }

    {
        let ui_weak = ui.as_weak();
        let service = service.clone();
        let runtime = Arc::clone(&runtime);
        ui.on_save_route(
            move |index, name, mode, default_action, geosite_enabled, rules| {
                let ui_weak = ui_weak.clone();
                let service = service.clone();
                let runtime_for_refresh = Arc::clone(&runtime);
                runtime.spawn(async move {
                    let route = match build_route(RouteDraft {
                        index,
                        name: name.to_string(),
                        mode: mode.to_string(),
                        default_action: default_action.to_string(),
                        geosite_enabled,
                        rules: rules.to_string(),
                    }) {
                        Ok(route) => route,
                        Err(error) => {
                            update_status(ui_weak, UiStatus::message(error));
                            return;
                        }
                    };

                    let target_index = (index >= 0).then_some(index as usize);
                    match service.save_route(target_index, route).await {
                        Ok(status) => {
                            close_route_editor(ui_weak.clone());
                            update_status(ui_weak.clone(), UiStatus::from(status));
                            refresh_routes(ui_weak, service, runtime_for_refresh);
                        }
                        Err(error) => {
                            update_status(
                                ui_weak,
                                UiStatus::message(format!("Route save error: {error}")),
                            );
                        }
                    }
                });
            },
        );
    }

    {
        let ui_weak = ui.as_weak();
        let service = service.clone();
        let runtime = Arc::clone(&runtime);
        ui.on_select_route(move |name| {
            let ui_weak = ui_weak.clone();
            let service = service.clone();
            let runtime_for_refresh = Arc::clone(&runtime);
            runtime.spawn(async move {
                let routes = service.routes().await;
                let Some(index) = routes
                    .iter()
                    .position(|entry| entry.route.name == name.as_str())
                else {
                    update_status(ui_weak, UiStatus::message("Route not found".into()));
                    return;
                };

                match service.set_active_route(index).await {
                    Ok(status) => {
                        update_status(ui_weak.clone(), UiStatus::from(status));
                        refresh_routes(ui_weak, service, runtime_for_refresh);
                    }
                    Err(error) => {
                        update_status(
                            ui_weak,
                            UiStatus::message(format!("Route activate error: {error}")),
                        );
                    }
                }
            });
        });
    }

    {
        let ui_weak = ui.as_weak();
        let service = service.clone();
        let runtime = Arc::clone(&runtime);
        ui.on_activate_route(move |index| {
            let ui_weak = ui_weak.clone();
            let service = service.clone();
            let runtime_for_refresh = Arc::clone(&runtime);
            runtime.spawn(async move {
                match service.set_active_route(index as usize).await {
                    Ok(status) => {
                        update_status(ui_weak.clone(), UiStatus::from(status));
                        refresh_routes(ui_weak, service, runtime_for_refresh);
                    }
                    Err(error) => {
                        update_status(
                            ui_weak,
                            UiStatus::message(format!("Route activate error: {error}")),
                        );
                    }
                }
            });
        });
    }

    {
        let ui_weak = ui.as_weak();
        let service = service.clone();
        let runtime = Arc::clone(&runtime);
        ui.on_delete_route(move |index| {
            let ui_weak = ui_weak.clone();
            let service = service.clone();
            let runtime_for_refresh = Arc::clone(&runtime);
            runtime.spawn(async move {
                match service.delete_route(index as usize).await {
                    Ok(status) => {
                        update_status(ui_weak.clone(), UiStatus::from(status));
                        refresh_routes(ui_weak, service, runtime_for_refresh);
                    }
                    Err(error) => {
                        update_status(
                            ui_weak,
                            UiStatus::message(format!("Route delete error: {error}")),
                        );
                    }
                }
            });
        });
    }

    {
        let ui_weak = ui.as_weak();
        ui.on_cancel_route_edit(move || {
            close_route_editor(ui_weak.clone());
        });
    }

    {
        let ui_weak = ui.as_weak();
        let service = service.clone();
        let runtime = Arc::clone(&runtime);
        ui.on_open_settings(move || {
            let ui_weak = ui_weak.clone();
            let service = service.clone();
            runtime.spawn(async move {
                let settings = service.settings().await;
                let _ = slint::invoke_from_event_loop(move || {
                    if let (Some(ui), Some(settings)) = (ui_weak.upgrade(), settings) {
                        ui.set_edit_pac_enabled(settings.pac_enabled);
                        ui.set_edit_auto_start(settings.auto_start);
                        ui.set_edit_http_listen(settings.http_listen.to_string().into());
                        ui.set_edit_socks_listen(settings.socks_listen.to_string().into());
                        ui.set_edit_pac_listen(settings.pac_listen.to_string().into());
                        ui.set_settings_open(true);
                    }
                });
            });
        });
    }

    {
        let ui_weak = ui.as_weak();
        let service = service.clone();
        let runtime = Arc::clone(&runtime);
        ui.on_save_settings(
            move |pac_enabled, auto_start, http_listen, socks_listen, pac_listen| {
                let ui_weak = ui_weak.clone();
                let service = service.clone();
                let runtime_for_settings = Arc::clone(&runtime);
                runtime.spawn(async move {
                    let http_listen = match parse_socket_addr(&http_listen, "HTTP listen") {
                        Ok(value) => value,
                        Err(error) => {
                            update_status(ui_weak, UiStatus::message(error));
                            return;
                        }
                    };
                    let socks_listen = match parse_socket_addr(&socks_listen, "SOCKS listen") {
                        Ok(value) => value,
                        Err(error) => {
                            update_status(ui_weak, UiStatus::message(error));
                            return;
                        }
                    };
                    let pac_listen = match parse_socket_addr(&pac_listen, "PAC listen") {
                        Ok(value) => value,
                        Err(error) => {
                            update_status(ui_weak, UiStatus::message(error));
                            return;
                        }
                    };

                    match service
                        .save_settings(
                            pac_enabled,
                            auto_start,
                            http_listen,
                            socks_listen,
                            pac_listen,
                        )
                        .await
                    {
                        Ok(status) => {
                            close_settings(ui_weak.clone());
                            update_status(ui_weak.clone(), UiStatus::from(status));
                            update_settings(ui_weak, service, runtime_for_settings);
                        }
                        Err(error) => {
                            update_status(
                                ui_weak,
                                UiStatus::message(format!("Settings error: {error}")),
                            );
                        }
                    }
                });
            },
        );
    }

    {
        let ui_weak = ui.as_weak();
        let service = service.clone();
        let runtime = Arc::clone(&runtime);
        ui.on_activate_node(move |index| {
            let ui_weak = ui_weak.clone();
            let service = service.clone();
            let runtime_for_refresh = Arc::clone(&runtime);
            runtime.spawn(async move {
                match service.set_active_node(index as usize).await {
                    Ok(status) => {
                        update_status(ui_weak.clone(), UiStatus::from(status));
                        refresh_nodes(ui_weak, service, runtime_for_refresh);
                    }
                    Err(error) => {
                        update_status(
                            ui_weak,
                            UiStatus::message(format!("Activate error: {error}")),
                        );
                    }
                }
            });
        });
    }

    {
        let ui_weak = ui.as_weak();
        let service = service.clone();
        let runtime = Arc::clone(&runtime);
        ui.on_delete_node(move |index| {
            let ui_weak = ui_weak.clone();
            let service = service.clone();
            let runtime_for_refresh = Arc::clone(&runtime);
            runtime.spawn(async move {
                match service.delete_node(index as usize).await {
                    Ok(status) => {
                        update_status(ui_weak.clone(), UiStatus::from(status));
                        refresh_nodes(ui_weak, service, runtime_for_refresh);
                    }
                    Err(error) => {
                        update_status(ui_weak, UiStatus::message(format!("Delete error: {error}")));
                    }
                }
            });
        });
    }

    {
        let ui_weak = ui.as_weak();
        ui.on_cancel_settings(move || {
            close_settings(ui_weak.clone());
        });
    }

    ui.show().context("failed to show UI")?;
    slint::run_event_loop_until_quit().context("failed to run UI")
}

fn install_tray(
    ui: &MainWindow,
    service: AppService,
    runtime: Arc<Runtime>,
) -> anyhow::Result<tray::TrayHandle> {
    let (sender, receiver) = std::sync::mpsc::channel();
    let tray = tray::TrayHandle::new(sender).context("failed to create system tray")?;
    let ui_weak = ui.as_weak();

    std::thread::spawn(move || {
        while let Ok(event) = receiver.recv() {
            let ui_weak = ui_weak.clone();
            let service = service.clone();
            let runtime = Arc::clone(&runtime);
            let _ = slint::invoke_from_event_loop(move || match event {
                tray::TrayEvent::ShowWindow => {
                    if let Some(ui) = ui_weak.upgrade() {
                        let _ = ui.window().show();
                        update_status(ui_weak.clone(), UiStatus::message("Window shown".into()));
                    }
                }
                tray::TrayEvent::ToggleProxy => {
                    toggle_proxy(ui_weak, service, runtime);
                }
                tray::TrayEvent::OpenPac => {
                    if let Some(ui) = ui_weak.upgrade() {
                        let _ = webbrowser::open(ui.get_pac_url().as_str());
                    }
                }
                tray::TrayEvent::Quit => {
                    runtime.spawn(async move {
                        let status = service.stop().await;
                        update_status(ui_weak.clone(), UiStatus::from(status));
                        let _ = slint::invoke_from_event_loop(|| {
                            let _ = slint::quit_event_loop();
                        });
                    });
                }
            });
        }
    });

    Ok(tray)
}

fn toggle_proxy(ui_weak: slint::Weak<MainWindow>, service: AppService, runtime: Arc<Runtime>) {
    runtime.spawn(async move {
        let is_running = service.status().await.running;
        let status = if is_running {
            UiStatus::from(service.stop().await)
        } else {
            match service.start().await {
                Ok(status) => UiStatus::from(status),
                Err(error) => UiStatus::message(format!("Start error: {error}")),
            }
        };
        update_status(ui_weak, status);
    });
}

fn default_config_path() -> PathBuf {
    appdata_dir()
        .unwrap_or_else(|| env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
        .join("rproxy")
        .join("config.yaml")
}

fn appdata_dir() -> Option<PathBuf> {
    env::var_os("APPDATA").map(PathBuf::from)
}

struct UiStatus {
    message: String,
    running: Option<bool>,
}

impl UiStatus {
    fn message(message: String) -> Self {
        Self {
            message,
            running: None,
        }
    }
}

impl From<rproxy_core::AppStatus> for UiStatus {
    fn from(status: rproxy_core::AppStatus) -> Self {
        Self {
            message: status.message,
            running: Some(status.running),
        }
    }
}

fn update_status(ui_weak: slint::Weak<MainWindow>, status: UiStatus) {
    let _ = slint::invoke_from_event_loop(move || {
        if let Some(ui) = ui_weak.upgrade() {
            let message = status.message;
            ui.set_status(SharedString::from(message.clone()));
            append_log(&ui, &message);
            if let Some(running) = status.running {
                ui.set_running(running);
            }
        }
    });
}

fn append_log(ui: &MainWindow, message: &str) {
    let mut lines = ui
        .get_log_text()
        .lines()
        .map(str::to_string)
        .collect::<Vec<_>>();
    lines.push(message.to_string());
    let keep_from = lines.len().saturating_sub(6);
    ui.set_log_text(lines[keep_from..].join("\n").into());
}

fn refresh_nodes(ui_weak: slint::Weak<MainWindow>, service: AppService, runtime: Arc<Runtime>) {
    runtime.spawn(async move {
        let rows = service
            .nodes()
            .await
            .into_iter()
            .enumerate()
            .map(|(index, entry)| NodeRow {
                index: index as i32,
                name: entry.node.name.clone().into(),
                server: entry.node.server.clone().into(),
                port: entry.node.port.to_string().into(),
                protocol: protocol_label(&entry.node.protocol).into(),
                transport: node_transport_label(&entry.node).into(),
                tls: if entry.node.options.tls { "tls" } else { "" }.into(),
                active: entry.active,
            })
            .collect::<Vec<_>>();

        let _ = slint::invoke_from_event_loop(move || {
            if let Some(ui) = ui_weak.upgrade() {
                ui.set_nodes(ModelRc::new(VecModel::from(rows)));
            }
        });
    });
}

fn refresh_routes(ui_weak: slint::Weak<MainWindow>, service: AppService, runtime: Arc<Runtime>) {
    runtime.spawn(async move {
        let rows = service
            .routes()
            .await
            .into_iter()
            .enumerate()
            .map(|(index, entry)| RouteRow {
                index: index as i32,
                name: entry.route.name.into(),
                mode: routing_mode_label(&entry.route.routing.mode).into(),
                default_action: route_action_label(entry.route.routing.default_action).into(),
                active: entry.active,
            })
            .collect::<Vec<_>>();

        let selected_index = rows
            .iter()
            .find(|row| row.active)
            .map(|row| row.index)
            .unwrap_or(-1);
        let route_names = rows
            .iter()
            .map(|row| row.name.clone())
            .collect::<Vec<SharedString>>();

        let _ = slint::invoke_from_event_loop(move || {
            if let Some(ui) = ui_weak.upgrade() {
                ui.set_routes(ModelRc::new(VecModel::from(rows)));
                ui.set_route_names(ModelRc::new(VecModel::from(route_names)));
                ui.set_route_selected_index(selected_index);
            }
        });
    });
}

fn update_settings(ui_weak: slint::Weak<MainWindow>, service: AppService, runtime: Arc<Runtime>) {
    runtime.spawn(async move {
        let settings = service.settings().await;
        let _ = slint::invoke_from_event_loop(move || {
            if let (Some(ui), Some(settings)) = (ui_weak.upgrade(), settings) {
                ui.set_edit_pac_enabled(settings.pac_enabled);
                ui.set_edit_auto_start(settings.auto_start);
                ui.set_edit_http_listen(settings.http_listen.to_string().into());
                ui.set_edit_socks_listen(settings.socks_listen.to_string().into());
                ui.set_edit_pac_listen(settings.pac_listen.to_string().into());
                ui.set_pac_url(format!("http://{}/proxy.pac", settings.pac_listen).into());
            }
        });
    });
}

fn open_editor(ui_weak: slint::Weak<MainWindow>, index: i32, node: NodeConfig) {
    let _ = slint::invoke_from_event_loop(move || {
        if let Some(ui) = ui_weak.upgrade() {
            ui.set_editor_open(true);
            ui.set_edit_index(index);
            ui.set_edit_title("编辑节点".into());
            ui.set_edit_name(node.name.into());
            ui.set_edit_server(node.server.into());
            ui.set_edit_port(node.port.to_string().into());
            ui.set_edit_protocol(protocol_label(&node.protocol).into());
            ui.set_edit_username(node.options.username.unwrap_or_default().into());
            ui.set_edit_password(node.options.password.unwrap_or_default().into());
            ui.set_edit_uuid(node.options.uuid.unwrap_or_default().into());
            ui.set_edit_tls(node.options.tls);
            ui.set_edit_transport(
                node.options
                    .transport
                    .as_ref()
                    .map(transport_label)
                    .unwrap_or("tcp")
                    .into(),
            );
            ui.set_edit_ws_path(
                node.options
                    .websocket
                    .as_ref()
                    .map(|ws| ws.path.clone())
                    .unwrap_or_else(|| "/".into())
                    .into(),
            );
        }
    });
}

fn open_editor_for_new_node(ui: &MainWindow) {
    ui.set_editor_open(true);
    ui.set_edit_index(-1);
    ui.set_edit_title("添加节点".into());
    ui.set_edit_name(String::new().into());
    ui.set_edit_server(String::new().into());
    ui.set_edit_port("443".into());
    ui.set_edit_protocol("vmess".into());
    ui.set_edit_username(String::new().into());
    ui.set_edit_password(String::new().into());
    ui.set_edit_uuid(String::new().into());
    ui.set_edit_tls(true);
    ui.set_edit_transport("websocket".into());
    ui.set_edit_ws_path("/".into());
}

fn open_route_editor(ui_weak: slint::Weak<MainWindow>, index: i32, route: RouteProfileConfig) {
    let _ = slint::invoke_from_event_loop(move || {
        if let Some(ui) = ui_weak.upgrade() {
            ui.set_route_editor_open(true);
            ui.set_edit_route_index(index);
            ui.set_edit_route_title("编辑路由".into());
            ui.set_edit_route_name(route.name.into());
            ui.set_edit_route_mode(routing_mode_label(&route.routing.mode).into());
            ui.set_edit_route_default_action(
                route_action_label(route.routing.default_action).into(),
            );
            ui.set_edit_route_geosite_enabled(route.routing.geosite.enabled);
            ui.set_edit_route_rules(route_rules_text(&route.routing.rules).into());
        }
    });
}

fn open_route_editor_for_new_route(ui: &MainWindow) {
    ui.set_route_editor_open(true);
    ui.set_edit_route_index(-1);
    ui.set_edit_route_title("添加路由".into());
    ui.set_edit_route_name(String::new().into());
    ui.set_edit_route_mode("auto".into());
    ui.set_edit_route_default_action("proxy".into());
    ui.set_edit_route_geosite_enabled(true);
    ui.set_edit_route_rules(String::new().into());
}

fn close_editor(ui_weak: slint::Weak<MainWindow>) {
    let _ = slint::invoke_from_event_loop(move || {
        if let Some(ui) = ui_weak.upgrade() {
            ui.set_editor_open(false);
        }
    });
}

fn close_route_editor(ui_weak: slint::Weak<MainWindow>) {
    let _ = slint::invoke_from_event_loop(move || {
        if let Some(ui) = ui_weak.upgrade() {
            ui.set_route_editor_open(false);
        }
    });
}

fn close_settings(ui_weak: slint::Weak<MainWindow>) {
    let _ = slint::invoke_from_event_loop(move || {
        if let Some(ui) = ui_weak.upgrade() {
            ui.set_settings_open(false);
        }
    });
}

struct NodeDraft {
    index: i32,
    name: String,
    server: String,
    port: String,
    protocol: String,
    username: String,
    password: String,
    uuid: String,
    tls: bool,
    transport: String,
    ws_path: String,
}

struct RouteDraft {
    index: i32,
    name: String,
    mode: String,
    default_action: String,
    geosite_enabled: bool,
    rules: String,
}

fn build_node(draft: NodeDraft) -> Result<NodeConfig, String> {
    let protocol = parse_protocol(&draft.protocol)?;
    let server = draft.server.trim().to_string();
    if server.is_empty() {
        return Err("Server is required".into());
    }
    let port = draft
        .port
        .trim()
        .parse::<u16>()
        .map_err(|_| "Port must be a number between 1 and 65535".to_string())?;
    let name = if draft.name.trim().is_empty() {
        server.clone()
    } else {
        draft.name.trim().to_string()
    };
    let proxy_protocol = matches!(protocol, Protocol::Vmess);
    let auth_protocol = matches!(protocol, Protocol::Http | Protocol::Socks);
    let transport = if proxy_protocol {
        parse_transport(&draft.transport)?
    } else {
        None
    };
    let ws_path = draft.ws_path.trim();
    let websocket = (transport == Some(Transport::WebSocket)).then(|| WebSocketOptions {
        path: if ws_path.is_empty() {
            "/".into()
        } else {
            ws_path.into()
        },
        host: Some(server.clone()),
    });

    Ok(NodeConfig {
        id: node_id(draft.index, &name),
        name,
        protocol,
        server,
        port,
        options: NodeOptions {
            username: auth_protocol.then(|| non_empty(draft.username)).flatten(),
            password: auth_protocol.then(|| non_empty(draft.password)).flatten(),
            uuid: proxy_protocol.then(|| non_empty(draft.uuid)).flatten(),
            alter_id: proxy_protocol.then_some(0),
            security: proxy_protocol.then(|| "none".into()),
            tls: proxy_protocol && draft.tls,
            transport,
            websocket,
            ..Default::default()
        },
    })
}

fn build_route(draft: RouteDraft) -> Result<RouteProfileConfig, String> {
    let name = draft.name.trim();
    if name.is_empty() {
        return Err("Route name is required".into());
    }

    Ok(RouteProfileConfig {
        id: route_id(draft.index, name),
        name: name.into(),
        routing: RoutingConfig {
            mode: parse_routing_mode(&draft.mode)?,
            default_action: parse_route_action(&draft.default_action)?,
            geosite: GeositeConfig {
                enabled: draft.geosite_enabled,
                auto_update: false,
                path: Some("data/dlc.dat".into()),
            },
            rules: parse_route_rules(&draft.rules)?,
        },
    })
}

fn node_id(index: i32, name: &str) -> String {
    if index >= 0 {
        return format!("node-{}", index + 1);
    }
    let slug = name
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() {
                ch.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_matches('-')
        .to_string();
    if slug.is_empty() {
        "node-new".into()
    } else {
        format!("node-{slug}")
    }
}

fn route_id(index: i32, name: &str) -> String {
    if index >= 0 {
        return format!("route-{}", index + 1);
    }
    let slug = name
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() {
                ch.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_matches('-')
        .to_string();
    if slug.is_empty() {
        "route-new".into()
    } else {
        format!("route-{slug}")
    }
}

fn non_empty(value: String) -> Option<String> {
    let value = value.trim().to_string();
    (!value.is_empty()).then_some(value)
}

fn parse_routing_mode(value: &str) -> Result<RoutingMode, String> {
    match value.trim().to_ascii_lowercase().as_str() {
        "" | "auto" => Ok(RoutingMode::Auto),
        "global_proxy" => Ok(RoutingMode::GlobalProxy),
        "global_direct" => Ok(RoutingMode::GlobalDirect),
        _ => Err("Route mode must be auto, global_proxy, or global_direct".into()),
    }
}

fn parse_route_action(value: &str) -> Result<RouteAction, String> {
    match value.trim().to_ascii_lowercase().as_str() {
        "" | "proxy" => Ok(RouteAction::Proxy),
        "direct" => Ok(RouteAction::Direct),
        "block" => Ok(RouteAction::Block),
        _ => Err("Route action must be proxy, direct, or block".into()),
    }
}

fn parse_route_rule_type(value: &str) -> Result<RouteRuleType, String> {
    match value.trim().to_ascii_lowercase().as_str() {
        "domain" => Ok(RouteRuleType::Domain),
        "domain_suffix" => Ok(RouteRuleType::DomainSuffix),
        "ip_cidr" => Ok(RouteRuleType::IpCidr),
        "port" => Ok(RouteRuleType::Port),
        "geosite" => Ok(RouteRuleType::Geosite),
        _ => Err("Rule type must be domain, domain_suffix, ip_cidr, port, or geosite".into()),
    }
}

fn parse_route_rules(value: &str) -> Result<Vec<RouteRule>, String> {
    value
        .lines()
        .enumerate()
        .filter_map(|(index, line)| {
            let line = line.trim();
            (!line.is_empty()).then_some((index, line))
        })
        .map(|(index, line)| {
            let parts = line.split(',').map(str::trim).collect::<Vec<_>>();
            if parts.len() != 3 {
                return Err(format!(
                    "Rule {} must use type,value,action format",
                    index + 1
                ));
            }
            if parts[1].is_empty() {
                return Err(format!("Rule {} value is required", index + 1));
            }
            Ok(RouteRule {
                kind: parse_route_rule_type(parts[0])?,
                value: parts[1].into(),
                action: parse_route_action(parts[2])?,
            })
        })
        .collect()
}

fn parse_protocol(value: &str) -> Result<Protocol, String> {
    match value.trim().to_ascii_lowercase().as_str() {
        "http" => Ok(Protocol::Http),
        "socks" | "socks5" => Ok(Protocol::Socks),
        "vmess" => Ok(Protocol::Vmess),
        _ => Err("Protocol must be http, socks, or vmess".into()),
    }
}

fn parse_transport(value: &str) -> Result<Option<Transport>, String> {
    match value.trim().to_ascii_lowercase().as_str() {
        "" | "tcp" => Ok(Some(Transport::Tcp)),
        "ws" | "websocket" => Ok(Some(Transport::WebSocket)),
        _ => Err("Transport must be tcp or websocket".into()),
    }
}

fn parse_socket_addr(value: &str, label: &str) -> Result<SocketAddr, String> {
    value
        .trim()
        .parse()
        .map_err(|_| format!("{label} must be a socket address like 127.0.0.1:7890"))
}

fn route_rules_text(rules: &[RouteRule]) -> String {
    rules
        .iter()
        .map(|rule| {
            format!(
                "{},{},{}",
                route_rule_type_label(&rule.kind),
                rule.value,
                route_action_label(rule.action)
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn protocol_label(protocol: &Protocol) -> &'static str {
    match protocol {
        Protocol::Http => "http",
        Protocol::Socks => "socks5",
        Protocol::Vmess => "vmess",
    }
}

fn node_transport_label(node: &NodeConfig) -> &'static str {
    node.options
        .transport
        .as_ref()
        .map(transport_label)
        .unwrap_or("")
}

fn routing_mode_label(mode: &RoutingMode) -> &'static str {
    match mode {
        RoutingMode::Auto => "auto",
        RoutingMode::GlobalProxy => "global_proxy",
        RoutingMode::GlobalDirect => "global_direct",
    }
}

fn route_action_label(action: RouteAction) -> &'static str {
    match action {
        RouteAction::Proxy => "proxy",
        RouteAction::Direct => "direct",
        RouteAction::Block => "block",
    }
}

fn route_rule_type_label(kind: &RouteRuleType) -> &'static str {
    match kind {
        RouteRuleType::Domain => "domain",
        RouteRuleType::DomainSuffix => "domain_suffix",
        RouteRuleType::IpCidr => "ip_cidr",
        RouteRuleType::Port => "port",
        RouteRuleType::Geosite => "geosite",
    }
}

fn transport_label(transport: &Transport) -> &'static str {
    match transport {
        Transport::Tcp => "tcp",
        Transport::WebSocket => "websocket",
    }
}

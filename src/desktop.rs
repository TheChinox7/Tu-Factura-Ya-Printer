use crate::printer::Printers;
use crate::settings::{CredentialKind, PrintMode, Settings};
use crate::{api, browser_bridge, printer, settings, startup, worker};
use eframe::egui::{
    self, Button, Color32, ComboBox, Context, Frame, Margin, RichText, Stroke, TextEdit, Ui,
    ViewportCommand, vec2,
};
use std::fs::{self, File, TryLockError};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::Duration;
use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};
use zeroize::{Zeroize, Zeroizing};

const APP_NAME: &str = "Tu Factura Ya Printer";
const APP_ICON: &[u8] = include_bytes!("../assets/app-icon.png");
const HEADER_ICON: &[u8] = include_bytes!("../assets/app-icon-header.png");
const KEY_FIELD: &str = "station-key";
const ACCENT: Color32 = Color32::from_rgb(22, 88, 187);
const INK: Color32 = Color32::from_rgb(31, 49, 84);
const MUTED: Color32 = Color32::from_rgb(82, 103, 132);
const PAGE: Color32 = Color32::from_rgb(243, 247, 251);
const SECTION_HEADER: Color32 = Color32::from_rgb(248, 251, 255);
const SECTION_BORDER: Color32 = Color32::from_rgb(196, 211, 229);
const FIELD_BORDER: Color32 = Color32::from_rgb(164, 183, 207);
const STATUS_OK: Color32 = Color32::from_rgb(23, 114, 86);
const STATUS_ERROR: Color32 = Color32::from_rgb(170, 35, 45);
const CONTROL_HEIGHT: f32 = 40.0;

pub(crate) struct Instance {
    _lock: File,
    activate: PathBuf,
}

impl Instance {
    pub(crate) fn acquire(background: bool) -> Result<Option<Self>, String> {
        let directory = settings::data_dir()?.join("desktop");
        fs::create_dir_all(&directory)
            .map_err(|_| "No se pudo abrir la carpeta de datos de la aplicación.")?;
        let lock = File::options()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(directory.join("instance.lock"))
            .map_err(|_| "No se pudo crear el bloqueo de la aplicación.")?;
        let activate = directory.join("show-settings");
        match lock.try_lock() {
            Ok(()) => {
                let _ = fs::remove_file(&activate);
                Ok(Some(Self {
                    _lock: lock,
                    activate,
                }))
            }
            Err(TryLockError::WouldBlock) => {
                if !background {
                    File::create(activate).map_err(
                        |_| "No se pudo abrir la ventana de la instancia que ya está activa.",
                    )?;
                }
                Ok(None)
            }
            Err(_) => Err("No se pudo comprobar si hay otra instancia activa.".into()),
        }
    }
}

pub(crate) fn run(instance: Instance, background: bool) -> eframe::Result<()> {
    let development = crate::development_mode();
    let icon = Arc::new(
        eframe::icon_data::from_png_bytes(APP_ICON)
            .map_err(|error| eframe::Error::AppCreation(error.into()))?,
    );
    let header_icon = eframe::icon_data::from_png_bytes(HEADER_ICON)
        .map_err(|error| eframe::Error::AppCreation(error.into()))?;
    let header_image = egui::ColorImage::from_rgba_unmultiplied(
        [header_icon.width as usize, header_icon.height as usize],
        &header_icon.rgba,
    );
    let options = eframe::NativeOptions {
        centered: true,
        persist_window: false,
        renderer: eframe::Renderer::Glow,
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([620.0, 730.0])
            .with_min_inner_size([460.0, 475.0])
            .with_visible(!background)
            .with_active(!background)
            .with_app_id(if development {
                "ec.tufacturaya.printer.dev"
            } else {
                "ec.tufacturaya.printer"
            })
            .with_icon(Arc::clone(&icon)),
        #[cfg(target_os = "macos")]
        event_loop_builder: Some(Box::new(move |builder| {
            use winit::platform::macos::{ActivationPolicy, EventLoopBuilderExtMacOS};
            builder
                .with_activation_policy(if development && !background {
                    ActivationPolicy::Regular
                } else {
                    ActivationPolicy::Accessory
                })
                .with_activate_ignoring_other_apps(!background);
        })),
        ..Default::default()
    };
    eframe::run_native(
        APP_NAME,
        options,
        Box::new(move |creation| {
            Ok(Box::new(Desktop::new(
                creation,
                instance,
                background,
                &icon,
                header_image,
            )))
        }),
    )
}

fn configure_style(ctx: &Context) {
    ctx.set_theme(egui::Theme::Light);
    ctx.global_style_mut(|style| {
        style.spacing.item_spacing = vec2(10.0, 5.0);
        style.spacing.button_padding = vec2(13.0, 8.0);
        style.spacing.interact_size.y = CONTROL_HEIGHT;
        style.spacing.icon_width = 18.0;
        style.spacing.icon_width_inner = 10.0;
        style.spacing.icon_spacing = 9.0;
        style.visuals.panel_fill = PAGE;
        style.visuals.window_fill = Color32::WHITE;
        style.visuals.window_stroke = Stroke::new(1.0, SECTION_BORDER);
        style.visuals.selection.bg_fill = Color32::from_rgb(219, 234, 254);
        style.visuals.selection.stroke = Stroke::new(1.0, ACCENT);
        style.visuals.weak_text_color = Some(MUTED);
        style.visuals.disabled_alpha = 0.72;
        let widgets = &mut style.visuals.widgets;
        widgets.noninteractive.fg_stroke.color = INK;
        for (widget, fill, border) in [
            (&mut widgets.inactive, Color32::WHITE, FIELD_BORDER),
            (&mut widgets.hovered, SECTION_HEADER, ACCENT),
            (
                &mut widgets.active,
                Color32::from_rgb(235, 244, 255),
                ACCENT,
            ),
            (&mut widgets.open, Color32::from_rgb(235, 244, 255), ACCENT),
        ] {
            widget.bg_fill = fill;
            widget.weak_bg_fill = fill;
            widget.bg_stroke = Stroke::new(1.0, border);
            widget.fg_stroke.color = INK;
            widget.corner_radius = 7.into();
            widget.expansion = 0.0;
        }
    });
}

fn settings_section(ui: &mut Ui, title: &str, body: impl FnOnce(&mut Ui)) {
    Frame::new()
        .fill(Color32::WHITE)
        .stroke(Stroke::new(1.0, SECTION_BORDER))
        .corner_radius(12)
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            let header = Frame::new()
                .fill(SECTION_HEADER)
                .corner_radius(egui::CornerRadius {
                    nw: 11,
                    ne: 11,
                    ..Default::default()
                })
                .inner_margin(Margin::symmetric(16, 11))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.label(RichText::new(title).size(17.0).strong().color(INK));
                });
            let rect = header.response.rect;
            ui.painter().hline(
                rect.x_range(),
                rect.bottom(),
                Stroke::new(1.0, SECTION_BORDER),
            );
            Frame::new()
                .inner_margin(Margin::symmetric(16, 13))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.spacing_mut().item_spacing.y = 5.0;
                    body(ui);
                });
        });
}

fn field_label(ui: &mut Ui, text: &str) -> egui::Response {
    ui.label(RichText::new(text).size(13.0).strong().color(INK))
}

fn input_frame(ui: &Ui, field: egui::Id) -> Frame {
    let focused = ui.memory(|memory| memory.has_focus(field));
    Frame::new()
        .inner_margin(Margin::symmetric(12, 9))
        .fill(Color32::WHITE)
        .stroke(Stroke::new(
            1.0,
            if focused { ACCENT } else { FIELD_BORDER },
        ))
        .corner_radius(7)
}

fn action(label: &str) -> Button<'_> {
    Button::new(RichText::new(label).strong()).min_size(vec2(0.0, CONTROL_HEIGHT))
}

fn create_tray(
    sender: mpsc::Sender<UiEvent>,
    ctx: Context,
    app_icon: &egui::IconData,
) -> Result<TrayIcon, String> {
    let menu = Menu::new();
    let open = MenuItem::new("Abrir configuración", true, None);
    let quit = MenuItem::new(format!("Salir de {APP_NAME}"), true, None);
    menu.append_items(&[&open, &PredefinedMenuItem::separator(), &quit])
        .map_err(|error| error.to_string())?;
    let open_id = open.id().clone();
    let quit_id = quit.id().clone();
    MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
        let action = match event.id {
            id if id == open_id => UiEvent::Show,
            id if id == quit_id => UiEvent::Quit,
            _ => return,
        };
        let _ = sender.send(action);
        ctx.request_repaint();
    }));
    let icon = Icon::from_rgba(app_icon.rgba.clone(), app_icon.width, app_icon.height)
        .map_err(|error| error.to_string())?;
    TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_tooltip(APP_NAME)
        .with_icon(icon)
        .build()
        .map_err(|error| error.to_string())
}

enum UiEvent {
    Show,
    Quit,
    Printers(Result<Printers, String>),
    Test(Result<String, String>),
    Connection(Result<String, String>),
    Save(Result<(String, Option<CredentialKind>), String>),
    Clear(Result<(), String>),
    Startup {
        enabled: bool,
        result: Result<(), String>,
    },
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Operation {
    Printers,
    Test,
    Connection,
    Save,
    Clear,
    Startup,
}

struct Desktop {
    logo: egui::TextureHandle,
    draft: Settings,
    key_input: Zeroizing<String>,
    saved_origin: String,
    credential_kind: Option<CredentialKind>,
    printers: Vec<String>,
    default_printer: Option<String>,
    status: Arc<Mutex<worker::Status>>,
    notice: Option<(String, bool)>,
    operation: Option<Operation>,
    auto_start: bool,
    tray: Option<TrayIcon>,
    quitting: bool,
    visible: bool,
    instance: Instance,
    sender: mpsc::Sender<UiEvent>,
    receiver: mpsc::Receiver<UiEvent>,
}

impl Desktop {
    fn new(
        creation: &eframe::CreationContext<'_>,
        instance: Instance,
        background: bool,
        app_icon: &egui::IconData,
        header_image: egui::ColorImage,
    ) -> Self {
        let development = crate::development_mode();
        let ctx = &creation.egui_ctx;
        configure_style(ctx);
        let logo = ctx.load_texture(
            "tu-factura-ya",
            header_image,
            egui::TextureOptions::LINEAR.with_mipmap_mode(Some(egui::TextureFilter::Linear)),
        );
        let (sender, receiver) = mpsc::channel();
        let tray = if development && !background {
            None
        } else {
            create_tray(sender.clone(), ctx.clone(), app_icon).ok()
        };
        let config = settings::load();
        let saved_origin = config
            .as_ref()
            .ok()
            .and_then(|value| settings::normalized_url(&value.url).ok())
            .unwrap_or_default();
        let credential_kind = settings::credential_kind(&saved_origin);
        let auto_start = startup::is_enabled();
        let (status, bridge_error) = if development {
            (Arc::new(Mutex::new(worker::Status::inactive(""))), None)
        } else {
            let (status, wake) = worker::spawn();
            let bridge_error = browser_bridge::start(wake).err().map(|_| {
                "No se pudo iniciar la conexión con el navegador. Comprueba que el puerto local 17881 esté disponible.".into()
            });
            (status, bridge_error)
        };
        let error = config
            .as_ref()
            .err()
            .or(credential_kind.as_ref().err())
            .or(auto_start.as_ref().err());
        let notice = error
            .cloned()
            .or(bridge_error)
            .map(|message| (message, false))
            .or_else(|| {
                (tray.is_none() && (!development || background)).then(|| {
                    (
                        "El icono de la barra no está disponible en este equipo.".into(),
                        false,
                    )
                })
            });
        let mut desktop = Self {
            logo,
            draft: config.unwrap_or_default(),
            key_input: Zeroizing::new(String::new()),
            saved_origin,
            credential_kind: credential_kind.unwrap_or_default(),
            printers: Vec::new(),
            default_printer: None,
            status,
            notice,
            operation: None,
            auto_start: auto_start.unwrap_or(false),
            tray,
            quitting: false,
            visible: !background,
            instance,
            sender,
            receiver,
        };
        if desktop.tray.is_none() && background {
            desktop.show(ctx);
        }
        desktop.refresh_printers(ctx);
        desktop
    }

    fn launch(
        &mut self,
        ctx: &Context,
        operation: Operation,
        action: impl FnOnce() -> UiEvent + Send + 'static,
    ) {
        if self.operation.is_some() {
            return;
        }
        self.operation = Some(operation);
        let sender = self.sender.clone();
        let ctx = ctx.clone();
        thread::spawn(move || {
            let _ = sender.send(action());
            ctx.request_repaint();
        });
    }

    fn refresh_printers(&mut self, ctx: &Context) {
        self.launch(ctx, Operation::Printers, || {
            UiEvent::Printers(printer::list())
        });
    }

    fn show(&mut self, ctx: &Context) {
        self.visible = true;
        ctx.send_viewport_cmd(ViewportCommand::Visible(true));
        ctx.send_viewport_cmd(ViewportCommand::Minimized(false));
        ctx.send_viewport_cmd(ViewportCommand::Focus);
        ctx.request_repaint();
    }

    fn receive(&mut self, ctx: &Context, event: UiEvent) {
        if !matches!(event, UiEvent::Show | UiEvent::Quit) {
            self.operation = None;
        }
        match event {
            UiEvent::Show => self.show(ctx),
            UiEvent::Quit => {
                self.quitting = true;
                ctx.send_viewport_cmd(ViewportCommand::Close);
            }
            UiEvent::Printers(Ok(Printers { default, names })) => {
                self.default_printer = default;
                self.printers = names;
                if self.draft.printer.is_empty()
                    && let Some(printer) = self
                        .default_printer
                        .as_ref()
                        .filter(|name| self.printers.contains(name))
                {
                    self.draft.printer.clone_from(printer);
                }
            }
            UiEvent::Printers(Err(message)) => self.notice = Some((message, false)),
            UiEvent::Test(result) => {
                self.notice = Some(match result {
                    Ok(message) => (message, true),
                    Err(message) => (message, false),
                })
            }
            UiEvent::Connection(result) => {
                self.notice = Some(match result {
                    Ok(message) => (message, true),
                    Err(message) => (message, false),
                })
            }
            UiEvent::Save(result) => {
                self.notice = Some(match result {
                    Ok((origin, kind)) => {
                        self.saved_origin = origin;
                        self.credential_kind = kind;
                        let message = if kind == Some(CredentialKind::Device) {
                            "Configuración guardada. El navegador detectará Printer al imprimir desde Ventas o POS."
                        } else {
                            "Configuración guardada."
                        };
                        (message.into(), true)
                    }
                    Err(message) => (message, false),
                })
            }
            UiEvent::Clear(result) => {
                self.notice = Some(match result {
                    Ok(()) => {
                        self.credential_kind = None;
                        ("Clave eliminada de este equipo.".into(), true)
                    }
                    Err(message) => (message, false),
                })
            }
            UiEvent::Startup { enabled, result } => {
                self.notice = Some(match result {
                    Ok(()) => {
                        self.auto_start = enabled;
                        (
                            if enabled {
                                "Inicio automático activado."
                            } else {
                                "Inicio automático desactivado."
                            }
                            .into(),
                            true,
                        )
                    }
                    Err(message) => (message, false),
                })
            }
        }
    }

    fn printer_panel(&mut self, ui: &mut Ui, ctx: &Context) {
        let disabled = self.operation.is_some();
        let printer_label = field_label(ui, "Impresora instalada");
        let selected = if self.draft.printer.is_empty() {
            "Selecciona una impresora".to_owned()
        } else {
            self.draft.printer.clone()
        };
        ui.horizontal(|ui| {
            let refresh_width = 130.0;
            let width = ui.available_width() - refresh_width - ui.spacing().item_spacing.x;
            ui.add_enabled_ui(!disabled, |ui| {
                ComboBox::from_id_salt("selected-printer")
                    .width(width)
                    .truncate()
                    .selected_text(selected)
                    .show_ui(ui, |ui| {
                        for name in &self.printers {
                            let label = if self.default_printer.as_ref() == Some(name) {
                                format!("{name} · Predeterminada")
                            } else {
                                name.clone()
                            };
                            ui.selectable_value(&mut self.draft.printer, name.clone(), label);
                        }
                    })
                    .response
                    .labelled_by(printer_label.id);
            });
            if ui
                .add_enabled(
                    !disabled,
                    action("Actualizar lista").min_size(vec2(refresh_width, CONTROL_HEIGHT)),
                )
                .clicked()
            {
                self.refresh_printers(ctx);
            }
        });
        if self.printers.is_empty() && self.operation != Some(Operation::Printers) {
            ui.label(
                RichText::new("Instala la impresora en el sistema y actualiza la lista.")
                    .color(MUTED),
            );
        } else if !self.draft.printer.is_empty()
            && !self.printers.contains(&self.draft.printer)
            && !disabled
        {
            ui.label(
                RichText::new("La impresora guardada ya no aparece en el sistema.")
                    .color(STATUS_ERROR),
            );
        }
        ui.add_space(4.0);
        ui.add_enabled_ui(!disabled, |ui| {
            ui.columns(2, |columns| {
                let ui = &mut columns[0];
                let paper_label = field_label(ui, "Ancho del papel");
                ComboBox::from_id_salt("paper-width")
                    .width(ui.available_width())
                    .truncate()
                    .selected_text(format!("{} mm", self.draft.width))
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut self.draft.width, 58, "58 mm");
                        ui.selectable_value(&mut self.draft.width, 80, "80 mm");
                    })
                    .response
                    .labelled_by(paper_label.id);

                let ui = &mut columns[1];
                let mode_label = field_label(ui, "Modo de impresión");
                let label = match self.draft.mode {
                    PrintMode::System => "Controlador del sistema",
                    PrintMode::EscPos => "ESC/POS directo",
                };
                ComboBox::from_id_salt("print-mode")
                    .width(ui.available_width())
                    .truncate()
                    .selected_text(label)
                    .show_ui(ui, |ui| {
                        ui.selectable_value(
                            &mut self.draft.mode,
                            PrintMode::System,
                            "Controlador del sistema",
                        );
                        ui.selectable_value(
                            &mut self.draft.mode,
                            PrintMode::EscPos,
                            "ESC/POS directo",
                        );
                    })
                    .response
                    .labelled_by(mode_label.id);
            });
        });
        if self.draft.mode == PrintMode::EscPos {
            ui.label(RichText::new("Para imprimir los tickets de Ventas y POS con su formato original, usa el controlador del sistema.").color(MUTED));
        }
        ui.add_space(4.0);
        if ui
            .add_enabled(
                !disabled && self.printers.contains(&self.draft.printer),
                action("Imprimir prueba"),
            )
            .clicked()
        {
            let config = self.draft.clone();
            self.notice = None;
            self.launch(ctx, Operation::Test, move || {
                UiEvent::Test(printer::test(&config))
            });
        }
    }

    fn connection_panel(&mut self, ui: &mut Ui, ctx: &Context) {
        let disabled = self.operation.is_some();
        let url_label = field_label(ui, "URL del sistema");
        let url_field = ui.make_persistent_id("system-url");
        ui.add_enabled(
            !disabled,
            TextEdit::singleline(&mut self.draft.url)
                .id(url_field)
                .frame(input_frame(ui, url_field))
                .min_size(vec2(0.0, CONTROL_HEIGHT))
                .hint_text("https://tu-dominio.com")
                .desired_width(f32::INFINITY),
        )
        .labelled_by(url_label.id);
        ui.add_space(4.0);
        let same_domain = settings::normalized_url(&self.draft.url)
            .is_ok_and(|origin| origin == self.saved_origin);
        let kind = if same_domain {
            self.credential_kind
        } else {
            None
        };
        let key_label = field_label(ui, "Clave API del sistema");
        let key_field = egui::Id::unique(KEY_FIELD);
        let hint = match kind {
            Some(CredentialKind::Device) => "Equipo registrado",
            Some(CredentialKind::Legacy) => "Clave de estación guardada",
            None => "Copia la clave desde Administración / Sistema",
        };
        ui.add_enabled(
            !disabled,
            TextEdit::singleline(&mut *self.key_input)
                .id(key_field)
                .frame(input_frame(ui, key_field))
                .min_size(vec2(0.0, CONTROL_HEIGHT))
                .password(true)
                .hint_text(hint)
                .desired_width(f32::INFINITY),
        )
        .labelled_by(key_label.id);
        ui.add_space(4.0);
        ui.horizontal_wrapped(|ui| {
            let can_check = !disabled
                && !self.draft.url.trim().is_empty()
                && (kind.is_some() || !self.key_input.trim().is_empty());
            if ui
                .add_enabled(can_check, action("Probar conexión"))
                .clicked()
            {
                let config = self.draft.clone();
                let key = Zeroizing::new(self.key_input.trim().to_owned());
                self.notice = None;
                self.launch(ctx, Operation::Connection, move || {
                    let key = (!key.is_empty()).then_some(key.as_str());
                    UiEvent::Connection(api::check(&config, key))
                });
            }
            if kind.is_some()
                && ui
                    .add_enabled(!disabled, action("Olvidar en este equipo"))
                    .clicked()
            {
                self.key_input.zeroize();
                clear_key_history(ctx);
                self.notice = None;
                self.launch(ctx, Operation::Clear, || {
                    UiEvent::Clear(settings::clear_key())
                });
            }
        });

        if kind == Some(CredentialKind::Legacy) {
            ui.label("Para que el navegador detecte este equipo, guarda una clave de Administración → Sistema → API Keys.");
        }
    }

    fn startup_option(&mut self, ui: &mut Ui, ctx: &Context, disabled: bool) {
        let mut launch = self.auto_start;
        let available = startup::available();
        let startup = Frame::new()
            .fill(SECTION_HEADER)
            .stroke(Stroke::new(1.0, SECTION_BORDER))
            .corner_radius(9)
            .inner_margin(Margin::symmetric(13, 2))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Wrap);
                ui.add_enabled(
                    !disabled && available,
                    egui::Checkbox::new(
                        &mut launch,
                        RichText::new("Iniciar en segundo plano al iniciar sesión").strong(),
                    ),
                )
                .on_disabled_hover_text(if !available {
                    "Disponible en la aplicación instalada."
                } else {
                    "Espera a que termine la operación actual."
                })
            });
        if startup.inner.changed() {
            self.notice = None;
            self.launch(ctx, Operation::Startup, move || UiEvent::Startup {
                enabled: launch,
                result: startup::set_enabled(launch),
            });
        }
    }

    fn footer(&mut self, ui: &mut Ui, ctx: &Context) {
        let disabled = self.operation.is_some();
        ui.horizontal(|ui| {
            let gap = ui.spacing().item_spacing.x;
            let save_width = 184.0;
            let action_height = CONTROL_HEIGHT + 6.0;
            let spinner_space = if disabled { gap + 18.0 } else { 0.0 };
            ui.allocate_ui_with_layout(
                vec2(
                    ui.available_width() - save_width - gap - spinner_space,
                    action_height,
                ),
                egui::Layout::top_down(egui::Align::Min),
                |ui| self.startup_option(ui, ctx, disabled),
            );
            if disabled {
                ui.add(egui::Spinner::new().size(18.0));
            }
            let save = Button::new(
                RichText::new("Guardar configuración")
                    .strong()
                    .color(Color32::WHITE),
            )
            .fill(ACCENT)
            .min_size(vec2(save_width, action_height));
            if ui.add_enabled(!disabled, save).clicked() {
                let config = self.draft.clone();
                let key = std::mem::take(&mut self.key_input);
                clear_key_history(ctx);
                self.notice = None;
                self.launch(ctx, Operation::Save, move || {
                    let key = key.trim();
                    UiEvent::Save(
                        api::save_settings(&config, (!key.is_empty()).then_some(key)).and_then(
                            |()| {
                                let origin = settings::normalized_url(&config.url)?;
                                let kind = settings::credential_kind(&origin)?;
                                Ok((origin, kind))
                            },
                        ),
                    )
                });
            }
        });
        if let Some((message, success)) = &self.notice {
            ui.add_space(7.0);
            ui.label(RichText::new(message).color(if *success { STATUS_OK } else { STATUS_ERROR }));
        }
    }

    fn header(&self, ui: &mut Ui) {
        ui.spacing_mut().interact_size.y = 0.0;
        ui.horizontal(|ui| {
            ui.image((self.logo.id(), vec2(32.0, 32.0)));
            ui.label(RichText::new(APP_NAME).size(28.0).strong().color(INK));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(RichText::new(concat!("v", env!("CARGO_PKG_VERSION"))).color(MUTED));
            });
        });
        ui.add_space(2.0);
        let (active, detail) = self
            .status
            .lock()
            .map(|status| (status.automatic_printing(), status.message.clone()))
            .unwrap_or_default();
        let (text, color, fill) = if active {
            (
                "Impresión automática activada",
                STATUS_OK,
                Color32::from_rgb(230, 246, 239),
            )
        } else {
            (
                "Impresión automática desactivada",
                MUTED,
                Color32::from_rgb(231, 238, 247),
            )
        };
        Frame::new()
            .fill(fill)
            .corner_radius(7)
            .inner_margin(Margin::symmetric(10, 5))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    let (dot, _) = ui.allocate_exact_size(vec2(7.0, 15.0), egui::Sense::hover());
                    ui.painter().circle_filled(dot.center(), 3.5, color);
                    ui.label(RichText::new(text).size(13.0).strong().color(color));
                });
            })
            .response
            .on_hover_text(
                "Indica si este equipo puede recibir trabajos de impresión de Ventas y POS.",
            );
        if !detail.is_empty() {
            ui.label(RichText::new(detail).size(13.0).color(MUTED));
        }
        ui.spacing_mut().interact_size.y = CONTROL_HEIGHT;
    }
}

fn clear_key_history(ctx: &Context) {
    let field = egui::Id::unique(KEY_FIELD);
    if let Some(mut state) = TextEdit::load_state(ctx, field) {
        state.clear_undoer();
        TextEdit::store_state(ctx, field, state);
    }
}

impl eframe::App for Desktop {
    fn logic(&mut self, ctx: &Context, _frame: &mut eframe::Frame) {
        if self.visible
            && !self.quitting
            && self.tray.is_some()
            && ctx.input(|input| input.viewport().close_requested())
        {
            self.visible = false;
            self.key_input.zeroize();
            clear_key_history(ctx);
            ctx.send_viewport_cmd(ViewportCommand::CancelClose);
            ctx.send_viewport_cmd(ViewportCommand::Visible(false));
        }
        if fs::remove_file(&self.instance.activate).is_ok() {
            self.show(ctx);
        }
        while let Ok(event) = self.receiver.try_recv() {
            self.receive(ctx, event);
        }
        ctx.request_repaint_after(Duration::from_secs(1));
    }

    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        egui::Panel::bottom("settings-actions")
            .frame(
                Frame::new()
                    .fill(PAGE)
                    .inner_margin(Margin::symmetric(20, 11)),
            )
            .show(ui, |ui| self.footer(ui, &ctx));
        Frame::new()
            .inner_margin(Margin::symmetric(20, 14))
            .fill(PAGE)
            .show(ui, |ui| {
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        self.header(ui);
                        ui.add_space(9.0);
                        settings_section(ui, "Impresora", |ui| self.printer_panel(ui, &ctx));
                        ui.add_space(7.0);
                        settings_section(ui, "Conexión con Tu Factura Ya", |ui| {
                            self.connection_panel(ui, &ctx);
                        });
                    });
            });
    }
}

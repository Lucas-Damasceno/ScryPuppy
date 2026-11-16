//! Native Iced UI for the ScryPuppy migration.
use global_hotkey::{hotkey::HotKey, GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};
use iced::widget::{
    button, column, container, image as iced_image, row, scrollable, text, text_input, Space,
};
use iced::{window, Alignment, Color, Element, Length, Point, Size, Task, Theme};
use keyring::{Entry as CredentialEntry, Error as CredentialError};
use scrypuppy_core::library::{
    count_captures_from_conn, list_captures_from_conn, list_contexts_from_conn, CaptureDto,
    CaptureFilter, ContextDto,
};
use scrypuppy_core::preview::{image_thumbnail, read_managed_asset};
use scrypuppy_core::storage::{open_existing_read_only, open_existing_read_write, DATABASE_FILE};
use std::{
    collections::{HashMap, VecDeque},
    path::{Path, PathBuf},
    str::FromStr,
    sync::{
        atomic::{AtomicBool, AtomicU8, Ordering},
        Arc, Mutex, OnceLock,
    },
    thread,
};

static QUICK_PASTE_HOTKEY_ID: OnceLock<u32> = OnceLock::new();
static CAPTURE_HOTKEY_ID: OnceLock<u32> = OnceLock::new();
static TRAY_EVENT_SENDER: OnceLock<
    Mutex<Option<iced::futures::channel::mpsc::UnboundedSender<Message>>>,
> = OnceLock::new();
static CLIPBOARD_EVENT_SENDER: OnceLock<
    Mutex<Option<iced::futures::channel::mpsc::UnboundedSender<Message>>>,
> = OnceLock::new();
static CLIPBOARD_MONITOR_ENABLED: AtomicBool = AtomicBool::new(false);
static OCR_WORKER_RUNNING: AtomicBool = AtomicBool::new(false);
static RETENTION_WORKER_RUNNING: AtomicBool = AtomicBool::new(false);
static AI_API_KEY_STATUS: AtomicU8 = AtomicU8::new(0);
static CAPTURE_COORDINATOR: OnceLock<scrypuppy_core::coordination::CaptureCoordinator> =
    OnceLock::new();
#[cfg(windows)]
thread_local! {
    static TRAY_ICON: std::cell::RefCell<Option<tray_icon::TrayIcon>> = const { std::cell::RefCell::new(None) };
}
const PAGE_SIZE: usize = 60;
const AI_KEY_CREDENTIAL: &str = "ai-api-key-v1";
const CREDENTIAL_SERVICE: &str = "Scryppy";

fn main() -> iced::Result {
    #[cfg(windows)]
    let debug_demo = cfg!(debug_assertions) && std::env::args_os().any(|arg| arg == "--demo");
    let clipboard_monitor_handle = scrypuppy_core::monitor::ClipboardMonitorHandle::new();
    #[cfg(windows)]
    if !debug_demo {
        if let Err(error) =
            scrypuppy_core::monitor::start(clipboard_monitor_handle.clone(), move |sequence| {
                send_clipboard_message(Message::ClipboardChanged(sequence));
            })
        {
            eprintln!("Não foi possível iniciar o monitor da área de transferência: {error}");
            clipboard_monitor_handle.shutdown();
        }
    }
    let mut hotkey_manager = None;
    match GlobalHotKeyManager::new() {
        Ok(manager) => {
            let mut registered = false;
            for (shortcut, label, id_slot) in [
                ("ctrl+shift+KeyV", "Quick Paste", &QUICK_PASTE_HOTKEY_ID),
                ("ctrl+shift+KeyC", "captura", &CAPTURE_HOTKEY_ID),
            ] {
                match HotKey::from_str(shortcut) {
                    Ok(hotkey) => match manager.register(hotkey) {
                        Ok(()) => {
                            let _ = id_slot.set(hotkey.id());
                            registered = true;
                        }
                        Err(error) => eprintln!(
                            "Não foi possível registrar o atalho global de {label}: {error}"
                        ),
                    },
                    Err(error) => eprintln!("Atalho global de {label} inválido: {error}"),
                }
            }
            if registered {
                hotkey_manager = Some(manager);
            }
        }
        Err(error) => eprintln!("Atalhos globais indisponíveis: {error}"),
    }
    let result = iced::daemon(App::new, App::update, App::view)
        .title(|app: &App, id: window::Id| {
            if app.quick_window == Some(id) {
                "Quick Paste".into()
            } else if app.quick_context_window == Some(id) {
                "Quick Context".into()
            } else {
                "ScryPuppy — Iced".into()
            }
        })
        .theme(|app: &App, id: window::Id| {
            app_theme_variant(
                app.dark,
                app.quick_window == Some(id) || app.quick_context_window == Some(id),
            )
        })
        .subscription(|_: &App| {
            iced::Subscription::batch([
                window::close_events().map(Message::Closed),
                window::close_requests().map(Message::CloseRequested),
                iced::Subscription::run(hotkey_events),
                iced::Subscription::run(tray_events),
                iced::Subscription::run(clipboard_events),
            ])
        })
        .run();
    clipboard_monitor_handle.shutdown();
    drop(hotkey_manager);
    result
}

fn app_theme_variant(dark: bool, transparent: bool) -> Theme {
    let rgb = Color::from_rgb8;
    let (mut background, text, primary) = if dark {
        (rgb(22, 26, 35), rgb(241, 243, 247), rgb(224, 103, 105))
    } else {
        (rgb(247, 248, 250), rgb(37, 42, 54), rgb(201, 72, 76))
    };
    if transparent {
        background = Color::TRANSPARENT;
    }
    Theme::custom(
        "ScryPuppy",
        iced::theme::Palette {
            background,
            text,
            primary,
            success: rgb(42, 146, 110),
            warning: rgb(184, 114, 22),
            danger: rgb(195, 52, 61),
        },
    )
}

fn panel_style(dark: bool, sidebar: bool) -> container::Style {
    let color = match (dark, sidebar) {
        (true, true) => Color::from_rgb8(29, 34, 45),
        (true, false) => Color::from_rgb8(41, 47, 59),
        (false, true) => Color::from_rgb8(240, 241, 244),
        (false, false) => Color::WHITE,
    };
    container::Style {
        background: Some(color.into()),
        border: iced::border::rounded(if sidebar { 0 } else { 14 }),
        ..container::Style::default()
    }
}

fn navigation_style(dark: bool, selected: bool, status: button::Status) -> button::Style {
    let highlighted =
        selected || matches!(status, button::Status::Hovered | button::Status::Pressed);
    let background = if highlighted {
        Some(
            if dark {
                Color::from_rgb8(43, 50, 64)
            } else {
                Color::WHITE
            }
            .into(),
        )
    } else {
        None
    };
    button::Style {
        background,
        text_color: if dark {
            Color::from_rgb8(230, 234, 241)
        } else {
            Color::from_rgb8(48, 55, 68)
        },
        border: iced::border::rounded(8),
        ..button::Style::default()
    }
}

fn context_rule_field_label(field: &str) -> &'static str {
    match field {
        "application" => "Aplicativo",
        "window_title" => "Título da janela",
        "content_type" => "Tipo de conteúdo",
        "file_extension" => "Extensão do arquivo",
        "file_path" => "Caminho do arquivo",
        _ => "Texto",
    }
}

fn context_rule_operator_label(operator: &str) -> &'static str {
    match operator {
        "equals" => "é igual a",
        "matches" => "corresponde à expressão regular",
        _ => "contém",
    }
}

#[derive(Clone, Debug)]
struct Snapshot {
    contexts: Vec<ContextDto>,
    captures: Vec<CaptureDto>,
    total: usize,
    asset_root: PathBuf,
    dark: bool,
    quick_paste_minimal: bool,
    quick_context_minimal: bool,
    clipboard_monitor_enabled: bool,
    link_preview_fetch_enabled: bool,
    capture_screenshots: bool,
    clipboard_monitor_capture_screenshots: bool,
    clipboard_monitor_quick_context_enabled: bool,
    quick_context_enabled: bool,
    onboarding_completed: bool,
    launch_at_startup: bool,
    retention_policy: String,
    ai_provider: String,
    ai_model: String,
    ai_api_key_configured: bool,
    context_rules: Vec<ContextRuleRow>,
}

#[derive(Clone, Debug)]
struct RetentionPreview {
    token: String,
    count: usize,
}

#[derive(Clone, Debug)]
struct ContextRuleRow {
    id: String,
    context_id: String,
    name: String,
    enabled: bool,
    condition: String,
}

#[derive(Clone, Debug, Default)]
struct LinkPreviewData {
    title: Option<String>,
    image: Option<Vec<u8>>,
    favicon: Option<Vec<u8>>,
}

#[derive(Clone, Debug)]
struct LinkPreviewHandles {
    title: Option<String>,
    image: Option<iced_image::Handle>,
    favicon: Option<iced_image::Handle>,
}

struct ContextEditor {
    id: String,
    name: String,
}

struct App {
    main: window::Id,
    quick_window: Option<window::Id>,
    quick_context_window: Option<window::Id>,
    quick_context_selected: Vec<String>,
    quick_search_id: iced::widget::Id,
    paste_target: Option<isize>,
    dark: bool,
    query: String,
    context: Option<String>,
    new_context_name: String,
    new_rule_name: String,
    new_rule_conditions: Vec<String>,
    new_rule_match_mode: String,
    new_rule_fields: Vec<String>,
    new_rule_operators: Vec<String>,
    rule_preview_revision: u64,
    rule_preview: Option<(u64, String, usize)>,
    rule_preview_loading: bool,
    new_tag: String,
    ai_provider: String,
    ai_model: String,
    ai_api_key_input: String,
    ai_provider_changed: bool,
    ai_model_changed: bool,
    ai_api_key_configured: bool,
    launch_at_startup: bool,
    autostart_changed: bool,
    retention_policy: String,
    retention_changed: bool,
    retention_preview: Option<RetentionPreview>,
    context_editor: Option<ContextEditor>,
    pending_delete_context: Option<String>,
    offset: usize,
    selected_capture: Option<String>,
    pending_delete_capture: Option<String>,
    theme_changed: bool,
    clipboard_monitor_enabled: bool,
    clipboard_monitor_changed: bool,
    link_preview_fetch_enabled: bool,
    link_preview_fetch_changed: bool,
    link_previews_loading: bool,
    capture_screenshots: bool,
    capture_screenshots_changed: bool,
    clipboard_monitor_capture_screenshots: bool,
    clipboard_monitor_capture_screenshots_changed: bool,
    clipboard_monitor_quick_context_enabled: bool,
    clipboard_monitor_quick_context_changed: bool,
    quick_context_enabled: bool,
    quick_context_enabled_changed: bool,
    settings_open: bool,
    onboarding_completed: bool,
    onboarding_changed: bool,
    quick_paste_minimal: bool,
    quick_context_minimal: bool,
    quick_paste_style_changed: bool,
    quick_context_style_changed: bool,
    revision: u64,
    snapshot: Option<Snapshot>,
    loading: bool,
    action_busy: bool,
    error: Option<String>,
    notice: Option<String>,
    thumbnails: HashMap<String, iced_image::Handle>,
    text_previews: HashMap<String, String>,
    link_previews: HashMap<String, LinkPreviewHandles>,
    #[cfg(debug_assertions)]
    demo: bool,
}

#[derive(Clone, Debug)]
enum Message {
    CloseRequested(window::Id),
    Closed(window::Id),
    MainOpened(window::Id),
    ShowMain,
    QuitFromTray,
    QueryChanged(String),
    Search,
    PreviousPage,
    NextPage,
    SelectContext(Option<String>),
    NewContextNameChanged(String),
    CreateContext,
    NewTagChanged(String),
    CycleAiProvider,
    CycleAiModel,
    AiApiKeyChanged(String),
    SaveAiApiKey,
    ClearAiApiKey,
    AiApiKeySaved(Result<(), String>),
    AiApiKeyCleared(Result<(), String>),
    AddTag(String),
    RemoveTag(String, String),
    TagSaved(Result<(), String>),
    NewRuleNameChanged(String),
    NewRuleConditionChanged(usize, String),
    AddRuleCondition,
    RemoveRuleCondition,
    ToggleRuleMatchMode,
    CycleRuleField(usize),
    CycleRuleOperator(usize),
    PreviewContextRule,
    ContextRulePreviewLoaded(u64, String, Result<usize, String>),
    CreateContextRule,
    ContextRuleCreated(Result<usize, String>),
    ToggleContextRule(String, bool),
    DeleteContextRule(String),
    ContextRuleChanged(Result<(), String>),
    ContextCreated(Result<String, String>),
    BeginRenameContext(String),
    ContextEditorNameChanged(String),
    SaveRenameContext,
    CancelRenameContext,
    ContextRenamed(Result<String, String>),
    RequestDeleteContext(String),
    CancelDeleteContext,
    DeleteContext(String),
    ContextDeleted(Result<(), String>),
    SelectCapture(String),
    RequestDeleteCapture(String),
    CancelDeleteCapture,
    DeleteCapture(String),
    CaptureDeleted(Result<(), String>),
    AssignContext(String, String),
    RemoveContext(String, String),
    CaptureClipboard,
    OpenQuickPaste(isize),
    QuickPasteOpened(window::Id),
    FinishQuickPaste,
    OpenQuickContext(String),
    ToggleQuickContextCapture(String),
    AssignQuickContext(String),
    QuickContextOpened(window::Id),
    FinishQuickContext,
    PasteCapture(String),
    ToggleTheme,
    ToggleClipboardMonitor,
    ToggleLinkPreviewFetch,
    ToggleAutostart,
    CycleRetentionPolicy,
    RetentionPreviewLoaded(String, Result<RetentionPreview, String>),
    ApplyRetention(bool),
    RetentionApplied(Result<usize, String>),
    RetentionCleanupFinished(usize),
    ToggleSettings,
    ToggleCaptureScreenshots,
    ToggleMonitorScreenshots,
    ToggleMonitorQuickContext,
    ToggleQuickContext,
    CompleteOnboarding,
    ClipboardChanged(u32),
    OcrUpdated(String),
    ClipboardCaptured(Result<Option<(String, bool)>, String>),
    ClipboardMonitorSaved(Result<(), String>),
    ToggleQuickPasteStyle,
    ToggleQuickContextStyle,
    Loaded(u64, Result<Snapshot, String>),
    Previews(u64, Vec<(String, Vec<u8>)>, Vec<(String, String)>),
    LinkPreviews(u64, Vec<(String, LinkPreviewData)>),
    ContextAssigned(u64, Result<(), String>),
    ContextRemoved(u64, Result<(), String>),
    Captured(u64, Result<String, String>),
    Pasted(Result<(), String>),
    ThemeSaved(Result<(), String>),
    QuickStyleSaved(Result<(), String>),
    LinkPreviewsSaved(Result<(), String>),
    AutostartSaved(bool, Result<(), String>),
    SettingSaved(&'static str, Result<(), String>),
}

impl App {
    fn invalidate_rule_preview(&mut self) {
        self.rule_preview_revision = self.rule_preview_revision.wrapping_add(1);
        self.rule_preview = None;
        self.rule_preview_loading = false;
    }

    fn is_demo_mode(&self) -> bool {
        #[cfg(debug_assertions)]
        {
            self.demo
        }
        #[cfg(not(debug_assertions))]
        {
            false
        }
    }

    fn new() -> (Self, Task<Message>) {
        let (main, open) = window::open(window::Settings {
            size: Size::new(1100.0, 720.0),
            min_size: Some(Size::new(920.0, 560.0)),
            exit_on_close_request: false,
            ..window::Settings::default()
        });
        let mut app = Self {
            main,
            quick_window: None,
            quick_context_window: None,
            quick_context_selected: Vec::new(),
            quick_search_id: iced::widget::Id::unique(),
            paste_target: None,
            dark: true,
            query: String::new(),
            context: None,
            new_context_name: String::new(),
            new_rule_name: String::new(),
            new_rule_conditions: vec![String::new()],
            new_rule_match_mode: "all".into(),
            new_rule_fields: vec!["text".into()],
            new_rule_operators: vec!["contains".into()],
            rule_preview_revision: 0,
            rule_preview: None,
            rule_preview_loading: false,
            new_tag: String::new(),
            ai_provider: "deepseek".into(),
            ai_model: "deepseek-v4-flash".into(),
            ai_api_key_input: String::new(),
            ai_provider_changed: false,
            ai_model_changed: false,
            ai_api_key_configured: false,
            launch_at_startup: false,
            autostart_changed: false,
            retention_policy: "3_months".into(),
            retention_changed: false,
            retention_preview: None,
            context_editor: None,
            pending_delete_context: None,
            offset: 0,
            selected_capture: None,
            pending_delete_capture: None,
            theme_changed: false,
            clipboard_monitor_enabled: false,
            clipboard_monitor_changed: false,
            link_preview_fetch_enabled: true,
            link_preview_fetch_changed: false,
            link_previews_loading: false,
            capture_screenshots: true,
            capture_screenshots_changed: false,
            clipboard_monitor_capture_screenshots: false,
            clipboard_monitor_capture_screenshots_changed: false,
            clipboard_monitor_quick_context_enabled: false,
            clipboard_monitor_quick_context_changed: false,
            quick_context_enabled: true,
            quick_context_enabled_changed: false,
            settings_open: false,
            onboarding_completed: false,
            onboarding_changed: false,
            quick_paste_minimal: true,
            quick_context_minimal: true,
            quick_paste_style_changed: false,
            quick_context_style_changed: false,
            revision: 0,
            snapshot: None,
            loading: true,
            action_busy: false,
            error: None,
            notice: None,
            thumbnails: HashMap::new(),
            text_previews: HashMap::new(),
            link_previews: HashMap::new(),
            #[cfg(debug_assertions)]
            demo: false,
        };
        #[cfg(debug_assertions)]
        if std::env::args_os().any(|arg| arg == "--demo") {
            app.demo = true;
            app.snapshot = Some(demo_snapshot());
            app.loading = false;
            return (app, open.map(Message::MainOpened));
        }
        let load = app.reload();
        (app, Task::batch([open.map(Message::MainOpened), load]))
    }

    fn reload(&mut self) -> Task<Message> {
        self.revision += 1;
        #[cfg(debug_assertions)]
        if self.demo {
            let mut snapshot = demo_snapshot();
            let query = self.query.trim().to_lowercase();
            snapshot
                .captures
                .retain(|capture| capture.content_text.to_lowercase().contains(&query));
            if self.context.as_deref().is_some_and(|id| id != "work") {
                snapshot.captures.clear();
            }
            snapshot.total = snapshot.captures.len();
            snapshot.captures = snapshot
                .captures
                .into_iter()
                .skip(self.offset)
                .take(PAGE_SIZE)
                .collect();
            self.snapshot = Some(snapshot);
            self.loading = false;
            self.error = None;
            self.thumbnails.clear();
            self.text_previews.clear();
            return Task::none();
        }
        let revision = self.revision;
        self.loading = true;
        self.error = None;
        let query = self.query.clone();
        let context = self.context.clone();
        let offset = self.offset;
        Task::perform(
            async move { load_default_library(query, context, offset) },
            move |result| Message::Loaded(revision, result),
        )
    }

    fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::CloseRequested(id) if id == self.main => hide_window(id),
            Message::CloseRequested(_) => Task::none(),
            Message::MainOpened(id) if id == self.main => {
                #[cfg(windows)]
                let tray_task = window::run(id, |_| initialize_tray()).discard::<Message>();
                #[cfg(not(windows))]
                let tray_task = Task::none();
                if !self.is_demo_mode() {
                    thread::spawn(|| {
                        if let Err(error) = prepare_ocr_queue() {
                            eprintln!("Não foi possível preparar a fila local de OCR: {error}");
                        }
                    });
                    start_retention_worker();
                }
                tray_task
            }
            Message::MainOpened(_) => Task::none(),
            Message::ShowMain => show_window(self.main),
            Message::QuitFromTray => iced::exit(),
            Message::Closed(id) if id == self.main => Task::none(),
            Message::Closed(id) if self.quick_window == Some(id) => {
                self.quick_window = None;
                self.paste_target = None;
                Task::none()
            }
            Message::Closed(id) if self.quick_context_window == Some(id) => {
                self.quick_context_window = None;
                self.quick_context_selected.clear();
                Task::none()
            }
            Message::Closed(_) => Task::none(),
            Message::OpenQuickPaste(target) => {
                if let Some(id) = self.quick_window {
                    return window::gain_focus(id);
                }
                self.paste_target = Some(target);
                let (id, open) = window::open(window::Settings {
                    size: Size::new(430.0, 570.0),
                    min_size: Some(Size::new(380.0, 420.0)),
                    position: window::Position::SpecificWith(|window, monitor| {
                        Point::new(
                            ((monitor.width - window.width) / 2.0).max(0.0),
                            ((monitor.height - window.height) / 2.0).max(0.0),
                        )
                    }),
                    level: window::Level::AlwaysOnTop,
                    decorations: false,
                    transparent: true,
                    ..window::Settings::default()
                });
                self.quick_window = Some(id);
                self.context = None;
                self.query.clear();
                self.offset = 0;
                Task::batch([open.map(Message::QuickPasteOpened), self.reload()])
            }
            Message::QuickPasteOpened(id) => Task::batch([
                window::gain_focus(id),
                iced::widget::operation::focus(self.quick_search_id.clone()),
            ]),
            Message::FinishQuickPaste => {
                if let Some(id) = self.quick_window.take() {
                    self.paste_target = None;
                    window::close(id)
                } else {
                    Task::none()
                }
            }
            Message::OpenQuickContext(capture_id) => {
                if let Some(id) = self.quick_context_window {
                    if !self.quick_context_selected.contains(&capture_id) {
                        self.quick_context_selected.push(capture_id);
                    }
                    return Task::batch([window::gain_focus(id), self.reload()]);
                }
                let (id, open) = window::open(window::Settings {
                    size: Size::new(390.0, 440.0),
                    min_size: Some(Size::new(340.0, 320.0)),
                    position: window::Position::SpecificWith(|window, monitor| {
                        Point::new(16.0, (monitor.height - window.height - 64.0).max(0.0))
                    }),
                    level: window::Level::AlwaysOnTop,
                    decorations: false,
                    transparent: true,
                    ..window::Settings::default()
                });
                self.quick_context_window = Some(id);
                self.quick_context_selected = vec![capture_id];
                Task::batch([open.map(Message::QuickContextOpened), self.reload()])
            }
            Message::QuickContextOpened(id) => window::gain_focus(id),
            Message::FinishQuickContext => {
                if let Some(id) = self.quick_context_window.take() {
                    self.quick_context_selected.clear();
                    window::close(id)
                } else {
                    Task::none()
                }
            }
            Message::ToggleQuickContextCapture(id) => {
                if let Some(index) = self
                    .quick_context_selected
                    .iter()
                    .position(|item| item == &id)
                {
                    self.quick_context_selected.remove(index);
                } else {
                    self.quick_context_selected.push(id);
                }
                Task::none()
            }
            Message::AssignQuickContext(context_id) => {
                #[cfg(debug_assertions)]
                if self.demo {
                    self.notice = Some("Gravações desativadas no modo de demonstração.".into());
                    return Task::none();
                }
                if self.action_busy || self.quick_context_selected.is_empty() {
                    return Task::none();
                }
                self.action_busy = true;
                self.notice = None;
                let revision = self.revision;
                let capture_ids = self.quick_context_selected.clone();
                Task::perform(
                    async move { assign_captures_to_context(capture_ids, context_id) },
                    move |result| Message::ContextAssigned(revision, result),
                )
            }
            Message::PasteCapture(id) => {
                #[cfg(debug_assertions)]
                if self.demo {
                    self.notice = Some("Colagem desativada no modo de demonstração.".into());
                    return Task::none();
                }
                if let Some(target) = self.paste_target {
                    Task::perform(
                        async move { paste_saved_capture(id, target) },
                        Message::Pasted,
                    )
                } else {
                    self.notice = Some("Janela de destino indisponível; pressione novamente o atalho no aplicativo de origem.".into());
                    Task::none()
                }
            }
            Message::Pasted(result) => match result {
                Ok(()) => {
                    if let Some(id) = self.quick_window.take() {
                        self.paste_target = None;
                        window::close(id)
                    } else {
                        Task::none()
                    }
                }
                Err(error) => {
                    self.notice = Some(error);
                    Task::none()
                }
            },
            Message::QueryChanged(value) => {
                self.query = value;
                Task::none()
            }
            Message::Search => {
                self.offset = 0;
                self.reload()
            }
            Message::PreviousPage => {
                self.offset = self.offset.saturating_sub(PAGE_SIZE);
                self.selected_capture = None;
                self.reload()
            }
            Message::NextPage => {
                if self
                    .snapshot
                    .as_ref()
                    .is_some_and(|snapshot| self.offset + PAGE_SIZE < snapshot.total)
                {
                    self.offset += PAGE_SIZE;
                    self.selected_capture = None;
                    self.reload()
                } else {
                    Task::none()
                }
            }
            Message::SelectContext(id) => {
                if id != self.context {
                    self.invalidate_rule_preview();
                }
                self.context = id;
                self.offset = 0;
                self.selected_capture = None;
                self.reload()
            }
            Message::NewContextNameChanged(name) => {
                self.new_context_name = name;
                Task::none()
            }
            Message::NewTagChanged(value) => {
                self.new_tag = value;
                Task::none()
            }
            Message::AddTag(capture_id) => {
                if self.action_busy || self.new_tag.trim().is_empty() {
                    return Task::none();
                }
                let tag = self.new_tag.trim().to_string();
                self.action_busy = true;
                Task::perform(
                    async move { update_capture_tag(&capture_id, &tag, false) },
                    Message::TagSaved,
                )
            }
            Message::RemoveTag(capture_id, tag) => {
                if self.action_busy {
                    return Task::none();
                }
                self.action_busy = true;
                Task::perform(
                    async move { update_capture_tag(&capture_id, &tag, true) },
                    Message::TagSaved,
                )
            }
            Message::TagSaved(result) => {
                self.action_busy = false;
                if let Err(error) = result {
                    self.notice = Some(error);
                } else {
                    self.new_tag.clear();
                }
                self.reload()
            }
            Message::CycleAiProvider => {
                #[cfg(debug_assertions)]
                if self.demo {
                    return Task::none();
                }
                let providers = scrypuppy_core::ai::provider_options();
                if providers.is_empty() {
                    return Task::none();
                }
                let index = providers
                    .iter()
                    .position(|provider| provider.id == self.ai_provider)
                    .unwrap_or(0);
                let provider = providers[(index + 1) % providers.len()].id.clone();
                let model = scrypuppy_core::ai::default_model(&provider);
                self.ai_provider = provider.clone();
                self.ai_model = model.clone();
                self.ai_provider_changed = true;
                self.ai_model_changed = true;
                Task::perform(
                    async move { save_ai_provider_model(&provider, &model) },
                    |result| Message::SettingSaved("ai_provider / ai_model", result),
                )
            }
            Message::CycleAiModel => {
                #[cfg(debug_assertions)]
                if self.demo {
                    return Task::none();
                }
                let Some(provider) = scrypuppy_core::ai::provider_options()
                    .into_iter()
                    .find(|provider| provider.id == self.ai_provider)
                else {
                    return Task::none();
                };
                if provider.models.is_empty() {
                    return Task::none();
                }
                let index = provider
                    .models
                    .iter()
                    .position(|model| model.id == self.ai_model)
                    .unwrap_or(0);
                let model = provider.models[(index + 1) % provider.models.len()]
                    .id
                    .clone();
                let provider_id = provider.id;
                self.ai_model = model.clone();
                self.ai_model_changed = true;
                Task::perform(
                    async move { save_ai_provider_model(&provider_id, &model) },
                    |result| Message::SettingSaved("ai_model", result),
                )
            }
            Message::AiApiKeyChanged(value) => {
                self.ai_api_key_input = value.chars().take(4096).collect();
                Task::none()
            }
            Message::SaveAiApiKey => {
                #[cfg(debug_assertions)]
                if self.demo {
                    self.notice = Some("Credenciais desativadas no modo de demonstração.".into());
                    return Task::none();
                }
                let api_key = self.ai_api_key_input.trim().to_string();
                if api_key.is_empty() {
                    self.notice = Some("Digite uma chave de API antes de salvar.".into());
                    return Task::none();
                }
                self.action_busy = true;
                Task::perform(
                    async move { save_ai_api_key(&api_key) },
                    Message::AiApiKeySaved,
                )
            }
            Message::ClearAiApiKey => {
                #[cfg(debug_assertions)]
                if self.demo {
                    self.notice = Some("Credenciais desativadas no modo de demonstração.".into());
                    return Task::none();
                }
                self.action_busy = true;
                Task::perform(async { clear_ai_api_key() }, Message::AiApiKeyCleared)
            }
            Message::AiApiKeySaved(result) => {
                self.action_busy = false;
                match result {
                    Ok(()) => {
                        self.ai_api_key_input.clear();
                        self.ai_api_key_configured = true;
                        AI_API_KEY_STATUS.store(2, Ordering::Release);
                        self.notice = Some(
                            "Chave de API salva no Gerenciador de Credenciais do Windows.".into(),
                        );
                    }
                    Err(error) => self.notice = Some(error),
                }
                Task::none()
            }
            Message::AiApiKeyCleared(result) => {
                self.action_busy = false;
                match result {
                    Ok(()) => {
                        self.ai_api_key_input.clear();
                        self.ai_api_key_configured = false;
                        AI_API_KEY_STATUS.store(1, Ordering::Release);
                        self.notice =
                            Some("Chave de API removida do Gerenciador de Credenciais.".into());
                    }
                    Err(error) => self.notice = Some(error),
                }
                Task::none()
            }
            Message::CreateContext => {
                #[cfg(debug_assertions)]
                if self.demo {
                    self.notice =
                        Some("Criação de Contextos desativada no modo de demonstração.".into());
                    return Task::none();
                }
                let name = self.new_context_name.trim().to_string();
                if name.is_empty() || self.action_busy {
                    return Task::none();
                }
                self.action_busy = true;
                Task::perform(
                    async move { create_context_in_library(name) },
                    Message::ContextCreated,
                )
            }
            Message::NewRuleNameChanged(value) => {
                self.new_rule_name = value;
                self.invalidate_rule_preview();
                Task::none()
            }
            Message::NewRuleConditionChanged(index, value) => {
                if let Some(condition) = self.new_rule_conditions.get_mut(index) {
                    *condition = value;
                }
                self.invalidate_rule_preview();
                Task::none()
            }
            Message::AddRuleCondition => {
                if self.new_rule_conditions.len() < 12 {
                    self.new_rule_conditions.push(String::new());
                    self.new_rule_fields.push("text".into());
                    self.new_rule_operators.push("contains".into());
                    self.invalidate_rule_preview();
                }
                Task::none()
            }
            Message::RemoveRuleCondition => {
                if self.new_rule_conditions.len() > 1 {
                    self.new_rule_conditions.pop();
                    self.new_rule_fields.pop();
                    self.new_rule_operators.pop();
                    self.invalidate_rule_preview();
                }
                Task::none()
            }
            Message::ToggleRuleMatchMode => {
                self.new_rule_match_mode = if self.new_rule_match_mode == "all" {
                    "any"
                } else {
                    "all"
                }
                .into();
                self.invalidate_rule_preview();
                Task::none()
            }
            Message::CycleRuleField(condition_index) => {
                let fields = [
                    "text",
                    "application",
                    "window_title",
                    "content_type",
                    "file_extension",
                    "file_path",
                ];
                let Some(selected_field) = self.new_rule_fields.get(condition_index) else {
                    return Task::none();
                };
                let index = fields
                    .iter()
                    .position(|field| *field == selected_field)
                    .unwrap_or(0);
                self.new_rule_fields[condition_index] = fields[(index + 1) % fields.len()].into();
                self.invalidate_rule_preview();
                Task::none()
            }
            Message::CycleRuleOperator(condition_index) => {
                let operators = ["contains", "equals", "matches"];
                let Some(selected_operator) = self.new_rule_operators.get(condition_index) else {
                    return Task::none();
                };
                let index = operators
                    .iter()
                    .position(|operator| *operator == selected_operator)
                    .unwrap_or(0);
                self.new_rule_operators[condition_index] =
                    operators[(index + 1) % operators.len()].into();
                self.invalidate_rule_preview();
                Task::none()
            }
            Message::PreviewContextRule => {
                let Some(context_id) = self.context.clone() else {
                    return Task::none();
                };
                let conditions = self
                    .new_rule_conditions
                    .iter()
                    .enumerate()
                    .map(|(index, value)| {
                        (
                            self.new_rule_fields[index].clone(),
                            self.new_rule_operators[index].clone(),
                            value.trim().to_string(),
                        )
                    })
                    .collect::<Vec<_>>();
                if conditions.is_empty()
                    || conditions.len() > 12
                    || conditions.iter().any(|(_, _, value)| value.is_empty())
                {
                    self.notice =
                        Some("Preencha todas as condições antes de gerar a prévia.".into());
                    return Task::none();
                }
                let mode = self.new_rule_match_mode.clone();
                self.rule_preview_revision = self.rule_preview_revision.wrapping_add(1);
                let revision = self.rule_preview_revision;
                self.rule_preview = None;
                self.rule_preview_loading = true;
                let task_context_id = context_id.clone();
                Task::perform(
                    async move { preview_context_rule(&task_context_id, &conditions, &mode) },
                    move |result| Message::ContextRulePreviewLoaded(revision, context_id, result),
                )
            }
            Message::ContextRulePreviewLoaded(revision, context_id, result) => {
                if revision == self.rule_preview_revision
                    && self.context.as_deref() == Some(context_id.as_str())
                {
                    self.rule_preview_loading = false;
                    match result {
                        Ok(count) => self.rule_preview = Some((revision, context_id, count)),
                        Err(error) => self.notice = Some(error),
                    }
                }
                Task::none()
            }
            Message::CreateContextRule => {
                #[cfg(debug_assertions)]
                if self.demo {
                    self.notice = Some("Regras desativadas no modo de demonstração.".into());
                    return Task::none();
                }
                let Some(context_id) = self.context.clone() else {
                    return Task::none();
                };
                if !self
                    .rule_preview
                    .as_ref()
                    .is_some_and(|(revision, context, _)| {
                        *revision == self.rule_preview_revision && context == &context_id
                    })
                {
                    self.notice =
                        Some("Gere uma prévia atualizada antes de salvar a regra.".into());
                    return Task::none();
                }
                let name = self.new_rule_name.trim().to_string();
                let conditions = self
                    .new_rule_conditions
                    .iter()
                    .enumerate()
                    .map(|(index, value)| {
                        (
                            self.new_rule_fields[index].clone(),
                            self.new_rule_operators[index].clone(),
                            value.trim().to_string(),
                        )
                    })
                    .collect::<Vec<_>>();
                let match_mode = self.new_rule_match_mode.clone();
                if name.is_empty() || name.chars().count() > 80 {
                    self.notice =
                        Some("Informe um nome para a regra com até 80 caracteres.".into());
                    return Task::none();
                }
                if conditions.iter().any(|(_, _, value)| value.is_empty()) {
                    self.notice = Some("Preencha todas as condições da regra.".into());
                    return Task::none();
                }
                if self.action_busy {
                    return Task::none();
                }
                self.action_busy = true;
                Task::perform(
                    async move {
                        create_text_context_rule(&context_id, &name, &conditions, &match_mode)
                    },
                    Message::ContextRuleCreated,
                )
            }
            Message::ContextRuleCreated(result) => {
                self.action_busy = false;
                match result {
                    Ok(count) => {
                        self.new_rule_name.clear();
                        self.new_rule_conditions = vec![String::new()];
                        self.new_rule_fields = vec!["text".into()];
                        self.new_rule_operators = vec!["contains".into()];
                        self.invalidate_rule_preview();
                        self.notice = Some(format!(
                            "Regra automática criada e aplicada a {count} capturas."
                        ));
                    }
                    Err(error) => self.notice = Some(error),
                }
                self.reload()
            }
            Message::ToggleContextRule(id, enabled) => {
                if self.action_busy {
                    return Task::none();
                }
                self.action_busy = true;
                Task::perform(
                    async move { set_context_rule_enabled(&id, enabled) },
                    Message::ContextRuleChanged,
                )
            }
            Message::DeleteContextRule(id) => {
                if self.action_busy {
                    return Task::none();
                }
                self.action_busy = true;
                Task::perform(
                    async move { delete_context_rule(&id) },
                    Message::ContextRuleChanged,
                )
            }
            Message::ContextRuleChanged(result) => {
                self.action_busy = false;
                if let Err(error) = result {
                    self.notice = Some(error);
                }
                self.reload()
            }
            Message::ContextCreated(result) => {
                self.action_busy = false;
                match result {
                    Ok(id) => {
                        self.context = Some(id);
                        self.new_context_name.clear();
                        self.offset = 0;
                        self.notice = Some("Contexto criado.".into());
                    }
                    Err(error) => self.notice = Some(error),
                }
                self.reload()
            }
            Message::BeginRenameContext(id) => {
                let name = self
                    .snapshot
                    .as_ref()
                    .and_then(|snapshot| snapshot.contexts.iter().find(|context| context.id == id))
                    .map(|context| context.name.clone());
                if let Some(name) = name {
                    self.context_editor = Some(ContextEditor { id, name });
                }
                Task::none()
            }
            Message::ContextEditorNameChanged(name) => {
                if let Some(editor) = self.context_editor.as_mut() {
                    editor.name = name;
                }
                Task::none()
            }
            Message::CancelRenameContext => {
                self.context_editor = None;
                Task::none()
            }
            Message::SaveRenameContext => {
                let Some(editor) = self.context_editor.take() else {
                    return Task::none();
                };
                if self.action_busy {
                    self.context_editor = Some(editor);
                    return Task::none();
                }
                self.action_busy = true;
                Task::perform(
                    async move { rename_context_in_library(editor.id, editor.name) },
                    Message::ContextRenamed,
                )
            }
            Message::ContextRenamed(result) => {
                self.action_busy = false;
                match result {
                    Ok(id) => {
                        self.context = Some(id);
                        self.notice = Some("Contexto renomeado.".into());
                    }
                    Err(error) => self.notice = Some(error),
                }
                self.reload()
            }
            Message::RequestDeleteContext(id) => {
                self.pending_delete_context = Some(id);
                Task::none()
            }
            Message::CancelDeleteContext => {
                self.pending_delete_context = None;
                Task::none()
            }
            Message::DeleteContext(id) => {
                #[cfg(debug_assertions)]
                if self.demo {
                    self.notice =
                        Some("Exclusão de Contextos desativada no modo de demonstração.".into());
                    self.pending_delete_context = None;
                    return Task::none();
                }
                if self.action_busy {
                    return Task::none();
                }
                self.action_busy = true;
                Task::perform(
                    async move { delete_context_from_library(id) },
                    Message::ContextDeleted,
                )
            }
            Message::ContextDeleted(result) => {
                self.action_busy = false;
                self.pending_delete_context = None;
                match result {
                    Ok(()) => {
                        self.context = None;
                        self.offset = 0;
                        self.notice =
                            Some("Contexto excluído; as capturas permanecem na biblioteca.".into());
                    }
                    Err(error) => self.notice = Some(error),
                }
                self.reload()
            }
            Message::SelectCapture(id) => {
                self.selected_capture = Some(id);
                Task::none()
            }
            Message::RequestDeleteCapture(id) => {
                self.pending_delete_capture = Some(id);
                Task::none()
            }
            Message::CancelDeleteCapture => {
                self.pending_delete_capture = None;
                Task::none()
            }
            Message::DeleteCapture(id) => {
                #[cfg(debug_assertions)]
                if self.demo {
                    self.notice = Some("Exclusão desativada no modo de demonstração.".into());
                    self.pending_delete_capture = None;
                    return Task::none();
                }
                if self.action_busy {
                    return Task::none();
                }
                self.action_busy = true;
                Task::perform(
                    async move { delete_capture_from_library(id) },
                    Message::CaptureDeleted,
                )
            }
            Message::CaptureDeleted(result) => {
                self.action_busy = false;
                self.pending_delete_capture = None;
                match result {
                    Ok(()) => {
                        self.selected_capture = None;
                        self.notice = Some("Captura excluída.".into());
                    }
                    Err(error) => self.notice = Some(error),
                }
                self.reload()
            }
            Message::AssignContext(capture_id, context_id) => {
                #[cfg(debug_assertions)]
                if self.demo {
                    self.notice = Some("Gravações desativadas no modo de demonstração.".into());
                    return Task::none();
                }
                if self.action_busy {
                    return Task::none();
                }
                self.action_busy = true;
                self.notice = None;
                let revision = self.revision;
                Task::perform(
                    async move { assign_capture_to_context(capture_id, context_id) },
                    move |result| Message::ContextAssigned(revision, result),
                )
            }
            Message::RemoveContext(capture_id, context_id) => {
                #[cfg(debug_assertions)]
                if self.demo {
                    self.notice = Some("Gravações desativadas no modo de demonstração.".into());
                    return Task::none();
                }
                if self.action_busy {
                    return Task::none();
                }
                self.action_busy = true;
                self.notice = None;
                let revision = self.revision;
                Task::perform(
                    async move { remove_capture_from_context(capture_id, context_id) },
                    move |result| Message::ContextRemoved(revision, result),
                )
            }
            Message::CaptureClipboard => {
                #[cfg(debug_assertions)]
                if self.demo {
                    self.notice = Some("Captura desativada no modo de demonstração.".into());
                    return Task::none();
                }
                if self.action_busy {
                    return Task::none();
                }
                self.action_busy = true;
                self.notice = None;
                let revision = self.revision;
                Task::perform(async move { capture_current_clipboard() }, move |result| {
                    Message::Captured(revision, result)
                })
            }
            Message::ToggleTheme => {
                self.dark = !self.dark;
                self.theme_changed = true;
                #[cfg(debug_assertions)]
                if self.demo {
                    return Task::none();
                }
                let dark = self.dark;
                Task::perform(
                    async move { save_theme_preference(dark) },
                    Message::ThemeSaved,
                )
            }
            Message::ToggleClipboardMonitor => {
                self.clipboard_monitor_enabled = !self.clipboard_monitor_enabled;
                self.clipboard_monitor_changed = true;
                #[cfg(debug_assertions)]
                if self.demo {
                    return Task::none();
                }
                CLIPBOARD_MONITOR_ENABLED.store(self.clipboard_monitor_enabled, Ordering::Release);
                let value = if self.clipboard_monitor_enabled {
                    "true"
                } else {
                    "false"
                };
                Task::perform(
                    async move { save_setting_value("clipboard_monitor_enabled", value) },
                    Message::ClipboardMonitorSaved,
                )
            }
            Message::ToggleLinkPreviewFetch => {
                self.link_preview_fetch_enabled = !self.link_preview_fetch_enabled;
                self.link_preview_fetch_changed = true;
                #[cfg(debug_assertions)]
                if self.demo {
                    return Task::none();
                }
                let value = if self.link_preview_fetch_enabled {
                    "true"
                } else {
                    "false"
                };
                let reload = self.reload();
                let save = Task::perform(
                    async move { save_setting_value("link_preview_fetch_enabled", value) },
                    Message::LinkPreviewsSaved,
                );
                Task::batch([reload, save])
            }
            Message::ToggleAutostart => {
                #[cfg(debug_assertions)]
                if self.demo {
                    self.notice = Some(
                        "Configurações do Windows estão desativadas no modo de demonstração."
                            .into(),
                    );
                    return Task::none();
                }
                self.launch_at_startup = !self.launch_at_startup;
                self.autostart_changed = true;
                let enabled = self.launch_at_startup;
                Task::perform(
                    async move { set_autostart_preference(enabled) },
                    move |result| Message::AutostartSaved(enabled, result),
                )
            }
            Message::ToggleSettings => {
                self.settings_open = !self.settings_open;
                if self.settings_open {
                    #[cfg(debug_assertions)]
                    if self.demo {
                        self.retention_preview = Some(RetentionPreview {
                            token: String::new(),
                            count: 0,
                        });
                        return Task::none();
                    }
                    let policy = self.retention_policy.clone();
                    Task::perform(
                        async move {
                            load_retention_preview(policy.clone()).map(|preview| (policy, preview))
                        },
                        |result| match result {
                            Ok((policy, preview)) => {
                                Message::RetentionPreviewLoaded(policy, Ok(preview))
                            }
                            Err(error) => {
                                Message::RetentionPreviewLoaded(String::new(), Err(error))
                            }
                        },
                    )
                } else {
                    Task::none()
                }
            }
            Message::CycleRetentionPolicy => {
                let policies = scrypuppy_core::retention::POLICIES;
                let index = policies
                    .iter()
                    .position(|policy| *policy == self.retention_policy)
                    .unwrap_or(4);
                self.retention_policy = policies[(index + 1) % policies.len()].into();
                self.retention_changed = true;
                self.retention_preview = None;
                #[cfg(debug_assertions)]
                if self.demo {
                    self.retention_preview = Some(RetentionPreview {
                        token: String::new(),
                        count: 0,
                    });
                    return Task::none();
                }
                let policy = self.retention_policy.clone();
                Task::perform(
                    async move {
                        load_retention_preview(policy.clone()).map(|preview| (policy, preview))
                    },
                    |result| match result {
                        Ok((policy, preview)) => {
                            Message::RetentionPreviewLoaded(policy, Ok(preview))
                        }
                        Err(error) => Message::RetentionPreviewLoaded(String::new(), Err(error)),
                    },
                )
            }
            Message::RetentionPreviewLoaded(policy, result) => {
                if policy.is_empty() || policy == self.retention_policy {
                    match result {
                        Ok(preview) => self.retention_preview = Some(preview),
                        Err(error) => self.notice = Some(error),
                    }
                }
                Task::none()
            }
            Message::ApplyRetention(delete_existing) => {
                #[cfg(debug_assertions)]
                if self.demo {
                    self.notice =
                        Some("Alterações de retenção desativadas no modo de demonstração.".into());
                    return Task::none();
                }
                if self.action_busy {
                    return Task::none();
                }
                let Some(preview) = self.retention_preview.clone() else {
                    return Task::none();
                };
                let policy = self.retention_policy.clone();
                self.action_busy = true;
                Task::perform(
                    async move { apply_retention_policy(policy, preview.token, delete_existing) },
                    Message::RetentionApplied,
                )
            }
            Message::RetentionApplied(result) => {
                self.action_busy = false;
                match result {
                    Ok(count) => {
                        self.notice = Some(if count == 0 {
                            "Política de retenção salva; os itens existentes foram mantidos.".into()
                        } else {
                            format!("Política salva; {count} capturas antigas removidas.")
                        })
                    }
                    Err(error) => self.notice = Some(error),
                }
                self.retention_preview = None;
                let policy = self.retention_policy.clone();
                let preview = Task::perform(
                    async move {
                        load_retention_preview(policy.clone()).map(|preview| (policy, preview))
                    },
                    |result| match result {
                        Ok((policy, preview)) => {
                            Message::RetentionPreviewLoaded(policy, Ok(preview))
                        }
                        Err(error) => Message::RetentionPreviewLoaded(String::new(), Err(error)),
                    },
                );
                Task::batch([self.reload(), preview])
            }
            Message::RetentionCleanupFinished(count) => {
                self.notice = Some(format!(
                    "A política de retenção removeu {count} capturas antigas."
                ));
                self.reload()
            }
            Message::CompleteOnboarding => {
                self.onboarding_completed = true;
                self.onboarding_changed = true;
                persist_boolean_setting("onboarding_completed", true, self.is_demo_mode())
            }
            Message::ToggleCaptureScreenshots => {
                self.capture_screenshots = !self.capture_screenshots;
                self.capture_screenshots_changed = true;
                persist_boolean_setting(
                    "capture_screenshots",
                    self.capture_screenshots,
                    self.is_demo_mode(),
                )
            }
            Message::ToggleMonitorScreenshots => {
                self.clipboard_monitor_capture_screenshots =
                    !self.clipboard_monitor_capture_screenshots;
                self.clipboard_monitor_capture_screenshots_changed = true;
                persist_boolean_setting(
                    "clipboard_monitor_capture_screenshots",
                    self.clipboard_monitor_capture_screenshots,
                    self.is_demo_mode(),
                )
            }
            Message::ToggleMonitorQuickContext => {
                self.clipboard_monitor_quick_context_enabled =
                    !self.clipboard_monitor_quick_context_enabled;
                self.clipboard_monitor_quick_context_changed = true;
                persist_boolean_setting(
                    "clipboard_monitor_quick_context_enabled",
                    self.clipboard_monitor_quick_context_enabled,
                    self.is_demo_mode(),
                )
            }
            Message::ToggleQuickContext => {
                self.quick_context_enabled = !self.quick_context_enabled;
                self.quick_context_enabled_changed = true;
                persist_boolean_setting(
                    "quick_context_enabled",
                    self.quick_context_enabled,
                    self.is_demo_mode(),
                )
            }
            Message::ClipboardChanged(sequence) => {
                let coordinator = CAPTURE_COORDINATOR
                    .get_or_init(scrypuppy_core::coordination::CaptureCoordinator::default);
                if coordinator.consume_internal_sequence(sequence)
                    || coordinator.monitor_is_suppressed()
                {
                    return Task::none();
                }
                if !CLIPBOARD_MONITOR_ENABLED.load(Ordering::Acquire) {
                    return Task::none();
                }
                Task::perform(
                    async { capture_monitored_clipboard() },
                    Message::ClipboardCaptured,
                )
            }
            Message::OcrUpdated(capture_id) => {
                if self.selected_capture.as_deref() == Some(capture_id.as_str()) {
                    self.notice = Some("Análise local da imagem concluída.".into());
                }
                self.reload()
            }
            Message::ClipboardCaptured(result) => match result {
                Ok(Some((id, open_quick_context))) => {
                    self.notice = Some("Captura automática salva.".into());
                    if open_quick_context {
                        self.query.clear();
                        self.context = None;
                        self.offset = 0;
                        self.selected_capture = Some(id.clone());
                        Task::done(Message::OpenQuickContext(id))
                    } else {
                        self.reload()
                    }
                }
                Ok(None) => Task::none(),
                Err(error) if error == "DUPLICATE_CAPTURE_IGNORED" => Task::none(),
                Err(error) => {
                    self.notice = Some(format!("Falha na captura automática: {error}"));
                    Task::none()
                }
            },
            Message::ClipboardMonitorSaved(result) => {
                if let Err(error) = result {
                    self.notice = Some(format!(
                        "Preferência alterada nesta sessão, mas não foi salva: {error}"
                    ));
                }
                Task::none()
            }
            Message::ToggleQuickPasteStyle => {
                self.quick_paste_minimal = !self.quick_paste_minimal;
                self.quick_paste_style_changed = true;
                #[cfg(debug_assertions)]
                if self.demo {
                    return Task::none();
                }
                let value = if self.quick_paste_minimal {
                    "minimal"
                } else {
                    "normal"
                };
                Task::perform(
                    async move { save_window_style_preference("iced_quick_paste_style", value) },
                    Message::QuickStyleSaved,
                )
            }
            Message::ToggleQuickContextStyle => {
                self.quick_context_minimal = !self.quick_context_minimal;
                self.quick_context_style_changed = true;
                #[cfg(debug_assertions)]
                if self.demo {
                    return Task::none();
                }
                let value = if self.quick_context_minimal {
                    "minimal"
                } else {
                    "normal"
                };
                Task::perform(
                    async move { save_window_style_preference("iced_quick_context_style", value) },
                    Message::QuickStyleSaved,
                )
            }
            Message::ThemeSaved(result) => {
                if let Err(error) = result {
                    self.notice = Some(format!(
                        "Tema alterado nesta sessão, mas a preferência não foi salva: {error}"
                    ));
                }
                Task::none()
            }
            Message::QuickStyleSaved(result) => {
                if let Err(error) = result {
                    self.notice = Some(format!(
                        "Estilo alterado nesta sessão, mas a preferência não foi salva: {error}"
                    ));
                }
                Task::none()
            }
            Message::LinkPreviewsSaved(result) => {
                if let Err(error) = result {
                    self.notice = Some(format!(
                        "Preferência de prévias alterada nesta sessão, mas não foi salva: {error}"
                    ));
                } else {
                    self.notice = Some(if self.link_preview_fetch_enabled {
                        "Prévia de links ativada; ScryPuppy buscará dados em sites públicos para os links salvos.".into()
                    } else {
                        "Busca remota de prévias desativada.".into()
                    });
                }
                Task::none()
            }
            Message::AutostartSaved(enabled, result) => {
                if let Err(error) = result {
                    self.launch_at_startup = !enabled;
                    self.notice = Some(format!(
                        "Não foi possível alterar a inicialização automática: {error}"
                    ));
                }
                Task::none()
            }
            Message::SettingSaved(key, result) => {
                match result {
                    Ok(()) => {}
                    Err(error) => {
                        self.notice = Some(format!(
                            "A configuração '{key}' mudou nesta sessão, mas não foi salva: {error}"
                        ));
                        if key.starts_with("ai_provider") || key.starts_with("ai_model") {
                            self.ai_provider_changed = false;
                            self.ai_model_changed = false;
                            return self.reload();
                        }
                    }
                }
                Task::none()
            }
            Message::Loaded(revision, result) if revision == self.revision => {
                self.loading = false;
                match result {
                    Ok(snapshot) => {
                        let captures = snapshot.captures.clone();
                        let root = snapshot.asset_root.clone();
                        if !self.theme_changed {
                            self.dark = snapshot.dark;
                        }
                        if !self.quick_paste_style_changed {
                            self.quick_paste_minimal = snapshot.quick_paste_minimal;
                        }
                        if !self.quick_context_style_changed {
                            self.quick_context_minimal = snapshot.quick_context_minimal;
                        }
                        if !self.clipboard_monitor_changed {
                            self.clipboard_monitor_enabled = snapshot.clipboard_monitor_enabled;
                            CLIPBOARD_MONITOR_ENABLED
                                .store(snapshot.clipboard_monitor_enabled, Ordering::Release);
                        }
                        if !self.link_preview_fetch_changed {
                            self.link_preview_fetch_enabled = snapshot.link_preview_fetch_enabled;
                        }
                        if !self.capture_screenshots_changed {
                            self.capture_screenshots = snapshot.capture_screenshots;
                        }
                        if !self.clipboard_monitor_capture_screenshots_changed {
                            self.clipboard_monitor_capture_screenshots =
                                snapshot.clipboard_monitor_capture_screenshots;
                        }
                        if !self.clipboard_monitor_quick_context_changed {
                            self.clipboard_monitor_quick_context_enabled =
                                snapshot.clipboard_monitor_quick_context_enabled;
                        }
                        if !self.quick_context_enabled_changed {
                            self.quick_context_enabled = snapshot.quick_context_enabled;
                        }
                        if !self.onboarding_changed {
                            self.onboarding_completed = snapshot.onboarding_completed;
                        }
                        if !self.autostart_changed {
                            self.launch_at_startup = snapshot.launch_at_startup;
                        }
                        if !self.retention_changed {
                            self.retention_policy = snapshot.retention_policy.clone();
                        }
                        if !self.ai_provider_changed {
                            self.ai_provider = snapshot.ai_provider.clone();
                        }
                        if !self.ai_model_changed {
                            self.ai_model = snapshot.ai_model.clone();
                        }
                        self.ai_api_key_configured = snapshot.ai_api_key_configured;
                        let fetch_links = self.link_preview_fetch_enabled;
                        self.link_previews_loading = fetch_links
                            && captures.iter().any(|capture| capture.content_kind == "url");
                        self.snapshot = Some(snapshot);
                        self.thumbnails.clear();
                        self.text_previews.clear();
                        self.link_previews.clear();
                        let preview_root = root.clone();
                        let preview_captures = captures.clone();
                        let local = Task::perform(
                            async move { previews(&preview_root, &preview_captures) },
                            move |(images, text)| Message::Previews(revision, images, text),
                        );
                        let links = Task::perform(
                            async move { link_previews(&root, &captures, fetch_links) },
                            move |links| Message::LinkPreviews(revision, links),
                        );
                        return Task::batch([local, links]);
                    }
                    Err(error) => {
                        self.snapshot = None;
                        self.error = Some(error);
                    }
                }
                Task::none()
            }
            Message::Loaded(_, _) => Task::none(),
            Message::Previews(revision, images, text) => {
                if revision == self.revision {
                    self.thumbnails = images
                        .into_iter()
                        .map(|(id, data)| (id, iced_image::Handle::from_bytes(data)))
                        .collect();
                    self.text_previews = text.into_iter().collect();
                }
                Task::none()
            }
            Message::LinkPreviews(revision, links) => {
                if revision == self.revision {
                    self.link_previews_loading = false;
                    self.link_previews = links
                        .into_iter()
                        .map(|(id, preview)| {
                            (
                                id,
                                LinkPreviewHandles {
                                    title: preview.title,
                                    image: preview.image.map(iced_image::Handle::from_bytes),
                                    favicon: preview.favicon.map(iced_image::Handle::from_bytes),
                                },
                            )
                        })
                        .collect();
                }
                Task::none()
            }
            Message::ContextAssigned(revision, result) => {
                self.action_busy = false;
                if revision != self.revision {
                    return Task::none();
                }
                let close_quick_context = match result {
                    Ok(()) => {
                        self.notice = Some("Captura adicionada ao Contexto.".into());
                        self.quick_context_window.take()
                    }
                    Err(error) => {
                        self.notice = Some(error);
                        None
                    }
                };
                if close_quick_context.is_some() {
                    self.quick_context_selected.clear();
                }
                let reload = self.reload();
                if let Some(id) = close_quick_context {
                    Task::batch([reload, window::close(id)])
                } else {
                    reload
                }
            }
            Message::ContextRemoved(revision, result) => {
                self.action_busy = false;
                if revision != self.revision {
                    return Task::none();
                }
                match result {
                    Ok(()) => self.notice = Some("Captura removida do Contexto.".into()),
                    Err(error) => self.notice = Some(error),
                }
                self.reload()
            }
            Message::Captured(revision, result) => {
                self.action_busy = false;
                if revision != self.revision {
                    return Task::none();
                }
                match result {
                    Ok(id) => {
                        self.notice = Some("Captura salva.".into());
                        self.query.clear();
                        self.context = None;
                        self.offset = 0;
                        self.selected_capture = Some(id.clone());
                        if self.quick_context_enabled {
                            return Task::done(Message::OpenQuickContext(id));
                        }
                        return self.reload();
                    }
                    Err(error) => self.notice = Some(error),
                }
                self.reload()
            }
        }
    }

    fn view(&self, id: window::Id) -> Element<'_, Message> {
        if self.quick_window == Some(id) {
            return self.quick_view();
        }
        if self.quick_context_window == Some(id) {
            return self.quick_context_view();
        }
        let dark = self.dark;
        let all_selected = self.context.is_none();
        let mut navigation = column![
            text("ScryPuppy").size(21),
            text("Seu espaço de ideias").size(12),
            Space::new().height(16),
            button("Todas as capturas")
                .on_press(Message::SelectContext(None))
                .style(move |_, status| navigation_style(dark, all_selected, status))
                .width(Length::Fill),
            button(if self.settings_open {
                "Voltar à biblioteca"
            } else {
                "Configurações"
            })
            .on_press(Message::ToggleSettings)
            .width(Length::Fill),
            Space::new().height(12),
            text("CONTEXTOS").size(11),
            text_input("Novo contexto", &self.new_context_name)
                .on_input(Message::NewContextNameChanged)
                .on_submit(Message::CreateContext)
                .padding(8),
            button("Criar Contexto")
                .on_press_maybe((!self.action_busy).then_some(Message::CreateContext))
                .width(Length::Fill),
        ]
        .spacing(8);
        if let Some(snapshot) = &self.snapshot {
            for context in &snapshot.contexts {
                let selected = self.context.as_deref() == Some(context.id.as_str());
                navigation = navigation.push(
                    button(text(context.name.as_str()).size(13))
                        .on_press(Message::SelectContext(Some(context.id.clone())))
                        .style(move |_, status| navigation_style(dark, selected, status))
                        .width(Length::Fill),
                );
            }
            #[cfg(debug_assertions)]
            if self.demo {
                if let Some(capture) = snapshot.captures.first() {
                    navigation = navigation.push(
                        button("Quick Context · demo")
                            .on_press(Message::OpenQuickContext(capture.id.clone()))
                            .width(Length::Fill),
                    );
                }
            }
        }
        #[cfg(debug_assertions)]
        let can_manage_contexts = !self.demo;
        #[cfg(not(debug_assertions))]
        let can_manage_contexts = true;
        if can_manage_contexts {
            if let Some(context_id) = self
                .context
                .as_deref()
                .filter(|id| !matches!(*id, "inbox" | "knowledge-base"))
            {
                if let Some(editor) = self
                    .context_editor
                    .as_ref()
                    .filter(|editor| editor.id == context_id)
                {
                    navigation = navigation.push(
                        text_input("Nome do Contexto", &editor.name)
                            .on_input(Message::ContextEditorNameChanged)
                            .on_submit(Message::SaveRenameContext)
                            .padding(8),
                    );
                    navigation = navigation.push(
                        row![
                            button("Salvar")
                                .on_press(Message::SaveRenameContext)
                                .width(Length::Fill),
                            button("Cancelar")
                                .on_press(Message::CancelRenameContext)
                                .width(Length::Fill),
                        ]
                        .spacing(6),
                    );
                } else {
                    navigation = navigation.push(
                        button("Renomear Contexto")
                            .on_press(Message::BeginRenameContext(context_id.to_string()))
                            .width(Length::Fill),
                    );
                }
                if self.pending_delete_context.as_deref() == Some(context_id) {
                    navigation = navigation.push(
                        text("As capturas permanecem na biblioteca sem este vínculo.").size(11),
                    );
                    navigation = navigation.push(
                        row![
                            button("Confirmar exclusão")
                                .on_press(Message::DeleteContext(context_id.to_string()))
                                .width(Length::Fill),
                            button("Cancelar")
                                .on_press(Message::CancelDeleteContext)
                                .width(Length::Fill),
                        ]
                        .spacing(6),
                    );
                } else {
                    navigation = navigation.push(
                        button("Excluir Contexto")
                            .on_press(Message::RequestDeleteContext(context_id.to_string()))
                            .width(Length::Fill),
                    );
                }
            }
            if let Some(context_id) = self.context.as_deref() {
                if !matches!(context_id, "inbox" | "knowledge-base") {
                    navigation = navigation
                        .push(Space::new().height(8))
                        .push(text("REGRAS AUTOMÁTICAS").size(11))
                        .push(
                            text_input("Nome da regra", &self.new_rule_name)
                                .on_input(Message::NewRuleNameChanged)
                                .padding(8),
                        );
                    for (index, condition) in self.new_rule_conditions.iter().enumerate() {
                        navigation = navigation.push(
                            column![
                                row![
                                    button(text(context_rule_field_label(
                                        &self.new_rule_fields[index]
                                    )))
                                    .on_press(Message::CycleRuleField(index)),
                                    button(text(context_rule_operator_label(
                                        &self.new_rule_operators[index]
                                    )))
                                    .on_press(Message::CycleRuleOperator(index)),
                                ]
                                .spacing(5),
                                text_input(&format!("Condição {}", index + 1), condition)
                                    .on_input(move |value| Message::NewRuleConditionChanged(
                                        index, value
                                    ))
                                    .padding(8),
                            ]
                            .spacing(5),
                        );
                    }
                    navigation = navigation
                        .push(row![
                            button("+ condição").on_press(Message::AddRuleCondition).width(Length::Fill),
                            button("− condição").on_press(Message::RemoveRuleCondition).width(Length::Fill),
                        ].spacing(5))
                        .push(button(if self.new_rule_match_mode == "all" { "Combinar: todas (E)" } else { "Combinar: qualquer (OU)" })
                            .on_press(Message::ToggleRuleMatchMode))
                        .push(text("Cada condição tem seu próprio campo e operador; novas capturas também seguem a regra.").size(10))
                        .push(button(if self.rule_preview_loading { "Calculando prévia…" } else { "Prévia das capturas" })
                            .on_press_maybe((!self.rule_preview_loading && !self.action_busy).then_some(Message::PreviewContextRule))
                            .width(Length::Fill));
                    let preview_ready = if let Some((revision, preview_context, count)) =
                        &self.rule_preview
                    {
                        if *revision == self.rule_preview_revision && preview_context == context_id
                        {
                            navigation = navigation.push(text(format!("A regra corresponderá a {count} capturas ainda não vinculadas a este Contexto.")).size(10));
                            true
                        } else {
                            false
                        }
                    } else {
                        false
                    };
                    navigation = navigation.push(
                        button(if self.action_busy {
                            "Salvando regra…"
                        } else {
                            "Salvar regra"
                        })
                        .on_press_maybe(
                            (preview_ready && !self.action_busy)
                                .then_some(Message::CreateContextRule),
                        )
                        .width(Length::Fill),
                    );
                    if let Some(snapshot) = &self.snapshot {
                        for rule in snapshot
                            .context_rules
                            .iter()
                            .filter(|rule| rule.context_id == context_id)
                        {
                            navigation = navigation
                                .push(
                                    row![
                                        button(text(if rule.enabled {
                                            format!("✓ {}", rule.name)
                                        } else {
                                            rule.name.clone()
                                        }))
                                        .on_press(Message::ToggleContextRule(
                                            rule.id.clone(),
                                            !rule.enabled
                                        ))
                                        .width(Length::Fill),
                                        button("×")
                                            .on_press(Message::DeleteContextRule(rule.id.clone())),
                                    ]
                                    .spacing(4),
                                )
                                .push(text(rule.condition.as_str()).size(10));
                        }
                    }
                }
            }
        }
        let sidebar = container(scrollable(navigation))
            .width(220)
            .height(Length::Fill)
            .padding(20)
            .style(move |_| panel_style(dark, true));
        let search = row![
            text_input("Buscar capturas", &self.query)
                .on_input(Message::QueryChanged)
                .on_submit(Message::Search)
                .padding(10),
            button("Buscar").on_press(Message::Search),
            button(if self.action_busy {
                "Aguarde…"
            } else {
                "Capturar clipboard"
            })
            .on_press(Message::CaptureClipboard),
            button(if self.clipboard_monitor_enabled {
                "Auto: ligada"
            } else {
                "Auto: desligada"
            })
            .on_press(Message::ToggleClipboardMonitor),
            button(if self.dark {
                "Tema claro"
            } else {
                "Tema escuro"
            })
            .on_press(Message::ToggleTheme)
            .style(move |_, status| navigation_style(dark, false, status)),
        ]
        .spacing(10)
        .align_y(Alignment::Center);
        let mut content = column![
            text(if self.settings_open {
                "Configurações"
            } else {
                "Biblioteca"
            })
            .size(28),
            text(if self.settings_open {
                "Preferências da biblioteca e das capturas"
            } else {
                "Biblioteca local · selecione uma captura para ver os detalhes"
            })
            .size(12),
        ]
        .spacing(18);
        if !self.onboarding_completed && !self.settings_open {
            content = content.push(
                container(
                    column![
                        text("Bem-vindo ao ScryPuppy").size(20),
                        text("Suas capturas ficam na biblioteca local criptografada. Use Ctrl + Shift + C para salvar o clipboard e Ctrl + Shift + V para abrir o Quick Paste.").size(13),
                        text("A captura automática começa desligada. Você pode controlar imagens, Quick Context e prévias de links em Configurações.").size(12),
                        button("Continuar para a biblioteca").on_press(Message::CompleteOnboarding),
                    ]
                    .spacing(10),
                )
                .width(Length::Fill)
                .padding(18)
                .style(move |_| panel_style(dark, false)),
            );
        }
        if !self.settings_open {
            content = content.push(search);
        }
        if self.clipboard_monitor_enabled && !self.settings_open {
            content = content.push(
                text("Capturas novas são salvas localmente; imagens seguem a preferência de captura de telas.").size(11),
            );
        }
        if self.settings_open {
            let ai_provider_label = scrypuppy_core::ai::provider_options()
                .into_iter()
                .find(|provider| provider.id == self.ai_provider)
                .map(|provider| provider.name)
                .unwrap_or_else(|| self.ai_provider.clone());
            let ai_model_label = scrypuppy_core::ai::provider_options()
                .into_iter()
                .find(|provider| provider.id == self.ai_provider)
                .and_then(|provider| {
                    provider
                        .models
                        .into_iter()
                        .find(|model| model.id == self.ai_model)
                        .map(|model| model.name)
                })
                .unwrap_or_else(|| self.ai_model.clone());
            content = content
                .push(text("Captura e privacidade").size(20))
                .push(
                    button(if self.capture_screenshots {
                        "Capturas manuais: salvar imagens"
                    } else {
                        "Capturas manuais: ignorar imagens"
                    })
                    .on_press(Message::ToggleCaptureScreenshots),
                )
                .push(text("Controla se imagens copiadas são mantidas como capturas locais.").size(12))
                .push(
                    button(if self.clipboard_monitor_capture_screenshots {
                        "Captura automática: salvar imagens"
                    } else {
                        "Captura automática: ignorar imagens"
                    })
                    .on_press(Message::ToggleMonitorScreenshots),
                )
                .push(text("A captura automática continua desativada até ser ligada na barra da biblioteca.").size(12))
                .push(
                    button(if self.quick_context_enabled {
                        "Quick Context após captura manual: ligado"
                    } else {
                        "Quick Context após captura manual: desligado"
                    })
                    .on_press(Message::ToggleQuickContext),
                )
                .push(
                    button(if self.clipboard_monitor_quick_context_enabled {
                        "Quick Context após captura automática: ligado"
                    } else {
                        "Quick Context após captura automática: desligado"
                    })
                    .on_press(Message::ToggleMonitorQuickContext),
                )
                .push(
                    button(if self.link_preview_fetch_enabled {
                        "Prévia web: ligada"
                    } else {
                        "Prévia web: desligada"
                    })
                    .on_press(Message::ToggleLinkPreviewFetch),
                )
                .push(text("Quando ligada, a busca acessa sites públicos para obter título, favicon e imagem dos links salvos. Endereços locais e privados são bloqueados; a busca pode ser desligada, mantendo as prévias já salvas em cache local.").size(12))
                .push(text("Inteligência artificial").size(20))
                .push(
                    button(text(format!("Provedor: {ai_provider_label}")))
                        .on_press(Message::CycleAiProvider),
                )
                .push(
                    button(text(format!("Modelo: {ai_model_label}")))
                        .on_press(Message::CycleAiModel),
                )
                .push(
                    text_input("Chave de API", &self.ai_api_key_input)
                        .on_input(Message::AiApiKeyChanged)
                        .secure(true)
                        .padding(8),
                )
                .push(row![
                    button("Salvar chave")
                        .on_press_maybe((!self.action_busy).then_some(Message::SaveAiApiKey))
                        .width(Length::Fill),
                    button("Remover chave")
                        .on_press_maybe((self.ai_api_key_configured && !self.action_busy).then_some(Message::ClearAiApiKey))
                        .width(Length::Fill),
                ].spacing(6))
                .push(text(if self.ai_api_key_configured {
                    "A chave está configurada no Gerenciador de Credenciais do Windows."
                } else {
                    "Nenhuma chave de API está configurada."
                }).size(12))
                .push(text("A chave fica fora do banco local. A geração de respostas e documentos ainda não está disponível na interface Iced.").size(12))
                .push(button(if self.launch_at_startup { "Iniciar com o Windows: ligado" } else { "Iniciar com o Windows: desligado" })
                    .on_press(Message::ToggleAutostart))
                .push(text("Adiciona ou remove ScryPuppy da inicialização da sua conta do Windows.").size(12));
            let retention_label = match self.retention_policy.as_str() {
                "1_day" => "1 dia",
                "3_days" => "3 dias",
                "7_days" => "7 dias",
                "1_month" => "1 mês",
                "3_months" => "3 meses",
                "6_months" => "6 meses",
                "12_months" => "12 meses",
                "never" => "Nunca",
                _ => "3 meses",
            };
            content = content
                .push(text("Retenção da biblioteca").size(20))
                .push(button(text(format!("Apagar capturas após: {retention_label}")))
                    .on_press(Message::CycleRetentionPolicy))
                .push(text("A prévia exclui referências e arquivos importados. A limpeza automática é executada no máximo uma vez por dia.").size(12));
            if let Some(preview) = &self.retention_preview {
                content = content
                    .push(
                        text(format!(
                            "{} capturas seriam elegíveis agora.",
                            preview.count
                        ))
                        .size(12),
                    )
                    .push(
                        button("Salvar e manter as capturas existentes").on_press_maybe(
                            (!self.action_busy).then_some(Message::ApplyRetention(false)),
                        ),
                    );
                if preview.count > 0 {
                    content = content.push(
                        button(text(format!(
                            "Salvar e excluir {} capturas antigas agora",
                            preview.count
                        )))
                        .on_press_maybe(
                            (!self.action_busy).then_some(Message::ApplyRetention(true)),
                        ),
                    );
                }
            } else {
                content = content.push(text("Calculando prévia de retenção…").size(12));
            }
        } else if self.loading {
            content = content.push(text("Carregando biblioteca…"));
        } else if let Some(error) = &self.error {
            content = content.push(text(error.as_str()).size(14));
        } else if let Some(snapshot) = &self.snapshot {
            let first = if snapshot.total == 0 {
                0
            } else {
                self.offset + 1
            };
            let last = (self.offset + snapshot.captures.len()).min(snapshot.total);
            content = content.push(
                text(format!(
                    "Suas capturas · {} itens · {}–{}",
                    snapshot.total, first, last
                ))
                .size(17),
            );
            if snapshot.captures.is_empty() {
                content = content.push(text("Nenhuma captura encontrada."));
            }
            for group in snapshot.captures.chunks(3) {
                let mut cards = row![].spacing(12);
                for capture in group {
                    let excerpt = self
                        .text_previews
                        .get(&capture.id)
                        .map(String::as_str)
                        .unwrap_or(&capture.content_text)
                        .chars()
                        .take(170)
                        .collect::<String>();
                    let excerpt = if excerpt.trim().is_empty() {
                        preview_fallback_text(capture)
                    } else {
                        excerpt
                    };
                    let source = capture
                        .source_app_name
                        .as_deref()
                        .unwrap_or("Aplicativo desconhecido");
                    let mut body =
                        column![text(capture.content_kind.to_uppercase()).size(10)].spacing(9);
                    let link_preview = self.link_previews.get(&capture.id);
                    if let Some(favicon) = link_preview.and_then(|preview| preview.favicon.clone())
                    {
                        body = body.push(
                            row![
                                iced_image(favicon).height(18).width(18),
                                text(
                                    link_preview
                                        .and_then(|preview| preview.title.as_deref())
                                        .unwrap_or(source)
                                )
                                .size(12),
                            ]
                            .spacing(8)
                            .align_y(Alignment::Center),
                        );
                    } else if let Some(title) =
                        link_preview.and_then(|preview| preview.title.as_deref())
                    {
                        body = body.push(text(title).size(12));
                    }
                    if let Some(thumbnail) = self.thumbnails.get(&capture.id) {
                        body = body.push(
                            iced_image(thumbnail.clone())
                                .height(80)
                                .width(Length::Fill)
                                .content_fit(iced::ContentFit::Contain),
                        );
                    } else if let Some(thumbnail) =
                        link_preview.and_then(|preview| preview.image.clone())
                    {
                        body = body.push(
                            iced_image(thumbnail)
                                .height(80)
                                .width(Length::Fill)
                                .content_fit(iced::ContentFit::Contain),
                        );
                    } else {
                        let excerpt = if capture.content_kind == "url" && self.link_previews_loading
                        {
                            "Buscando prévia do site…".to_string()
                        } else {
                            excerpt
                        };
                        body = body.push(text(excerpt).size(15));
                    }
                    if let Some(file) = capture.files.first() {
                        let kind = file.extension.as_deref().unwrap_or("arquivo");
                        body = body.push(
                            text(format!("{} · {}", file.display_name, kind.to_uppercase()))
                                .size(11),
                        );
                    }
                    body = body
                        .push(Space::new().height(Length::Fill))
                        .push(text(source).size(11));
                    cards = cards.push(
                        button(
                            container(body)
                                .width(Length::Fill)
                                .height(160)
                                .padding(15)
                                .style(move |_| panel_style(dark, false)),
                        )
                        .on_press(Message::SelectCapture(capture.id.clone()))
                        .style(move |_, _| button::Style {
                            background: None,
                            text_color: if dark {
                                Color::from_rgb8(241, 243, 247)
                            } else {
                                Color::from_rgb8(37, 42, 54)
                            },
                            ..button::Style::default()
                        })
                        .width(Length::FillPortion(1)),
                    );
                }
                for _ in group.len()..3 {
                    cards = cards.push(Space::new().width(Length::FillPortion(1)));
                }
                content = content.push(cards);
            }
            if snapshot.total > PAGE_SIZE {
                content = content.push(
                    row![
                        button("Anterior")
                            .on_press_maybe((self.offset > 0).then_some(Message::PreviousPage)),
                        text(format!(
                            "Página {} de {}",
                            self.offset / PAGE_SIZE + 1,
                            snapshot.total.div_ceil(PAGE_SIZE)
                        ))
                        .size(12),
                        button("Próxima").on_press_maybe(
                            (self.offset + PAGE_SIZE < snapshot.total).then_some(Message::NextPage)
                        ),
                    ]
                    .spacing(12)
                    .align_y(Alignment::Center),
                );
            }
            if let Some(capture) = snapshot
                .captures
                .iter()
                .find(|capture| self.selected_capture.as_deref() == Some(capture.id.as_str()))
            {
                let mut details = column![
                    text("Detalhes da captura").size(18),
                    text(format!(
                        "{} · {}",
                        capture.content_kind.to_uppercase(),
                        capture.captured_at
                    ))
                    .size(12),
                    text(capture.content_text.as_str()).size(14),
                    text(format!(
                        "Origem: {}",
                        capture
                            .source_app_name
                            .as_deref()
                            .unwrap_or("Aplicativo desconhecido")
                    ))
                    .size(12),
                ]
                .spacing(10);
                if let Some(title) = &capture.window_title {
                    details = details.push(text(format!("Janela: {title}")).size(12));
                }
                if !capture.tags.is_empty() {
                    details =
                        details.push(text(format!("Tags: {}", capture.tags.join(", "))).size(12));
                }
                if !self.is_demo_mode() {
                    let mut tag_row = row![
                        text_input("Adicionar tag", &self.new_tag)
                            .on_input(Message::NewTagChanged)
                            .on_submit(Message::AddTag(capture.id.clone()))
                            .padding(7),
                        button("Adicionar").on_press_maybe(
                            (!self.action_busy).then_some(Message::AddTag(capture.id.clone()))
                        ),
                    ]
                    .spacing(6);
                    for tag in &capture.tags {
                        tag_row = tag_row.push(
                            button(text(format!("× {tag}"))).on_press_maybe(
                                (!self.action_busy)
                                    .then_some(Message::RemoveTag(capture.id.clone(), tag.clone())),
                            ),
                        );
                    }
                    details = details.push(tag_row);
                }
                if !capture.contexts.is_empty() {
                    details = details.push(
                        text(format!(
                            "Contextos: {}",
                            capture
                                .contexts
                                .iter()
                                .map(|c| c.name.as_str())
                                .collect::<Vec<_>>()
                                .join(", ")
                        ))
                        .size(12),
                    );
                    if !self.action_busy {
                        for context in &capture.contexts {
                            details = details.push(
                                button(text(format!("Remover de {}", context.name))).on_press(
                                    Message::RemoveContext(capture.id.clone(), context.id.clone()),
                                ),
                            );
                        }
                    }
                }
                for asset in &capture.assets {
                    details =
                        details.push(text(format!("{} · {}", asset.kind, asset.status)).size(11));
                }
                for file in &capture.files {
                    details = details.push(
                        text(format!(
                            "Arquivo: {} · {}",
                            file.display_name, file.availability
                        ))
                        .size(12),
                    );
                }
                if !self.action_busy {
                    for context in snapshot.contexts.iter().filter(|context| {
                        context.id != "inbox"
                            && context.id != "knowledge-base"
                            && !capture
                                .contexts
                                .iter()
                                .any(|assigned| assigned.id == context.id)
                    }) {
                        details = details.push(
                            button(text(format!("Adicionar ao {}", context.name))).on_press(
                                Message::AssignContext(capture.id.clone(), context.id.clone()),
                            ),
                        );
                    }
                } else {
                    details = details.push(text("Salvando Contexto…").size(12));
                }
                if let Some(ocr) = &capture.ocr {
                    if let Some(ocr_text) = &ocr.text {
                        details = details.push(text(format!("Texto OCR: {ocr_text}")).size(12));
                    } else {
                        let label = match ocr.status.as_str() {
                            "queued" => "OCR local aguardando análise…",
                            "running" => "Analisando imagem local…",
                            "failed" => "OCR local indisponível para esta imagem.",
                            _ => "OCR local ainda não disponível.",
                        };
                        details =
                            details.push(text(ocr.error.as_deref().unwrap_or(label)).size(11));
                    }
                }
                if self.pending_delete_capture.as_deref() == Some(capture.id.as_str()) {
                    details = details.push(
                        text("Excluir esta captura e seus arquivos locais? Essa ação não pode ser desfeita.")
                            .size(12),
                    );
                    details = details.push(
                        row![
                            button("Confirmar exclusão")
                                .on_press(Message::DeleteCapture(capture.id.clone())),
                            button("Cancelar").on_press(Message::CancelDeleteCapture),
                        ]
                        .spacing(8),
                    );
                } else {
                    details = details.push(
                        button("Excluir captura")
                            .on_press(Message::RequestDeleteCapture(capture.id.clone())),
                    );
                }
                content = content.push(
                    container(details)
                        .width(Length::Fill)
                        .padding(18)
                        .style(move |_| panel_style(dark, false)),
                );
            }
            if let Some(notice) = &self.notice {
                content = content.push(text(notice.as_str()).size(12));
            }
        }
        row![
            sidebar,
            container(scrollable(content))
                .width(Length::Fill)
                .height(Length::Fill)
                .padding(24)
        ]
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
    }

    fn quick_view(&self) -> Element<'_, Message> {
        let dark = self.dark;
        let minimal = self.quick_paste_minimal;
        let search = text_input("Buscar no histórico", &self.query)
            .id(self.quick_search_id.clone())
            .on_input(Message::QueryChanged)
            .on_submit(Message::Search)
            .padding(12);
        let mut items = column![
            row![
                text("Quick Paste").size(20),
                Space::new().width(Length::Fill),
                button(if minimal { "Minimalista" } else { "Normal" })
                    .on_press(Message::ToggleQuickPasteStyle),
                button("Fechar").on_press(Message::FinishQuickPaste)
            ]
            .align_y(Alignment::Center),
            text("Ctrl + Shift + V para abrir · escolha uma captura para colar").size(11),
            search
        ]
        .spacing(12);
        if let Some(snapshot) = &self.snapshot {
            for capture in &snapshot.captures {
                let excerpt = self
                    .text_previews
                    .get(&capture.id)
                    .unwrap_or(&capture.content_text)
                    .chars()
                    .take(120)
                    .collect::<String>();
                let excerpt = if excerpt.trim().is_empty() {
                    preview_fallback_text(capture)
                } else {
                    excerpt
                };
                let source = capture
                    .source_app_name
                    .as_deref()
                    .unwrap_or("Aplicativo desconhecido");
                let mut card = column![text(format!(
                    "{} · {}",
                    capture.content_kind.to_uppercase(),
                    source
                ))
                .size(10),]
                .spacing(7);
                let link_preview = self.link_previews.get(&capture.id);
                if let Some(favicon) = link_preview.and_then(|preview| preview.favicon.clone()) {
                    card = card.push(
                        row![
                            iced_image(favicon).height(16).width(16),
                            text(
                                link_preview
                                    .and_then(|preview| preview.title.as_deref())
                                    .unwrap_or(source)
                            )
                            .size(11),
                        ]
                        .spacing(7)
                        .align_y(Alignment::Center),
                    );
                } else if let Some(title) =
                    link_preview.and_then(|preview| preview.title.as_deref())
                {
                    card = card.push(text(title).size(11));
                }
                if let Some(thumbnail) = self.thumbnails.get(&capture.id) {
                    card = card.push(
                        iced_image(thumbnail.clone())
                            .height(96)
                            .width(Length::Fill)
                            .content_fit(iced::ContentFit::Contain),
                    );
                } else if let Some(thumbnail) =
                    link_preview.and_then(|preview| preview.image.clone())
                {
                    card = card.push(
                        iced_image(thumbnail)
                            .height(96)
                            .width(Length::Fill)
                            .content_fit(iced::ContentFit::Contain),
                    );
                } else {
                    let excerpt = if capture.content_kind == "url" && self.link_previews_loading {
                        "Buscando prévia do site…".to_string()
                    } else {
                        excerpt
                    };
                    card = card.push(text(excerpt).size(14));
                }
                if let Some(file) = capture.files.first() {
                    card = card.push(
                        text(format!(
                            "{} · {}",
                            file.display_name,
                            file.extension
                                .as_deref()
                                .unwrap_or("arquivo")
                                .to_uppercase()
                        ))
                        .size(10),
                    );
                }
                card = card.push(text(capture.captured_at.as_str()).size(10));
                items = items.push(
                    button(
                        container(card)
                            .width(Length::Fill)
                            .padding(12)
                            .style(move |_| {
                                if minimal {
                                    container::Style::default()
                                } else {
                                    panel_style(dark, false)
                                }
                            }),
                    )
                    .on_press(Message::PasteCapture(capture.id.clone()))
                    .style(move |_, _| button::Style {
                        background: None,
                        text_color: if dark {
                            Color::from_rgb8(241, 243, 247)
                        } else {
                            Color::from_rgb8(37, 42, 54)
                        },
                        ..button::Style::default()
                    })
                    .width(Length::Fill),
                );
            }
            if snapshot.total > PAGE_SIZE {
                items = items.push(
                    row![
                        button("‹")
                            .on_press_maybe((self.offset > 0).then_some(Message::PreviousPage)),
                        text(format!(
                            "{} / {}",
                            self.offset / PAGE_SIZE + 1,
                            snapshot.total.div_ceil(PAGE_SIZE)
                        ))
                        .size(11),
                        button("›").on_press_maybe(
                            (self.offset + PAGE_SIZE < snapshot.total).then_some(Message::NextPage)
                        ),
                    ]
                    .spacing(10)
                    .align_y(Alignment::Center),
                );
            }
        }
        if self.loading {
            items = items.push(text("Carregando histórico…").size(12));
        }
        if let Some(error) = &self.error {
            items = items.push(text(error.as_str()).size(12));
        }
        container(scrollable(items).height(Length::Fill))
            .width(Length::Fill)
            .height(Length::Fill)
            .padding(14)
            .style(move |_| {
                if minimal {
                    container::Style {
                        background: None,
                        border: iced::border::rounded(18),
                        ..container::Style::default()
                    }
                } else {
                    panel_style(dark, false)
                }
            })
            .into()
    }

    fn quick_context_view(&self) -> Element<'_, Message> {
        let dark = self.dark;
        let minimal = self.quick_context_minimal;
        let mut content = column![
            row![
                text("Organizar capturas").size(20),
                Space::new().width(Length::Fill),
                button(if minimal { "Minimalista" } else { "Normal" })
                    .on_press(Message::ToggleQuickContextStyle),
                button("Fechar").on_press(Message::FinishQuickContext),
            ]
            .align_y(Alignment::Center),
            text(format!(
                "Selecione uma ou mais capturas para adicionar a um Contexto · {} selecionada(s)",
                self.quick_context_selected.len()
            ))
            .size(12),
        ]
        .spacing(12);
        if let Some(snapshot) = &self.snapshot {
            for capture in &snapshot.captures {
                let selected = self.quick_context_selected.contains(&capture.id);
                let source = capture
                    .source_app_name
                    .as_deref()
                    .unwrap_or("Aplicativo desconhecido");
                let excerpt = self
                    .text_previews
                    .get(&capture.id)
                    .unwrap_or(&capture.content_text)
                    .chars()
                    .take(58)
                    .collect::<String>();
                let excerpt = if excerpt.trim().is_empty() {
                    preview_fallback_text(capture)
                } else {
                    excerpt
                };
                let title = self
                    .link_previews
                    .get(&capture.id)
                    .and_then(|preview| preview.title.as_deref());
                let favicon = self
                    .link_previews
                    .get(&capture.id)
                    .and_then(|preview| preview.favicon.clone());
                let mut item = row![].spacing(8).align_y(Alignment::Center);
                if let Some(favicon) = favicon {
                    item = item.push(iced_image(favicon).height(16).width(16));
                }
                let excerpt = title.map(str::to_string).unwrap_or_else(|| {
                    if capture.content_kind == "url" && self.link_previews_loading {
                        "Buscando prévia…".into()
                    } else {
                        excerpt
                    }
                });
                item = item.push(
                    text(format!(
                        "{} {} · {} · {}",
                        if selected { "[x]" } else { "[ ]" },
                        capture.content_kind.to_uppercase(),
                        source,
                        excerpt
                    ))
                    .size(11),
                );
                content = content.push(
                    button(item)
                        .on_press(Message::ToggleQuickContextCapture(capture.id.clone()))
                        .width(Length::Fill),
                );
            }

            if self.quick_context_selected.is_empty() {
                content =
                    content.push(text("Marque ao menos uma captura para continuar.").size(12));
            } else {
                let selected_captures = snapshot
                    .captures
                    .iter()
                    .filter(|capture| self.quick_context_selected.contains(&capture.id))
                    .collect::<Vec<_>>();
                if let Some(capture) = selected_captures.first() {
                    let mut preview =
                        column![
                            text(format!("Prévia · {}", capture.content_kind.to_uppercase()))
                                .size(10)
                        ]
                        .spacing(6);
                    if let Some(link_preview) = self.link_previews.get(&capture.id) {
                        let mut identity = row![].spacing(7).align_y(Alignment::Center);
                        if let Some(favicon) = link_preview.favicon.clone() {
                            identity = identity.push(iced_image(favicon).height(18).width(18));
                        }
                        if let Some(title) = link_preview.title.as_deref() {
                            identity = identity.push(text(title).size(11));
                        }
                        preview = preview.push(identity);
                    }
                    if let Some(thumbnail) = self.thumbnails.get(&capture.id) {
                        preview = preview.push(
                            iced_image(thumbnail.clone())
                                .height(88)
                                .width(Length::Fill)
                                .content_fit(iced::ContentFit::Contain),
                        );
                    } else if let Some(thumbnail) = self
                        .link_previews
                        .get(&capture.id)
                        .and_then(|preview| preview.image.clone())
                    {
                        preview = preview.push(
                            iced_image(thumbnail)
                                .height(88)
                                .width(Length::Fill)
                                .content_fit(iced::ContentFit::Contain),
                        );
                    } else {
                        let excerpt = self
                            .text_previews
                            .get(&capture.id)
                            .unwrap_or(&capture.content_text);
                        preview = preview.push(
                            text(if excerpt.trim().is_empty() {
                                preview_fallback_text(capture)
                            } else {
                                excerpt.chars().take(100).collect::<String>()
                            })
                            .size(11),
                        );
                    }
                    content = content.push(
                        container(preview)
                            .width(Length::Fill)
                            .padding(8)
                            .style(move |_| panel_style(dark, false)),
                    );
                }
                let available = snapshot
                    .contexts
                    .iter()
                    .filter(|context| {
                        context.id != "inbox"
                            && context.id != "knowledge-base"
                            && selected_captures.iter().any(|capture| {
                                !capture
                                    .contexts
                                    .iter()
                                    .any(|assigned| assigned.id == context.id)
                            })
                    })
                    .cloned()
                    .collect::<Vec<_>>();
                if available.is_empty() {
                    content = content.push(
                        text("As capturas selecionadas já estão nos Contextos disponíveis.")
                            .size(12),
                    );
                }
                for context in available {
                    content = content.push(
                        button(text(format!(
                            "Adicionar {} captura(s) a {}",
                            selected_captures.len(),
                            context.name
                        )))
                        .on_press_maybe(
                            (!self.action_busy).then_some(Message::AssignQuickContext(context.id)),
                        )
                        .width(Length::Fill),
                    );
                }
            }
        } else if self.loading {
            content = content.push(text("Carregando a captura salva…").size(12));
        }
        if self.action_busy {
            content = content.push(text("Salvando…").size(12));
        }
        if let Some(notice) = &self.notice {
            content = content.push(text(notice.as_str()).size(11));
        }
        content = content.push(button("Concluir depois").on_press(Message::FinishQuickContext));
        container(scrollable(content).height(Length::Fill))
            .width(Length::Fill)
            .height(Length::Fill)
            .padding(16)
            .style(move |_| {
                if minimal {
                    container::Style {
                        background: None,
                        border: iced::border::rounded(18),
                        ..container::Style::default()
                    }
                } else {
                    panel_style(dark, false)
                }
            })
            .into()
    }
}

fn hotkey_events() -> impl iced::futures::Stream<Item = Message> {
    iced::stream::channel(
        8,
        |mut output: iced::futures::channel::mpsc::Sender<Message>| async move {
            use iced::futures::{channel::mpsc, sink::SinkExt, StreamExt};
            let (sender, mut receiver) = mpsc::unbounded();
            let quick_paste_id = QUICK_PASTE_HOTKEY_ID.get().copied();
            let capture_id = CAPTURE_HOTKEY_ID.get().copied();
            let _ = thread::Builder::new()
                .name("scrypuppy-hotkey-events".into())
                .spawn(move || {
                    while let Ok(event) = GlobalHotKeyEvent::receiver().recv() {
                        if event.state != HotKeyState::Pressed {
                            continue;
                        }
                        if Some(event.id) == capture_id {
                            if sender.unbounded_send(Message::CaptureClipboard).is_err() {
                                break;
                            }
                            continue;
                        }
                        if Some(event.id) != quick_paste_id {
                            continue;
                        }
                        #[cfg(windows)]
                        let target = unsafe {
                            windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow().0
                                as isize
                        };
                        #[cfg(not(windows))]
                        let target = 0;
                        if sender
                            .unbounded_send(Message::OpenQuickPaste(target))
                            .is_err()
                        {
                            break;
                        }
                    }
                });
            while let Some(message) = receiver.next().await {
                if output.send(message).await.is_err() {
                    break;
                }
            }
        },
    )
}

fn tray_events() -> impl iced::futures::Stream<Item = Message> {
    iced::stream::channel(
        8,
        |mut output: iced::futures::channel::mpsc::Sender<Message>| async move {
            use iced::futures::{channel::mpsc, sink::SinkExt, StreamExt};
            let (sender, mut receiver) = mpsc::unbounded();
            let _ = TRAY_EVENT_SENDER.set(Mutex::new(Some(sender)));
            while let Some(message) = receiver.next().await {
                if output.send(message).await.is_err() {
                    break;
                }
            }
        },
    )
}

fn clipboard_events() -> impl iced::futures::Stream<Item = Message> {
    iced::stream::channel(
        8,
        |mut output: iced::futures::channel::mpsc::Sender<Message>| async move {
            use iced::futures::{channel::mpsc, sink::SinkExt, StreamExt};
            let (sender, mut receiver) = mpsc::unbounded();
            let _ = CLIPBOARD_EVENT_SENDER.set(Mutex::new(Some(sender)));
            while let Some(message) = receiver.next().await {
                if output.send(message).await.is_err() {
                    break;
                }
            }
        },
    )
}

fn send_clipboard_message(message: Message) {
    if let Some(sender) = CLIPBOARD_EVENT_SENDER.get() {
        if let Ok(sender) = sender.lock() {
            if let Some(sender) = sender.as_ref() {
                let _ = sender.unbounded_send(message);
            }
        }
    }
}

fn send_tray_message(message: Message) {
    if let Some(sender) = TRAY_EVENT_SENDER.get() {
        if let Ok(sender) = sender.lock() {
            if let Some(sender) = sender.as_ref() {
                let _ = sender.unbounded_send(message);
            }
        }
    }
}

#[cfg(windows)]
fn initialize_tray() {
    use tray_icon::{
        menu::{Menu, MenuEvent, MenuItem},
        Icon, TrayIconBuilder, TrayIconEvent,
    };

    TRAY_ICON.with(|slot| {
        if slot.borrow().is_some() {
            return;
        }
        let icon =
            match ::image::load_from_memory(include_bytes!("../../../src-tauri/icons/icon.png")) {
                Ok(icon) => icon.to_rgba8(),
                Err(error) => {
                    eprintln!("Não foi possível carregar o ícone da bandeja: {error}");
                    return;
                }
            };
        let (width, height) = icon.dimensions();
        let icon = match Icon::from_rgba(icon.into_raw(), width, height) {
            Ok(icon) => icon,
            Err(error) => {
                eprintln!("Ícone da bandeja inválido: {error}");
                return;
            }
        };
        let menu = Menu::new();
        let open = MenuItem::with_id("tray-open", "Abrir ScryPuppy", true, None);
        let quit = MenuItem::with_id("tray-quit", "Sair", true, None);
        if menu.append(&open).is_err() || menu.append(&quit).is_err() {
            eprintln!("Não foi possível criar o menu da bandeja.");
            return;
        }
        let tray = match TrayIconBuilder::new()
            .with_id("scrypuppy-main")
            .with_tooltip("ScryPuppy")
            .with_icon(icon)
            .with_menu(Box::new(menu))
            .with_menu_on_left_click(false)
            .build()
        {
            Ok(tray) => tray,
            Err(error) => {
                eprintln!("Não foi possível criar o ícone da bandeja: {error}");
                return;
            }
        };
        MenuEvent::set_event_handler(Some(|event: MenuEvent| {
            if event.id() == "tray-open" {
                send_tray_message(Message::ShowMain);
            } else if event.id() == "tray-quit" {
                send_tray_message(Message::QuitFromTray);
            }
        }));
        TrayIconEvent::set_event_handler(Some(|event| {
            if matches!(
                event,
                TrayIconEvent::Click {
                    button: tray_icon::MouseButton::Left,
                    button_state: tray_icon::MouseButtonState::Up,
                    ..
                } | TrayIconEvent::DoubleClick {
                    button: tray_icon::MouseButton::Left,
                    ..
                }
            ) {
                send_tray_message(Message::ShowMain);
            }
        }));
        *slot.borrow_mut() = Some(tray);
    });
}

#[cfg(windows)]
fn hide_window(id: window::Id) -> Task<Message> {
    use raw_window_handle::RawWindowHandle;
    use windows::Win32::{Foundation::HWND, UI::WindowsAndMessaging::ShowWindow};

    if !TRAY_ICON.with(|slot| slot.borrow().is_some()) {
        return window::close(id);
    }
    window::run(id, |window| {
        if let Ok(handle) = window.window_handle() {
            if let RawWindowHandle::Win32(handle) = handle.as_raw() {
                let hwnd = HWND(handle.hwnd.get() as *mut std::ffi::c_void);
                unsafe {
                    let _ = ShowWindow(hwnd, windows::Win32::UI::WindowsAndMessaging::SW_HIDE);
                }
            }
        }
    })
    .discard()
}

#[cfg(not(windows))]
fn hide_window(id: window::Id) -> Task<Message> {
    window::close(id)
}

#[cfg(windows)]
fn show_window(id: window::Id) -> Task<Message> {
    use raw_window_handle::RawWindowHandle;
    use windows::Win32::{Foundation::HWND, UI::WindowsAndMessaging::ShowWindow};

    Task::batch([
        window::run(id, |window| {
            if let Ok(handle) = window.window_handle() {
                if let RawWindowHandle::Win32(handle) = handle.as_raw() {
                    let hwnd = HWND(handle.hwnd.get() as *mut std::ffi::c_void);
                    unsafe {
                        let _ = ShowWindow(hwnd, windows::Win32::UI::WindowsAndMessaging::SW_SHOW);
                    }
                }
            }
        })
        .discard(),
        window::gain_focus(id),
    ])
}

#[cfg(not(windows))]
fn show_window(id: window::Id) -> Task<Message> {
    window::gain_focus(id)
}

#[cfg(debug_assertions)]
fn demo_snapshot() -> Snapshot {
    let sample = |id: &str, body: &str, kind: &str, source: &str| CaptureDto {
        id: id.into(),
        content_text: body.into(),
        captured_at: "2026-01-01".into(),
        source_app_name: Some(source.into()),
        source_app_id: None,
        source_process_id: None,
        source_process_path: None,
        window_title: None,
        window_id: None,
        platform: "windows".into(),
        kind: "capture".into(),
        content_kind: kind.into(),
        metadata: Default::default(),
        assets: vec![],
        representations: vec![],
        files: vec![],
        clipboard_formats: vec![],
        tags: vec![],
        entities: vec![],
        ocr: None,
        contexts: vec![],
    };
    Snapshot {
        contexts: vec![ContextDto {
            id: "work".into(),
            name: "Trabalho".into(),
            normalized_name: "trabalho".into(),
            slug: "trabalho".into(),
            created_at: "2026".into(),
            updated_at: "2026".into(),
            capture_count: 3,
        }],
        captures: vec![
            sample(
                "1",
                "Tudo que você copia, em um só lugar.",
                "texto",
                "Editor",
            ),
            sample("2", "https://pasteapp.io/", "link", "Navegador"),
            sample(
                "3",
                "Pesquisa de produto — próximos passos",
                "texto",
                "Notas",
            ),
        ],
        total: 3,
        asset_root: PathBuf::new(),
        dark: true,
        quick_paste_minimal: true,
        quick_context_minimal: true,
        clipboard_monitor_enabled: false,
        link_preview_fetch_enabled: true,
        capture_screenshots: true,
        clipboard_monitor_capture_screenshots: false,
        clipboard_monitor_quick_context_enabled: false,
        quick_context_enabled: true,
        onboarding_completed: true,
        launch_at_startup: false,
        retention_policy: "3_months".into(),
        ai_provider: "deepseek".into(),
        ai_model: "deepseek-v4-flash".into(),
        ai_api_key_configured: false,
        context_rules: Vec::new(),
    }
}

fn load_default_library(
    query: String,
    context: Option<String>,
    offset: usize,
) -> Result<Snapshot, String> {
    let (path, key) = default_database_credentials()?;
    load_library(&path, &key, query, context, offset)
        .map_err(|_| "Não foi possível abrir a biblioteca existente com segurança.".into())
}

fn default_database_credentials() -> Result<(PathBuf, String), String> {
    let appdata = dirs::data_dir().ok_or("Diretório de dados do Windows indisponível.")?;
    let app_dir = appdata.join("com.scryppy.desktop");
    let path = app_dir.join(DATABASE_FILE);
    let allow_create = !path.exists();
    std::fs::create_dir_all(&app_dir).map_err(|error| error.to_string())?;

    let database_entry = CredentialEntry::new("Scryppy", "database-key-v1")
        .map_err(|_| "Credencial local indisponível.")?;
    let database_key = match database_entry.get_password() {
        Ok(key) if !key.trim().is_empty() => key,
        Ok(_) | Err(CredentialError::NoEntry) if allow_create => {
            let key = format!(
                "{}{}",
                uuid::Uuid::new_v4().simple(),
                uuid::Uuid::new_v4().simple()
            );
            database_entry.set_password(&key).map_err(|_| {
                "Não foi possível salvar a chave no Gerenciador de Credenciais do Windows."
            })?;
            key
        }
        Ok(_) => return Err("A credencial da biblioteca existente está vazia.".into()),
        Err(CredentialError::NoEntry) => {
            return Err("A chave da biblioteca existente não foi encontrada no Gerenciador de Credenciais do Windows.".into());
        }
        Err(_) => return Err("Não foi possível acessar a chave da biblioteca existente.".into()),
    };

    let context_entry = CredentialEntry::new("Scryppy", "context-key-v1")
        .map_err(|_| "Credencial local indisponível.")?;
    match context_entry.get_password() {
        Ok(key) if !key.trim().is_empty() => {}
        Ok(_) | Err(CredentialError::NoEntry) if allow_create => {
            let key = format!(
                "{}{}",
                uuid::Uuid::new_v4().simple(),
                uuid::Uuid::new_v4().simple()
            );
            context_entry
                .set_password(&key)
                .map_err(|_| "Não foi possível salvar a chave dos Contextos no Gerenciador de Credenciais do Windows.")?;
        }
        Ok(_) => return Err("A credencial de Contextos existente está vazia.".into()),
        Err(CredentialError::NoEntry) => {
            return Err("A chave dos Contextos não foi encontrada no Gerenciador de Credenciais do Windows.".into());
        }
        Err(_) => return Err("Não foi possível acessar a chave dos Contextos.".into()),
    }

    let conn = scrypuppy_core::storage::open_or_create(&app_dir, &database_key)?;
    scrypuppy_core::schema::migrate(&conn).map_err(|error| error.to_string())?;
    if conn
        .query_row(
            "SELECT value FROM settings WHERE key = 'magic_search_engine'",
            [],
            |row| row.get::<_, String>(0),
        )
        .is_err()
    {
        conn.execute(
            "INSERT OR IGNORE INTO settings (key, value) VALUES ('magic_search_engine', 'local')",
            [],
        )
        .map_err(|error| error.to_string())?;
    }
    Ok((path, database_key))
}

fn save_theme_preference(dark: bool) -> Result<(), String> {
    let (path, key) = default_database_credentials()?;
    let conn = open_existing_read_write(&path, &key)?;
    conn.execute(
        "INSERT INTO settings (key, value) VALUES ('iced_theme_dark', ?1)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        [if dark { "true" } else { "false" }],
    )
    .map_err(|error| error.to_string())?;
    Ok(())
}

fn save_window_style_preference(key: &str, value: &str) -> Result<(), String> {
    save_setting_value(key, value)
}

fn save_setting_value(key: &str, value: &str) -> Result<(), String> {
    let (path, database_key) = default_database_credentials()?;
    let conn = open_existing_read_write(&path, &database_key)?;
    save_setting_value_in(&conn, key, value)
}

fn save_ai_provider_model(provider: &str, model: &str) -> Result<(), String> {
    let (path, database_key) = default_database_credentials()?;
    let mut conn = open_existing_read_write(&path, &database_key)?;
    save_ai_provider_model_in(&mut conn, provider, model)
}

fn save_ai_provider_model_in(
    conn: &mut rusqlite::Connection,
    provider: &str,
    model: &str,
) -> Result<(), String> {
    let provider_option = scrypuppy_core::ai::provider_options()
        .into_iter()
        .find(|option| option.id == provider)
        .ok_or("Provedor de IA inválido.")?;
    if !provider_option
        .models
        .iter()
        .any(|option| option.id == model)
    {
        return Err("Modelo de IA inválido para o provedor selecionado.".into());
    }
    let transaction = conn.transaction().map_err(|error| error.to_string())?;
    save_setting_value_in(&transaction, "ai_provider", provider)?;
    save_setting_value_in(&transaction, "ai_model", model)?;
    transaction.commit().map_err(|error| error.to_string())
}

fn save_ai_api_key(api_key: &str) -> Result<(), String> {
    let api_key = api_key.trim();
    if api_key.is_empty() || api_key.chars().count() > 4096 {
        return Err("A chave de API está vazia ou excede 4096 caracteres.".into());
    }
    let entry = CredentialEntry::new(CREDENTIAL_SERVICE, AI_KEY_CREDENTIAL)
        .map_err(|_| "Credencial de IA indisponível.")?;
    entry.set_password(api_key).map_err(|_| {
        "Não foi possível salvar a chave de IA no Gerenciador de Credenciais do Windows.".into()
    })
}

fn ai_api_key_is_configured() -> bool {
    match AI_API_KEY_STATUS.load(Ordering::Acquire) {
        1 => false,
        2 => true,
        _ => {
            let configured = CredentialEntry::new(CREDENTIAL_SERVICE, AI_KEY_CREDENTIAL)
                .and_then(|entry| entry.get_password())
                .is_ok();
            let status = if configured { 2 } else { 1 };
            let _ =
                AI_API_KEY_STATUS.compare_exchange(0, status, Ordering::AcqRel, Ordering::Acquire);
            AI_API_KEY_STATUS.load(Ordering::Acquire) == 2
        }
    }
}

fn clear_ai_api_key() -> Result<(), String> {
    let entry = CredentialEntry::new(CREDENTIAL_SERVICE, AI_KEY_CREDENTIAL)
        .map_err(|_| "Credencial de IA indisponível.")?;
    match entry.delete_credential() {
        Ok(()) | Err(CredentialError::NoEntry) => Ok(()),
        Err(_) => Err(
            "Não foi possível remover a chave de IA do Gerenciador de Credenciais do Windows."
                .into(),
        ),
    }
}

fn set_autostart_preference(enabled: bool) -> Result<(), String> {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        use windows::{
            core::PCWSTR,
            Win32::System::Registry::{
                RegCloseKey, RegCreateKeyExW, RegDeleteValueW, RegSetValueExW, HKEY,
                HKEY_CURRENT_USER, KEY_SET_VALUE, REG_OPTION_NON_VOLATILE, REG_SZ,
            },
        };
        let subkey: Vec<u16> = "Software\\Microsoft\\Windows\\CurrentVersion\\Run"
            .encode_utf16()
            .chain(Some(0))
            .collect();
        let name: Vec<u16> = "ScryPuppy".encode_utf16().chain(Some(0)).collect();
        let mut key = HKEY::default();
        let status = unsafe {
            RegCreateKeyExW(
                HKEY_CURRENT_USER,
                PCWSTR(subkey.as_ptr()),
                None,
                PCWSTR::null(),
                REG_OPTION_NON_VOLATILE,
                KEY_SET_VALUE,
                None,
                &mut key,
                None,
            )
        };
        if status.0 != 0 {
            return Err(format!("Registro do Windows ({})", status.0));
        }
        let result = if enabled {
            let executable = std::env::current_exe().map_err(|error| error.to_string())?;
            let command = format!("\"{}\"", executable.display());
            let data: Vec<u16> = std::ffi::OsStr::new(&command)
                .encode_wide()
                .chain(Some(0))
                .collect();
            let bytes =
                unsafe { std::slice::from_raw_parts(data.as_ptr().cast::<u8>(), data.len() * 2) };
            let status =
                unsafe { RegSetValueExW(key, PCWSTR(name.as_ptr()), None, REG_SZ, Some(bytes)) };
            if status.0 == 0 {
                Ok(())
            } else {
                Err(format!("Registro do Windows ({})", status.0))
            }
        } else {
            let status = unsafe { RegDeleteValueW(key, PCWSTR(name.as_ptr())) };
            if status.0 == 0 || status.0 == 2 {
                Ok(())
            } else {
                Err(format!("Registro do Windows ({})", status.0))
            }
        };
        unsafe {
            let _ = RegCloseKey(key);
        }
        result?;
    }
    #[cfg(not(windows))]
    if enabled {
        return Err("A inicialização automática está disponível no Windows.".into());
    }
    let save = save_setting_value("launch_at_startup", if enabled { "true" } else { "false" });
    save
}

fn load_retention_preview(policy: String) -> Result<RetentionPreview, String> {
    let (path, key) = default_database_credentials()?;
    let conn = open_existing_read_only(&path, &key)?;
    let candidates =
        scrypuppy_core::retention::candidate_ids(&conn, &policy, None, chrono::Utc::now())?;
    let ids = candidates
        .iter()
        .map(|(id, _)| id.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    let token = scrypuppy_core::crypto::sha256_hex(format!("{policy}\n{ids}").as_bytes());
    Ok(RetentionPreview {
        token,
        count: candidates.len(),
    })
}

fn apply_retention_policy(
    policy: String,
    selection_token: String,
    delete_existing: bool,
) -> Result<usize, String> {
    if !scrypuppy_core::retention::is_valid_policy(&policy) {
        return Err("Política de retenção inválida.".into());
    }
    let (path, key) = default_database_credentials()?;
    let conn = open_existing_read_only(&path, &key)?;
    let candidates =
        scrypuppy_core::retention::candidate_ids(&conn, &policy, None, chrono::Utc::now())?;
    let ids = candidates
        .iter()
        .map(|(id, _)| id.clone())
        .collect::<Vec<_>>();
    let expected =
        scrypuppy_core::crypto::sha256_hex(format!("{policy}\n{}", ids.join("\n")).as_bytes());
    if expected != selection_token {
        return Err("A biblioteca mudou após a prévia; confira os itens novamente.".into());
    }
    drop(conn);
    if delete_existing {
        for id in &ids {
            delete_capture_from_library(id.clone())?;
        }
    }
    let at = chrono::Utc::now().to_rfc3339();
    let (path, key) = default_database_credentials()?;
    let conn = open_existing_read_write(&path, &key)?;
    save_setting_value_in(&conn, "retention_policy", &policy)?;
    save_setting_value_in(
        &conn,
        "retention_started_at",
        if delete_existing {
            "1970-01-01T00:00:00+00:00"
        } else {
            &at
        },
    )?;
    save_setting_value_in(&conn, "retention_last_run_at", &at)?;
    Ok(if delete_existing { ids.len() } else { 0 })
}

fn run_retention_cleanup() -> Result<usize, String> {
    let (path, key) = default_database_credentials()?;
    let conn = open_existing_read_only(&path, &key)?;
    let at = chrono::Utc::now();
    let last_run = conn
        .query_row(
            "SELECT value FROM settings WHERE key = 'retention_last_run_at'",
            [],
            |row| row.get::<_, String>(0),
        )
        .ok()
        .and_then(|value| chrono::DateTime::parse_from_rfc3339(&value).ok());
    if last_run.is_some_and(|last| {
        at.signed_duration_since(last.with_timezone(&chrono::Utc)) < chrono::TimeDelta::hours(24)
    }) {
        return Ok(0);
    }
    let policy = conn
        .query_row(
            "SELECT value FROM settings WHERE key = 'retention_policy'",
            [],
            |row| row.get::<_, String>(0),
        )
        .unwrap_or_else(|_| "3_months".into());
    let policy = if scrypuppy_core::retention::is_valid_policy(&policy) {
        policy
    } else {
        "3_months".into()
    };
    let started_at = conn
        .query_row(
            "SELECT value FROM settings WHERE key = 'retention_started_at'",
            [],
            |row| row.get::<_, String>(0),
        )
        .ok();
    let ids = scrypuppy_core::retention::candidate_ids(&conn, &policy, started_at.as_deref(), at)?
        .into_iter()
        .map(|(id, _)| id)
        .collect::<Vec<_>>();
    drop(conn);
    for id in &ids {
        delete_capture_from_library(id.clone())?;
    }
    let stamp = at.to_rfc3339();
    let (path, key) = default_database_credentials()?;
    let conn = open_existing_read_write(&path, &key)?;
    save_setting_value_in(&conn, "retention_last_run_at", &stamp)?;
    Ok(ids.len())
}

fn start_retention_worker() {
    if RETENTION_WORKER_RUNNING
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return;
    }
    thread::spawn(|| loop {
        match run_retention_cleanup() {
            Ok(count) if count > 0 => {
                send_clipboard_message(Message::RetentionCleanupFinished(count));
            }
            Ok(_) => {}
            Err(error) => eprintln!("Limpeza automática por retenção falhou: {error}"),
        }
        thread::sleep(std::time::Duration::from_secs(24 * 60 * 60));
    });
}

fn persist_boolean_setting(key: &'static str, value: bool, demo: bool) -> Task<Message> {
    if demo {
        return Task::none();
    }
    let value = if value { "true" } else { "false" };
    Task::perform(
        async move { save_setting_value(key, value) },
        move |result| Message::SettingSaved(key, result),
    )
}

fn save_setting_value_in(
    conn: &rusqlite::Connection,
    key: &str,
    value: &str,
) -> Result<(), String> {
    conn.execute(
        "INSERT INTO settings (key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        rusqlite::params![key, value],
    )
    .map_err(|error| error.to_string())?;
    Ok(())
}

fn update_capture_tag(capture_id: &str, raw_tag: &str, remove: bool) -> Result<(), String> {
    let tag = raw_tag.trim();
    if tag.is_empty() || tag.chars().count() > 80 || tag.chars().any(char::is_control) {
        return Err("A tag precisa ter até 80 caracteres e não pode estar vazia.".into());
    }
    let (path, key) = default_database_credentials()?;
    let app_dir = path
        .parent()
        .ok_or("Diretório da biblioteca indisponível.")?;
    let mut conn = open_existing_read_write(&path, &key)?;
    let transaction = conn.transaction().map_err(|e| e.to_string())?;
    let exists: bool = transaction
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM captures WHERE id = ?1)",
            [capture_id],
            |row| row.get(0),
        )
        .map_err(|e| e.to_string())?;
    if !exists {
        return Err("Captura não encontrada.".into());
    }
    if remove {
        transaction
            .execute(
                "DELETE FROM capture_tags WHERE capture_id = ?1 AND tag = ?2",
                rusqlite::params![capture_id, tag],
            )
            .map_err(|e| e.to_string())?;
    } else {
        transaction.execute(
            "INSERT OR IGNORE INTO capture_tags (capture_id, tag, source, created_at) VALUES (?1, ?2, 'manual', ?3)",
            rusqlite::params![capture_id, tag, chrono::Utc::now().to_rfc3339()],
        ).map_err(|e| e.to_string())?;
    }
    let context_ids = {
        let mut stmt = transaction
            .prepare("SELECT context_id FROM capture_contexts WHERE capture_id = ?1")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([capture_id], |row| row.get::<_, String>(0))
            .map_err(|e| e.to_string())?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?
    };
    transaction.commit().map_err(|e| e.to_string())?;
    let timestamp = chrono::Utc::now().to_rfc3339();
    scrypuppy_core::search_index::refresh_search_document(&conn, capture_id, &timestamp)?;
    sync_context_exports_for_capture(&conn, app_dir, &context_ids)
}

fn preview_context_rule(
    context_id: &str,
    conditions: &[(String, String, String)],
    match_mode: &str,
) -> Result<usize, String> {
    if !matches!(match_mode, "all" | "any")
        || conditions.is_empty()
        || conditions.len() > 12
        || conditions.iter().any(|(field, operator, value)| {
            !matches!(
                field.as_str(),
                "text"
                    | "application"
                    | "window_title"
                    | "content_type"
                    | "file_extension"
                    | "file_path"
            ) || !matches!(operator.as_str(), "contains" | "equals" | "matches")
                || value.trim().is_empty()
                || value.trim().chars().count() > 500
                || (operator == "matches"
                    && regex::RegexBuilder::new(value.trim())
                        .case_insensitive(true)
                        .build()
                        .is_err())
        })
    {
        return Err("Revise o campo, operador e condições da regra.".into());
    }
    let (path, key) = default_database_credentials()?;
    let conn = open_existing_read_only(&path, &key)?;
    let mut stmt = conn
        .prepare(
            "SELECT c.id FROM captures c WHERE NOT EXISTS (
             SELECT 1 FROM capture_contexts cc WHERE cc.capture_id = c.id AND cc.context_id = ?1
         ) ORDER BY c.captured_at DESC, c.id",
        )
        .map_err(|error| error.to_string())?;
    let ids = stmt
        .query_map([context_id], |row| row.get::<_, String>(0))
        .map_err(|error| error.to_string())?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|error| error.to_string())?;
    drop(stmt);
    let conditions = conditions
        .iter()
        .map(|(field, operator, value)| (field.clone(), operator.clone(), value.trim().to_string()))
        .collect::<Vec<_>>();
    let mut matched = 0;
    for id in ids {
        let capture = scrypuppy_core::library::get_capture_by_id(&conn, &id)?;
        matched += usize::from(scrypuppy_core::context_rules::matches_rule(
            &capture,
            &conditions,
            match_mode,
        ));
    }
    Ok(matched)
}

fn create_text_context_rule(
    context_id: &str,
    name: &str,
    conditions: &[(String, String, String)],
    match_mode: &str,
) -> Result<usize, String> {
    let name = name.trim();
    if name.is_empty()
        || name.chars().count() > 80
        || conditions.is_empty()
        || conditions.len() > 12
        || !matches!(match_mode, "all" | "any")
        || conditions.iter().any(|(field, operator, value)| {
            !matches!(
                field.as_str(),
                "text"
                    | "application"
                    | "window_title"
                    | "content_type"
                    | "file_extension"
                    | "file_path"
            ) || !matches!(operator.as_str(), "contains" | "equals" | "matches")
                || value.trim().is_empty()
                || value.trim().chars().count() > 500
                || (operator == "matches"
                    && regex::RegexBuilder::new(value.trim())
                        .case_insensitive(true)
                        .build()
                        .is_err())
        })
    {
        return Err(
            "Informe um nome (até 80 caracteres) e até 12 condições (até 500 caracteres cada)."
                .into(),
        );
    }
    let (path, database_key) = default_database_credentials()?;
    let mut conn = open_existing_read_write(&path, &database_key)?;
    let transaction = conn.transaction().map_err(|e| e.to_string())?;
    let exists: bool = transaction
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM contexts WHERE id = ?1 AND is_builtin = 0)",
            [context_id],
            |row| row.get(0),
        )
        .map_err(|e| e.to_string())?;
    if !exists {
        return Err("As regras só podem ser adicionadas a Contextos personalizados.".into());
    }
    let id = uuid::Uuid::new_v4().to_string();
    let at = chrono::Utc::now().to_rfc3339();
    transaction.execute(
        "INSERT INTO context_rules (id, context_id, name, enabled, match_mode, created_at, updated_at)
         VALUES (?1, ?2, ?3, 1, ?5, ?4, ?4)",
        rusqlite::params![id, context_id, name, at, match_mode],
    ).map_err(|e| e.to_string())?;
    for (ordinal, (field, operator, value)) in conditions.iter().enumerate() {
        transaction.execute(
            "INSERT INTO context_rule_conditions (id, rule_id, ordinal, field, operator, value, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            rusqlite::params![uuid::Uuid::new_v4().to_string(), id, ordinal as i64, field, operator, value.trim(), at],
        ).map_err(|e| e.to_string())?;
    }
    transaction.commit().map_err(|e| e.to_string())?;
    let app_dir = path
        .parent()
        .ok_or("Diretório da biblioteca indisponível.")?;
    let mut stmt = conn
        .prepare(
            "SELECT c.id FROM captures c WHERE NOT EXISTS (
             SELECT 1 FROM capture_contexts cc WHERE cc.capture_id = c.id AND cc.context_id = ?1
         ) ORDER BY c.captured_at DESC, c.id",
        )
        .map_err(|error| error.to_string())?;
    let capture_ids = stmt
        .query_map([context_id], |row| row.get::<_, String>(0))
        .map_err(|error| error.to_string())?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|error| error.to_string())?;
    drop(stmt);
    let mut added = 0;
    for capture_id in capture_ids {
        let assigned = scrypuppy_core::context_rules::apply_enabled_rules(&conn, &capture_id)?;
        if assigned
            .iter()
            .any(|assigned_context| assigned_context == context_id)
        {
            added += 1;
        }
        sync_context_exports_for_capture(&conn, app_dir, &assigned)?;
    }
    Ok(added)
}

fn set_context_rule_enabled(id: &str, enabled: bool) -> Result<(), String> {
    let (path, database_key) = default_database_credentials()?;
    let conn = open_existing_read_write(&path, &database_key)?;
    let changed = conn
        .execute(
            "UPDATE context_rules SET enabled = ?2, updated_at = ?3 WHERE id = ?1",
            rusqlite::params![id, i64::from(enabled), chrono::Utc::now().to_rfc3339()],
        )
        .map_err(|e| e.to_string())?;
    if changed == 0 {
        return Err("Regra não encontrada.".into());
    }
    Ok(())
}

fn delete_context_rule(id: &str) -> Result<(), String> {
    let (path, database_key) = default_database_credentials()?;
    let conn = open_existing_read_write(&path, &database_key)?;
    let changed = conn
        .execute("DELETE FROM context_rules WHERE id = ?1", [id])
        .map_err(|e| e.to_string())?;
    if changed == 0 {
        return Err("Regra não encontrada.".into());
    }
    Ok(())
}

fn create_context_in_library(raw_name: String) -> Result<String, String> {
    let (path, database_key) = default_database_credentials()?;
    let app_dir = path
        .parent()
        .ok_or("Diretório da biblioteca indisponível.")?;
    let context_key = CredentialEntry::new("Scryppy", "context-key-v1")
        .map_err(|_| "Credencial local indisponível.")?
        .get_password()
        .map_err(|_| "Não foi possível acessar a chave dos Contextos.")?;
    let mut conn = open_existing_read_write(&path, &database_key)?;
    create_context_in(&mut conn, app_dir, &context_key, raw_name)
}

fn create_context_in(
    conn: &mut rusqlite::Connection,
    app_dir: &Path,
    context_key: &str,
    raw_name: String,
) -> Result<String, String> {
    let name = raw_name.split_whitespace().collect::<Vec<_>>().join(" ");
    if name.is_empty() {
        return Err("Informe um nome para o Contexto.".into());
    }
    if name.chars().count() > 80 {
        return Err("O nome do Contexto deve ter até 80 caracteres.".into());
    }
    let normalized_name = name.to_lowercase();
    if matches!(normalized_name.as_str(), "inbox" | "knowledge base") {
        return Err("Esse nome é reservado pelo sistema.".into());
    }
    let exists = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM contexts WHERE normalized_name = ?1)",
            [&normalized_name],
            |row| row.get::<_, bool>(0),
        )
        .map_err(|error| error.to_string())?;
    if exists {
        return Err("Já existe um Contexto com esse nome.".into());
    }
    let base_slug = slugify_context(&name);
    let mut slug = if base_slug.is_empty() {
        "contexto".to_string()
    } else {
        base_slug.clone()
    };
    let mut suffix = 2;
    while conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM contexts WHERE slug = ?1)",
            [&slug],
            |row| row.get::<_, bool>(0),
        )
        .map_err(|error| error.to_string())?
    {
        slug = format!("{base_slug}-{suffix}");
        suffix += 1;
    }
    let id = uuid::Uuid::new_v4().to_string();
    let timestamp = chrono::Utc::now().to_rfc3339();
    let context = ContextDto {
        id: id.clone(),
        name,
        normalized_name,
        slug,
        created_at: timestamp.clone(),
        updated_at: timestamp,
        capture_count: 0,
    };
    let transaction = conn.transaction().map_err(|error| error.to_string())?;
    transaction
        .execute(
            "INSERT INTO contexts (id, name, normalized_name, slug, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![
                context.id,
                context.name,
                context.normalized_name,
                context.slug,
                context.created_at,
                context.updated_at
            ],
        )
        .map_err(|error| error.to_string())?;
    transaction.commit().map_err(|error| error.to_string())?;

    let export_dir = app_dir.join("markdown").join("contexts");
    std::fs::create_dir_all(&export_dir).map_err(|error| error.to_string())?;
    scrypuppy_core::context_exports::write_context_export(
        &conn,
        &export_dir,
        &context,
        context_key,
    )
    .map_err(|error| {
        format!("Contexto criado, mas o export criptografado não foi escrito: {error}")
    })?;
    Ok(id)
}

fn rename_context_in_library(id: String, name: String) -> Result<String, String> {
    let (path, database_key) = default_database_credentials()?;
    let app_dir = path
        .parent()
        .ok_or("Diretório da biblioteca indisponível.")?;
    let context_key = CredentialEntry::new("Scryppy", "context-key-v1")
        .map_err(|_| "Credencial local indisponível.")?
        .get_password()
        .map_err(|_| "Não foi possível acessar a chave dos Contextos.")?;
    let mut conn = open_existing_read_write(&path, &database_key)?;
    rename_context_in(&mut conn, app_dir, &context_key, &id, &name)
}

fn rename_context_in(
    conn: &mut rusqlite::Connection,
    app_dir: &Path,
    context_key: &str,
    id: &str,
    raw_name: &str,
) -> Result<String, String> {
    if matches!(id, "inbox" | "knowledge-base") {
        return Err("Esse Contexto é protegido e não pode ser renomeado.".into());
    }
    let name = raw_name.split_whitespace().collect::<Vec<_>>().join(" ");
    if name.is_empty() || name.chars().count() > 80 {
        return Err("Informe um nome de até 80 caracteres.".into());
    }
    let normalized_name = name.to_lowercase();
    if matches!(normalized_name.as_str(), "inbox" | "knowledge base") {
        return Err("Esse nome é reservado pelo sistema.".into());
    }
    let contexts = scrypuppy_core::library::list_contexts_from_conn(conn)?;
    let old_context = contexts
        .iter()
        .find(|context| context.id == id)
        .ok_or("Contexto não encontrado.")?;
    if contexts
        .iter()
        .any(|context| context.id != id && context.normalized_name == normalized_name)
    {
        return Err("Já existe um Contexto com esse nome.".into());
    }
    let affected = context_capture_ids(conn, id)?;
    let base_slug = slugify_context(&name);
    let base_slug = if base_slug.is_empty() {
        "contexto".to_string()
    } else {
        base_slug
    };
    let mut slug = base_slug.clone();
    let mut suffix = 2;
    while conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM contexts WHERE slug = ?1 AND id <> ?2)",
            rusqlite::params![slug, id],
            |row| row.get::<_, bool>(0),
        )
        .map_err(|error| error.to_string())?
    {
        slug = format!("{base_slug}-{suffix}");
        suffix += 1;
    }
    let timestamp = chrono::Utc::now().to_rfc3339();
    let transaction = conn.transaction().map_err(|error| error.to_string())?;
    transaction
        .execute(
            "UPDATE contexts SET name = ?1, normalized_name = ?2, slug = ?3, updated_at = ?4 WHERE id = ?5",
            rusqlite::params![name, normalized_name, slug, timestamp, id],
        )
        .map_err(|error| error.to_string())?;
    transaction.commit().map_err(|error| error.to_string())?;

    let export_dir = app_dir.join("markdown").join("contexts");
    remove_context_export_file(&export_dir, &old_context.slug)?;
    let updated = scrypuppy_core::library::list_contexts_from_conn(conn)?
        .into_iter()
        .find(|context| context.id == id)
        .ok_or("Contexto não encontrado após a gravação.")?;
    std::fs::create_dir_all(&export_dir).map_err(|error| error.to_string())?;
    scrypuppy_core::context_exports::write_context_export(conn, &export_dir, &updated, context_key)
        .map_err(|error| {
            format!("Contexto renomeado, mas o export criptografado falhou: {error}")
        })?;
    for capture_id in affected {
        scrypuppy_core::search_index::refresh_search_document(conn, &capture_id, &timestamp)?;
    }
    Ok(id.to_string())
}

fn delete_context_from_library(id: String) -> Result<(), String> {
    let (path, database_key) = default_database_credentials()?;
    let app_dir = path
        .parent()
        .ok_or("Diretório da biblioteca indisponível.")?;
    let mut conn = open_existing_read_write(&path, &database_key)?;
    delete_context_in(&mut conn, app_dir, &id)
}

fn delete_context_in(
    conn: &mut rusqlite::Connection,
    app_dir: &Path,
    id: &str,
) -> Result<(), String> {
    if matches!(id, "inbox" | "knowledge-base") {
        return Err("Esse Contexto é protegido e não pode ser excluído.".into());
    }
    let context = scrypuppy_core::library::list_contexts_from_conn(conn)?
        .into_iter()
        .find(|context| context.id == id)
        .ok_or("Contexto não encontrado.")?;
    let affected = context_capture_ids(conn, id)?;
    let transaction = conn.transaction().map_err(|error| error.to_string())?;
    let deleted = transaction
        .execute("DELETE FROM contexts WHERE id = ?", [id])
        .map_err(|error| error.to_string())?;
    if deleted == 0 {
        return Err("Contexto não encontrado.".into());
    }
    transaction.commit().map_err(|error| error.to_string())?;

    remove_context_export_file(&app_dir.join("markdown").join("contexts"), &context.slug)?;
    let timestamp = chrono::Utc::now().to_rfc3339();
    for capture_id in affected {
        scrypuppy_core::search_index::refresh_search_document(conn, &capture_id, &timestamp)?;
    }
    Ok(())
}

fn context_capture_ids(conn: &rusqlite::Connection, id: &str) -> Result<Vec<String>, String> {
    let mut stmt = conn
        .prepare("SELECT capture_id FROM capture_contexts WHERE context_id = ?")
        .map_err(|error| error.to_string())?;
    let rows = stmt
        .query_map([id], |row| row.get::<_, String>(0))
        .map_err(|error| error.to_string())?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|error| error.to_string())?;
    Ok(rows)
}

fn remove_context_export_file(export_dir: &Path, slug: &str) -> Result<(), String> {
    if slug.is_empty()
        || !slug
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '-')
    {
        return Err("Slug do Contexto inválido.".into());
    }
    let path = export_dir.join(format!("{slug}.scryppy"));
    let directory_metadata = match std::fs::symlink_metadata(export_dir) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.to_string()),
    };
    if directory_metadata.file_type().is_symlink() || !directory_metadata.is_dir() {
        return Ok(());
    }
    let file_metadata = match std::fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.to_string()),
    };
    if file_metadata.file_type().is_symlink() || !file_metadata.is_file() {
        return Ok(());
    }
    scrypuppy_core::context_exports::secure_remove_file(&path)
}

fn slugify_context(value: &str) -> String {
    let mut slug = String::new();
    let mut separator = false;
    for character in value.chars().flat_map(char::to_lowercase) {
        if character.is_ascii_alphanumeric() {
            slug.push(character);
            separator = false;
        } else if !separator {
            slug.push('-');
            separator = true;
        }
    }
    slug.trim_matches('-').to_string()
}

fn assign_capture_to_context(capture_id: String, context_id: String) -> Result<(), String> {
    assign_captures_to_context(vec![capture_id], context_id)
}

fn sync_context_exports_for_capture(
    conn: &rusqlite::Connection,
    app_dir: &Path,
    context_ids: &[String],
) -> Result<(), String> {
    if context_ids.is_empty() {
        return Ok(());
    }
    let context_key = CredentialEntry::new("Scryppy", "context-key-v1")
        .map_err(|_| "Credencial local indisponível.")?
        .get_password()
        .map_err(|_| "Não foi possível acessar a chave dos Contextos.")?;
    let contexts =
        scrypuppy_core::context_exports::list_contexts(conn).map_err(|error| error.to_string())?;
    let export_dir = app_dir.join("markdown").join("contexts");
    std::fs::create_dir_all(&export_dir).map_err(|error| error.to_string())?;
    for context_id in context_ids {
        if let Some(context) = contexts.iter().find(|context| context.id == *context_id) {
            scrypuppy_core::context_exports::write_context_export(
                conn,
                &export_dir,
                context,
                &context_key,
            )
            .map_err(|error| {
                format!("Captura salva, mas o export criptografado do Contexto falhou: {error}")
            })?;
        }
    }
    Ok(())
}

fn assign_captures_to_context(capture_ids: Vec<String>, context_id: String) -> Result<(), String> {
    let appdata = dirs::data_dir().ok_or("Diretório de dados do Windows indisponível.")?;
    let app_dir = appdata.join("com.scryppy.desktop");
    let database_key = keyring::Entry::new("Scryppy", "database-key-v1")
        .map_err(|_| "Credencial local indisponível.")?
        .get_password()
        .map_err(|_| "Não foi possível acessar a chave da biblioteca existente.")?;
    let context_key = keyring::Entry::new("Scryppy", "context-key-v1")
        .map_err(|_| "Credencial local indisponível.")?
        .get_password()
        .map_err(|_| "Não foi possível acessar a chave dos Contextos.")?;
    assign_captures_to_context_in(
        &app_dir,
        &database_key,
        &context_key,
        &capture_ids,
        &context_id,
    )
}

fn remove_capture_from_context(capture_id: String, context_id: String) -> Result<(), String> {
    let appdata = dirs::data_dir().ok_or("Diretório de dados do Windows indisponível.")?;
    let app_dir = appdata.join("com.scryppy.desktop");
    let database_key = keyring::Entry::new("Scryppy", "database-key-v1")
        .map_err(|_| "Credencial local indisponível.")?
        .get_password()
        .map_err(|_| "Não foi possível acessar a chave da biblioteca existente.")?;
    let context_key = keyring::Entry::new("Scryppy", "context-key-v1")
        .map_err(|_| "Credencial local indisponível.")?
        .get_password()
        .map_err(|_| "Não foi possível acessar a chave dos Contextos.")?;
    remove_capture_from_context_in(
        &app_dir,
        &database_key,
        &context_key,
        &capture_id,
        &context_id,
    )
}

fn delete_capture_from_library(capture_id: String) -> Result<(), String> {
    let (path, database_key) = default_database_credentials()?;
    let app_dir = path
        .parent()
        .ok_or("Diretório da biblioteca indisponível.")?;
    let context_key = CredentialEntry::new("Scryppy", "context-key-v1")
        .map_err(|_| "Credencial local indisponível.")?
        .get_password()
        .map_err(|_| "Não foi possível acessar a chave dos Contextos.")?;
    let mut conn = open_existing_read_write(&path, &database_key)?;
    let mut asset_stmt = conn
        .prepare("SELECT path FROM capture_assets WHERE capture_id = ? AND path IS NOT NULL")
        .map_err(|error| error.to_string())?;
    let asset_paths = asset_stmt
        .query_map([&capture_id], |row| row.get::<_, String>(0))
        .map_err(|error| error.to_string())?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|error| error.to_string())?;
    drop(asset_stmt);
    let mut context_stmt = conn
        .prepare("SELECT context_id FROM capture_contexts WHERE capture_id = ?")
        .map_err(|error| error.to_string())?;
    let context_ids = context_stmt
        .query_map([&capture_id], |row| row.get::<_, String>(0))
        .map_err(|error| error.to_string())?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|error| error.to_string())?;
    drop(context_stmt);

    let transaction = conn.transaction().map_err(|error| error.to_string())?;
    let deleted = transaction
        .execute("DELETE FROM captures WHERE id = ?", [&capture_id])
        .map_err(|error| error.to_string())?;
    if deleted == 0 {
        return Err("Captura não encontrada.".into());
    }
    transaction.commit().map_err(|error| error.to_string())?;

    let assets_root = app_dir.join("assets");
    for raw_path in asset_paths {
        remove_managed_capture_file(&assets_root, Path::new(&raw_path))?;
    }
    remove_managed_capture_dir(
        &assets_root,
        &assets_root.join("clipboard-files").join(&capture_id),
    )?;

    let contexts =
        scrypuppy_core::context_exports::list_contexts(&conn).map_err(|error| error.to_string())?;
    let export_dir = app_dir.join("markdown").join("contexts");
    for context_id in context_ids {
        if matches!(context_id.as_str(), "inbox" | "knowledge-base") {
            continue;
        }
        if let Some(context) = contexts.iter().find(|context| context.id == context_id) {
            scrypuppy_core::context_exports::write_context_export(
                &conn,
                &export_dir,
                context,
                &context_key,
            )
            .map_err(|error| {
                format!("Captura excluída, mas o export criptografado do Contexto não foi atualizado: {error}")
            })?;
        }
    }
    Ok(())
}

fn remove_managed_capture_file(assets_root: &Path, path: &Path) -> Result<(), String> {
    if !path.starts_with(assets_root) {
        return Ok(());
    }
    let mut ancestors = path
        .ancestors()
        .take_while(|candidate| candidate.starts_with(assets_root))
        .collect::<Vec<_>>();
    ancestors.reverse();
    for ancestor in ancestors.iter().take(ancestors.len().saturating_sub(1)) {
        let metadata = match std::fs::symlink_metadata(ancestor) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error.to_string()),
        };
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Ok(());
        }
    }
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.to_string()),
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Ok(());
    }
    scrypuppy_core::context_exports::secure_remove_file(path)
}

fn remove_managed_capture_dir(assets_root: &Path, directory: &Path) -> Result<(), String> {
    if !directory.starts_with(assets_root) {
        return Ok(());
    }
    let mut ancestors = directory
        .ancestors()
        .take_while(|candidate| candidate.starts_with(assets_root))
        .collect::<Vec<_>>();
    ancestors.reverse();
    for ancestor in ancestors {
        let metadata = match std::fs::symlink_metadata(ancestor) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error.to_string()),
        };
        if metadata.file_type().is_symlink() {
            return Ok(());
        }
        if !metadata.is_dir() {
            return Ok(());
        }
    }
    for entry in std::fs::read_dir(directory).map_err(|error| error.to_string())? {
        let path = entry.map_err(|error| error.to_string())?.path();
        let metadata = std::fs::symlink_metadata(&path).map_err(|error| error.to_string())?;
        if metadata.file_type().is_symlink() {
            return Ok(());
        } else if metadata.is_dir() {
            remove_managed_capture_dir(assets_root, &path)?;
        } else if metadata.is_file() {
            remove_managed_capture_file(assets_root, &path)?;
        }
    }
    std::fs::remove_dir(directory).map_err(|error| error.to_string())
}

fn remove_capture_from_context_in(
    app_dir: &Path,
    database_key: &str,
    context_key: &str,
    capture_id: &str,
    context_id: &str,
) -> Result<(), String> {
    let path = app_dir.join(DATABASE_FILE);
    let conn = open_existing_read_write(&path, database_key).map_err(|_| {
        "Não foi possível abrir a biblioteca com segurança para edição.".to_string()
    })?;
    scrypuppy_core::contexts::remove_capture_context(&conn, capture_id, context_id)
        .map_err(|error| error.code)?;
    let at = chrono::Utc::now().to_rfc3339();
    scrypuppy_core::search_index::refresh_search_document(&conn, capture_id, &at)?;
    let context = scrypuppy_core::context_exports::list_contexts(&conn)
        .map_err(|error| error.to_string())?
        .into_iter()
        .find(|context| context.id == context_id)
        .ok_or("Contexto não encontrado após a gravação.")?;
    let export_dir = app_dir.join("markdown").join("contexts");
    std::fs::create_dir_all(&export_dir).map_err(|error| error.to_string())?;
    scrypuppy_core::context_exports::write_context_export(&conn, &export_dir, &context, context_key)
        .map_err(|error| {
            format!(
            "Contexto atualizado, mas não foi possível atualizar seu export criptografado: {error}"
        )
        })
}

fn capture_current_clipboard() -> Result<String, String> {
    let _capture_session = CAPTURE_COORDINATOR
        .get_or_init(scrypuppy_core::coordination::CaptureCoordinator::default)
        .begin()?;
    let (path, database_key) = default_database_credentials()?;
    let app_dir = path
        .parent()
        .ok_or("Diretório da biblioteca indisponível.")?;
    let settings_conn = open_existing_read_only(&path, &database_key)?;
    let include_images = read_boolean_setting(&settings_conn, "capture_screenshots", true);
    drop(settings_conn);
    let clipboard = scrypuppy_core::clipboard::ClipboardService::start()?;
    let read = clipboard.read();
    clipboard.shutdown();
    let mut snapshot = read?.ok_or("O clipboard está vazio ou indisponível.")?;
    if !include_images {
        snapshot.representations.retain(|representation| {
            !matches!(
                representation,
                scrypuppy_core::clipboard::ClipboardRepresentation::Image(_)
            )
        });
    }
    let active = active_window_metadata();
    let mut conn = scrypuppy_core::storage::open_existing_read_write(&path, &database_key)
        .map_err(|_| {
            "Não foi possível abrir a biblioteca com segurança para edição.".to_string()
        })?;
    let id = scrypuppy_core::capture_store::save_clipboard_capture_with_window_metadata(
        &mut conn,
        app_dir,
        &snapshot,
        "capture",
        "explicit_hotkey",
        active.app_name.as_deref(),
        active.app_id.as_deref(),
        active.process_id,
        active.process_path.as_deref(),
        active.window_title.as_deref(),
        active.window_id.as_deref(),
    )?;
    let context_ids = scrypuppy_core::context_rules::apply_enabled_rules(&conn, &id)?;
    sync_context_exports_for_capture(&conn, app_dir, &context_ids)?;
    if scrypuppy_core::ocr::enqueue_capture(&conn, &id)? {
        start_ocr_worker();
    }
    Ok(id)
}

#[derive(Default)]
struct ActiveWindowMetadata {
    app_name: Option<String>,
    app_id: Option<String>,
    process_id: Option<i64>,
    process_path: Option<String>,
    window_title: Option<String>,
    window_id: Option<String>,
}

fn active_window_metadata() -> ActiveWindowMetadata {
    #[cfg(windows)]
    {
        if let Ok(window) = active_win_pos_rs::get_active_window() {
            return ActiveWindowMetadata {
                app_name: (!window.app_name.trim().is_empty()).then_some(window.app_name),
                app_id: None,
                process_id: Some(window.process_id as i64),
                process_path: Some(window.process_path.display().to_string()),
                window_title: (!window.title.trim().is_empty()).then_some(window.title),
                window_id: Some(window.window_id),
            };
        }
    }
    ActiveWindowMetadata::default()
}

fn capture_monitored_clipboard() -> Result<Option<(String, bool)>, String> {
    if !CLIPBOARD_MONITOR_ENABLED.load(Ordering::Acquire) {
        return Ok(None);
    }
    let _capture_session = CAPTURE_COORDINATOR
        .get_or_init(scrypuppy_core::coordination::CaptureCoordinator::default)
        .begin()?;
    let Ok((path, database_key)) = default_database_credentials() else {
        return Ok(None);
    };
    let settings_conn = open_existing_read_only(&path, &database_key)?;
    let enabled = read_boolean_setting(&settings_conn, "clipboard_monitor_enabled", false);
    let include_images = read_boolean_setting(
        &settings_conn,
        "clipboard_monitor_capture_screenshots",
        false,
    );
    let prompt_quick_context = read_boolean_setting(&settings_conn, "quick_context_enabled", true)
        && read_boolean_setting(
            &settings_conn,
            "clipboard_monitor_quick_context_enabled",
            false,
        );
    drop(settings_conn);
    if !enabled || !CLIPBOARD_MONITOR_ENABLED.load(Ordering::Acquire) {
        return Ok(None);
    }

    let clipboard = scrypuppy_core::clipboard::ClipboardService::start()?;
    let read = clipboard.read();
    clipboard.shutdown();
    let Some(mut snapshot) = read? else {
        return Ok(None);
    };
    if !include_images {
        snapshot.representations.retain(|representation| {
            !matches!(
                representation,
                scrypuppy_core::clipboard::ClipboardRepresentation::Image(_)
            )
        });
    }
    let mut conn = open_existing_read_write(&path, &database_key)?;
    let app_dir = path
        .parent()
        .ok_or("Diretório da biblioteca indisponível.")?;
    let active = active_window_metadata();
    match scrypuppy_core::capture_store::save_clipboard_capture_with_window_metadata(
        &mut conn,
        app_dir,
        &snapshot,
        "capture",
        "clipboard_monitor",
        active.app_name.as_deref(),
        active.app_id.as_deref(),
        active.process_id,
        active.process_path.as_deref(),
        active.window_title.as_deref(),
        active.window_id.as_deref(),
    ) {
        Ok(id) => {
            let context_ids = scrypuppy_core::context_rules::apply_enabled_rules(&conn, &id)?;
            sync_context_exports_for_capture(&conn, app_dir, &context_ids)?;
            if scrypuppy_core::ocr::enqueue_capture(&conn, &id)? {
                start_ocr_worker();
            }
            Ok(Some((id, prompt_quick_context)))
        }
        Err(error)
            if error == "DUPLICATE_CAPTURE_IGNORED"
                || error == "O clipboard nao retornou conteudo para salvar." =>
        {
            Ok(None)
        }
        Err(error) => Err(error),
    }
}

fn prepare_ocr_queue() -> Result<(), String> {
    let (path, key) = default_database_credentials()?;
    let conn = open_existing_read_write(&path, &key)?;
    scrypuppy_core::ocr::recover_interrupted_jobs(&conn)?;
    scrypuppy_core::ocr::enqueue_pending_jobs(&conn)?;
    drop(conn);
    start_ocr_worker();
    Ok(())
}

fn start_ocr_worker() {
    if OCR_WORKER_RUNNING
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return;
    }
    thread::spawn(|| {
        let mut worker_failed = false;
        loop {
            let job = (|| -> Result<Option<(String, PathBuf, PathBuf)>, String> {
                let (path, key) = default_database_credentials()?;
                let mut conn = open_existing_read_write(&path, &key)?;
                let Some((capture_id, asset_path)) =
                    scrypuppy_core::ocr::claim_next_job(&mut conn)?
                else {
                    return Ok(None);
                };
                let app_dir = path
                    .parent()
                    .ok_or("Diretório da biblioteca indisponível.")?
                    .to_path_buf();
                Ok(Some((capture_id, PathBuf::from(asset_path), app_dir)))
            })();
            let (capture_id, asset_path, app_dir) = match job {
                Ok(Some(job)) => job,
                Ok(None) => break,
                Err(error) => {
                    worker_failed = true;
                    eprintln!("Fila local de OCR indisponível: {error}");
                    break;
                }
            };
            let preview_root = app_dir.join("assets");
            let result = scrypuppy_core::preview::read_managed_asset(&preview_root, &asset_path)
                .map_err(|error| format!("Imagem local indisponível para OCR: {error:?}"))
                .and_then(|_| scrypuppy_core::ocr::run_local(&asset_path));
            let (path, key) = match default_database_credentials() {
                Ok(credentials) => credentials,
                Err(error) => {
                    worker_failed = true;
                    eprintln!("Não foi possível salvar o resultado de OCR: {error}");
                    break;
                }
            };
            let mut conn = match open_existing_read_write(&path, &key) {
                Ok(conn) => conn,
                Err(error) => {
                    worker_failed = true;
                    eprintln!("Não foi possível abrir a biblioteca para salvar OCR: {error}");
                    break;
                }
            };
            match scrypuppy_core::ocr::finish_job(&mut conn, &capture_id, result) {
                Ok(context_ids) => {
                    if let Err(error) =
                        sync_context_exports_for_capture(&conn, &app_dir, &context_ids)
                    {
                        eprintln!("OCR salvo, mas export de Contexto indisponível: {error}");
                    }
                    send_clipboard_message(Message::OcrUpdated(capture_id.clone()));
                }
                Err(error) => {
                    worker_failed = true;
                    eprintln!("Falha ao salvar resultado de OCR: {error}");
                    break;
                }
            }
        }
        OCR_WORKER_RUNNING.store(false, Ordering::Release);
        let queued = default_database_credentials()
            .ok()
            .and_then(|(path, key)| {
                open_existing_read_only(&path, &key).ok().and_then(|conn| {
                    conn.query_row(
                        "SELECT EXISTS(SELECT 1 FROM ocr_jobs WHERE status = 'queued')",
                        [],
                        |row| row.get::<_, bool>(0),
                    )
                    .ok()
                })
            })
            .unwrap_or(false);
        if queued && !worker_failed {
            start_ocr_worker();
        }
    });
}

fn paste_saved_capture(capture_id: String, target: isize) -> Result<(), String> {
    if target == 0 {
        return Err("Janela de destino indisponível; abra o Quick Paste pelo atalho no aplicativo de origem.".into());
    }
    let appdata = dirs::data_dir().ok_or("Diretório de dados do Windows indisponível.")?;
    let path = appdata.join("com.scryppy.desktop").join(DATABASE_FILE);
    let database_key = keyring::Entry::new("Scryppy", "database-key-v1")
        .map_err(|_| "Credencial local indisponível.")?
        .get_password()
        .map_err(|_| "Não foi possível acessar a chave da biblioteca existente.")?;
    let conn = open_existing_read_only(&path, &database_key)
        .map_err(|_| "Não foi possível abrir a biblioteca existente com segurança.".to_string())?;
    let capture = scrypuppy_core::library::get_capture_by_id(&conn, &capture_id)?;
    let snapshot = scrypuppy_core::restore::snapshot_from_capture(&capture)?;
    let clipboard = scrypuppy_core::clipboard::ClipboardService::start()?;
    let coordinator =
        CAPTURE_COORDINATOR.get_or_init(scrypuppy_core::coordination::CaptureCoordinator::default);
    let suppression = coordinator.suppress_monitor();
    let write = clipboard.write(snapshot);
    clipboard.shutdown();
    if write.is_ok() {
        #[cfg(windows)]
        coordinator.mark_internal_sequence(unsafe {
            windows::Win32::System::DataExchange::GetClipboardSequenceNumber()
        });
    }
    drop(suppression);
    write?;

    #[cfg(windows)]
    {
        use std::{ffi::c_void, thread, time::Duration};
        use windows::Win32::{
            Foundation::HWND,
            UI::WindowsAndMessaging::{GetForegroundWindow, SetForegroundWindow},
        };
        let target = HWND(target as *mut c_void);
        let mut restored = false;
        for _ in 0..20 {
            unsafe {
                let _ = SetForegroundWindow(target);
            }
            if unsafe { GetForegroundWindow() } == target {
                restored = true;
                break;
            }
            thread::sleep(Duration::from_millis(25));
        }
        if !restored {
            return Err("Não foi possível devolver o foco ao aplicativo de origem.".into());
        }
    }

    std::thread::sleep(std::time::Duration::from_millis(120));
    let mut enigo =
        enigo::Enigo::new(&enigo::Settings::default()).map_err(|error| error.to_string())?;
    use enigo::{Direction, Key, Keyboard};
    for key in [
        Key::Shift,
        Key::LShift,
        Key::RShift,
        Key::Control,
        Key::LControl,
        Key::RControl,
        Key::Meta,
        Key::Alt,
    ] {
        let _ = enigo.key(key, Direction::Release);
    }
    enigo
        .key(Key::Control, Direction::Press)
        .map_err(|error| error.to_string())?;
    let paste = enigo.key(Key::Unicode('v'), Direction::Click);
    let release = enigo.key(Key::Control, Direction::Release);
    paste.map_err(|error| error.to_string())?;
    release.map_err(|error| error.to_string())?;
    Ok(())
}

fn assign_captures_to_context_in(
    app_dir: &Path,
    database_key: &str,
    context_key: &str,
    capture_ids: &[String],
    context_id: &str,
) -> Result<(), String> {
    let path = app_dir.join(DATABASE_FILE);
    let mut conn = scrypuppy_core::storage::open_existing_read_write(&path, &database_key)
        .map_err(|_| {
            "Não foi possível abrir a biblioteca com segurança para edição.".to_string()
        })?;
    let at = chrono::Utc::now().to_rfc3339();
    scrypuppy_core::contexts::add_captures_to_context(&mut conn, capture_ids, context_id, &at)
        .map_err(|error| error.code)?;
    for capture_id in capture_ids {
        scrypuppy_core::search_index::refresh_search_document(&conn, capture_id, &at)?;
    }

    let context = scrypuppy_core::context_exports::list_contexts(&conn)
        .map_err(|error| error.to_string())?
        .into_iter()
        .find(|context| context.id == context_id)
        .ok_or("Contexto não encontrado após a gravação.")?;
    let export_dir = app_dir.join("markdown").join("contexts");
    std::fs::create_dir_all(&export_dir).map_err(|error| error.to_string())?;
    scrypuppy_core::context_exports::write_context_export(&conn, &export_dir, &context, context_key)
        .map_err(|error| {
            format!(
                "Contexto salvo, mas não foi possível atualizar seu export criptografado: {error}"
            )
        })
}

fn load_library(
    path: &Path,
    key: &str,
    query: String,
    context: Option<String>,
    offset: usize,
) -> Result<Snapshot, String> {
    let conn = open_existing_read_only(path, key)?;
    let filter = CaptureFilter {
        context_id: context,
        search: (!query.trim().is_empty()).then_some(query),
        tag: None,
        limit: Some(PAGE_SIZE),
        offset: Some(offset),
        exclude_references: false,
    };
    let dark = conn
        .query_row(
            "SELECT value FROM settings WHERE key = 'iced_theme_dark'",
            [],
            |row| row.get::<_, String>(0),
        )
        .map(|value| value != "false")
        .unwrap_or(true);
    let quick_paste_minimal = read_style_preference(&conn, "iced_quick_paste_style");
    let quick_context_minimal = read_style_preference(&conn, "iced_quick_context_style");
    let clipboard_monitor_enabled = read_boolean_setting(&conn, "clipboard_monitor_enabled", false);
    let link_preview_fetch_enabled =
        read_boolean_setting(&conn, "link_preview_fetch_enabled", true);
    let capture_screenshots = read_boolean_setting(&conn, "capture_screenshots", true);
    let clipboard_monitor_capture_screenshots =
        read_boolean_setting(&conn, "clipboard_monitor_capture_screenshots", false);
    let clipboard_monitor_quick_context_enabled =
        read_boolean_setting(&conn, "clipboard_monitor_quick_context_enabled", false);
    let quick_context_enabled = read_boolean_setting(&conn, "quick_context_enabled", true);
    let onboarding_completed = read_boolean_setting(&conn, "onboarding_completed", false);
    let launch_at_startup = read_boolean_setting(&conn, "launch_at_startup", false);
    let retention_policy = conn
        .query_row(
            "SELECT value FROM settings WHERE key = 'retention_policy'",
            [],
            |row| row.get::<_, String>(0),
        )
        .unwrap_or_else(|_| "3_months".into());
    let ai_provider = conn
        .query_row(
            "SELECT value FROM settings WHERE key = 'ai_provider'",
            [],
            |row| row.get::<_, String>(0),
        )
        .unwrap_or_else(|_| "deepseek".into());
    let ai_model = conn
        .query_row(
            "SELECT value FROM settings WHERE key = 'ai_model'",
            [],
            |row| row.get::<_, String>(0),
        )
        .unwrap_or_else(|_| scrypuppy_core::ai::default_model(&ai_provider));
    let ai_api_key_configured = ai_api_key_is_configured();
    let context_rules = {
        let mut stmt = conn
            .prepare(
                "SELECT r.id, r.context_id, r.name, r.enabled,
                        COALESCE(c.field || ' ' || c.operator || ': ' || c.value, '')
             FROM context_rules r LEFT JOIN context_rule_conditions c
             ON c.rule_id = r.id AND c.ordinal = 0 ORDER BY r.created_at",
            )
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |row| {
                Ok(ContextRuleRow {
                    id: row.get(0)?,
                    context_id: row.get(1)?,
                    name: row.get(2)?,
                    enabled: row.get::<_, i64>(3)? != 0,
                    condition: row.get(4)?,
                })
            })
            .map_err(|e| e.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?;
        rows
    };
    Ok(Snapshot {
        contexts: list_contexts_from_conn(&conn)?,
        total: count_captures_from_conn(&conn, &filter)?,
        captures: list_captures_from_conn(&conn, &filter)?,
        asset_root: path
            .parent()
            .ok_or("Diretório da biblioteca indisponível")?
            .join("assets"),
        dark,
        quick_paste_minimal,
        quick_context_minimal,
        clipboard_monitor_enabled,
        link_preview_fetch_enabled,
        capture_screenshots,
        clipboard_monitor_capture_screenshots,
        clipboard_monitor_quick_context_enabled,
        quick_context_enabled,
        onboarding_completed,
        launch_at_startup,
        retention_policy,
        ai_provider,
        ai_model,
        ai_api_key_configured,
        context_rules,
    })
}

fn read_style_preference(conn: &rusqlite::Connection, key: &str) -> bool {
    conn.query_row("SELECT value FROM settings WHERE key = ?1", [key], |row| {
        row.get::<_, String>(0)
    })
    .map(|value| value != "normal")
    .unwrap_or(true)
}

fn read_boolean_setting(conn: &rusqlite::Connection, key: &str, default: bool) -> bool {
    conn.query_row("SELECT value FROM settings WHERE key = ?1", [key], |row| {
        row.get::<_, String>(0)
    })
    .map(|value| value == "true")
    .unwrap_or(default)
}

fn previews(
    root: &Path,
    captures: &[CaptureDto],
) -> (Vec<(String, Vec<u8>)>, Vec<(String, String)>) {
    let mut images = Vec::new();
    let mut text = Vec::new();
    for capture in captures {
        if let Some(path) = capture
            .representations
            .iter()
            .find(|representation| representation.kind == "image")
            .and_then(|representation| representation.asset_path.as_deref())
        {
            if let Ok(bytes) = read_managed_asset(root, Path::new(path)) {
                if let Ok(preview) = image_thumbnail(&bytes) {
                    images.push((capture.id.clone(), preview.png));
                }
            }
        }
        for file in &capture.files {
            let Some(extension) = file.extension.as_deref() else {
                continue;
            };
            let bytes = if let Some(path) = file.local_path.as_deref() {
                read_managed_asset(root, Path::new(path))
            } else if let Some(path) = file.original_path.as_deref() {
                scrypuppy_core::preview::read_local_file_preview(Path::new(path))
            } else {
                continue;
            };
            if extension.eq_ignore_ascii_case("pdf") {
                if let Some(path) = file.local_path.as_deref().map(Path::new) {
                    if read_managed_asset(root, path).is_ok() {
                        if let Ok(preview) = render_pdf_first_page(path) {
                            images.push((capture.id.clone(), preview));
                        }
                    }
                }
                continue;
            }
            if !extension.eq_ignore_ascii_case("txt") && !extension.eq_ignore_ascii_case("docx") {
                continue;
            }
            if let Ok(bytes) = bytes {
                let excerpt = match Some(extension) {
                    Some(extension) if extension.eq_ignore_ascii_case("txt") => {
                        scrypuppy_core::preview::text_excerpt(&bytes)
                    }
                    Some(_) => scrypuppy_core::preview::docx_excerpt(&bytes),
                    None => continue,
                };
                if let Ok(excerpt) = excerpt {
                    text.push((capture.id.clone(), excerpt));
                }
            }
        }
    }
    (images, text)
}

#[cfg(windows)]
fn render_pdf_first_page(path: &Path) -> Result<Vec<u8>, String> {
    use windows::{
        core::HSTRING,
        Data::Pdf::PdfDocument,
        Storage::StorageFile,
        Storage::Streams::{DataReader, InMemoryRandomAccessStream},
        Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED},
    };
    let metadata = std::fs::metadata(path).map_err(|error| error.to_string())?;
    if !metadata.is_file() || metadata.len() == 0 || metadata.len() > 20 * 1024 * 1024 {
        return Err("PDF acima do limite seguro de prévia.".into());
    }
    // The preview worker owns this COM apartment and never blocks capture or window creation.
    let _ = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
    let file = StorageFile::GetFileFromPathAsync(&HSTRING::from(path.display().to_string()))
        .map_err(|error| error.to_string())?
        .get()
        .map_err(|error| error.to_string())?;
    let document = PdfDocument::LoadFromFileAsync(&file)
        .map_err(|error| error.to_string())?
        .get()
        .map_err(|error| error.to_string())?;
    if document.PageCount().map_err(|error| error.to_string())? == 0 {
        return Err("O PDF não contém páginas.".into());
    }
    let page = document.GetPage(0).map_err(|error| error.to_string())?;
    let stream = InMemoryRandomAccessStream::new().map_err(|error| error.to_string())?;
    page.RenderToStreamAsync(&stream)
        .map_err(|error| error.to_string())?
        .get()
        .map_err(|error| error.to_string())?;
    let size = stream.Size().map_err(|error| error.to_string())?;
    if size == 0 || size > 12 * 1024 * 1024 {
        return Err("A miniatura do PDF excedeu o limite seguro.".into());
    }
    let reader = DataReader::CreateDataReader(
        &stream
            .GetInputStreamAt(0)
            .map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;
    reader
        .LoadAsync(size as u32)
        .map_err(|error| error.to_string())?
        .get()
        .map_err(|error| error.to_string())?;
    let mut bytes = vec![0; size as usize];
    reader
        .ReadBytes(&mut bytes)
        .map_err(|error| error.to_string())?;
    let preview =
        image_thumbnail(&bytes).map_err(|error| format!("Prévia PDF inválida: {error:?}"))?;
    Ok(preview.png)
}

#[cfg(not(windows))]
fn render_pdf_first_page(_path: &Path) -> Result<Vec<u8>, String> {
    Err("A prévia visual de PDF está disponível no Windows.".into())
}

fn link_previews(
    root: &Path,
    captures: &[CaptureDto],
    fetch: bool,
) -> Vec<(String, LinkPreviewData)> {
    let cache_root = root.join("link-previews");
    let entries = captures
        .iter()
        .filter(|capture| capture.content_kind == "url")
        .filter_map(|capture| capture_link_url(capture).map(|url| (capture.id.clone(), url)))
        .collect::<VecDeque<_>>();
    if entries.is_empty() {
        return Vec::new();
    }
    let queue = Arc::new(Mutex::new(entries));
    std::thread::scope(|scope| {
        let workers = (0..4)
            .map(|_| {
                let queue = Arc::clone(&queue);
                let cache_root = cache_root.clone();
                scope.spawn(move || {
                    let mut results = Vec::new();
                    loop {
                        let entry = queue.lock().ok().and_then(|mut queue| queue.pop_front());
                        let Some((id, url)) = entry else { break };
                        results.push((id, load_link_preview(&cache_root, &url, fetch)));
                    }
                    results
                })
            })
            .collect::<Vec<_>>();
        workers
            .into_iter()
            .filter_map(|worker| worker.join().ok())
            .flatten()
            .collect()
    })
}

fn capture_link_url(capture: &CaptureDto) -> Option<String> {
    let candidate = capture
        .representations
        .iter()
        .find(|representation| representation.kind == "url")
        .and_then(|representation| representation.text_content.as_deref())
        .unwrap_or(&capture.content_text)
        .trim();
    let parsed = url::Url::parse(candidate).ok()?;
    matches!(parsed.scheme(), "http" | "https").then(|| parsed.to_string())
}

fn load_link_preview(cache_root: &Path, url: &str, fetch: bool) -> LinkPreviewData {
    use std::time::Duration;

    let hash = scrypuppy_core::crypto::sha256_hex(url.as_bytes());
    let marker_path = cache_root.join(format!("{hash}.cache"));
    let title_path = cache_root.join(format!("{hash}.title"));
    let image_path = cache_root.join(format!("{hash}.image.png"));
    let favicon_path = cache_root.join(format!("{hash}.favicon.png"));
    let read_cache = || LinkPreviewData {
        title: read_bounded_cache_file(&title_path, 4096)
            .and_then(|bytes| String::from_utf8(bytes).ok())
            .filter(|title| !title.trim().is_empty()),
        image: read_bounded_cache_file(&image_path, 3 * 1024 * 1024)
            .and_then(|bytes| image_thumbnail(&bytes).ok())
            .map(|preview| preview.png),
        favicon: read_bounded_cache_file(&favicon_path, 3 * 1024 * 1024)
            .and_then(|bytes| image_thumbnail(&bytes).ok())
            .map(|preview| preview.png),
    };
    let fresh = std::fs::metadata(&marker_path)
        .and_then(|metadata| metadata.modified())
        .ok()
        .and_then(|modified| modified.elapsed().ok())
        .is_some_and(|age| age < Duration::from_secs(7 * 24 * 60 * 60));
    if fresh || !fetch {
        return read_cache();
    }

    let fetched = scrypuppy_core::link_fetch::fetch_link(url);
    let Ok(fetched) = fetched else {
        return read_cache();
    };
    let image = fetched
        .image
        .as_deref()
        .and_then(|bytes| image_thumbnail(bytes).ok())
        .map(|preview| preview.png);
    let favicon = fetched
        .favicon
        .as_deref()
        .and_then(|bytes| image_thumbnail(bytes).ok())
        .map(|preview| preview.png);
    if std::fs::create_dir_all(cache_root).is_ok() {
        if let Some(title) = fetched.metadata.title.as_deref() {
            let _ = std::fs::write(&title_path, title);
        }
        if let Some(bytes) = &image {
            let _ = std::fs::write(&image_path, bytes);
        } else {
            let _ = std::fs::remove_file(&image_path);
        }
        if let Some(bytes) = &favicon {
            let _ = std::fs::write(&favicon_path, bytes);
        } else {
            let _ = std::fs::remove_file(&favicon_path);
        }
        let _ = std::fs::write(marker_path, b"cached");
    }
    LinkPreviewData {
        title: fetched.metadata.title,
        image,
        favicon,
    }
}

fn read_bounded_cache_file(path: &Path, max_bytes: u64) -> Option<Vec<u8>> {
    let metadata = std::fs::metadata(path).ok()?;
    if !metadata.is_file() || metadata.len() > max_bytes {
        return None;
    }
    std::fs::read(path).ok()
}

fn preview_fallback_text(capture: &CaptureDto) -> String {
    let extension = capture
        .files
        .first()
        .and_then(|file| file.extension.as_deref())
        .unwrap_or_default()
        .to_ascii_lowercase();
    match extension.as_str() {
        "pdf" => "Prévia da primeira página do PDF indisponível.".into(),
        "doc" => "Prévia visual do documento Word indisponível.".into(),
        "docx" => "Prévia de texto do documento Word indisponível.".into(),
        "txt" => "Prévia do arquivo de texto indisponível.".into(),
        _ if capture.content_kind == "image" => "Miniatura da imagem indisponível.".into(),
        _ if capture.content_kind == "url" => {
            "Prévia do site indisponível; o link continua identificado.".into()
        }
        _ => "Prévia indisponível para este tipo de captura.".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use scrypuppy_core::storage::open_or_create;

    #[test]
    fn quick_paste_shortcut_is_registered_as_ctrl_shift_v() {
        let hotkey = HotKey::from_str("ctrl+shift+KeyV").unwrap();
        assert_ne!(hotkey.id(), 0);
    }

    #[test]
    fn capture_shortcut_matches_the_existing_default() {
        let hotkey = HotKey::from_str("ctrl+shift+KeyC").unwrap();
        assert_ne!(hotkey.id(), 0);
    }

    #[test]
    fn ai_provider_and_model_preferences_share_the_library_schema() {
        let mut conn = rusqlite::Connection::open_in_memory().unwrap();
        scrypuppy_core::schema::migrate(&conn).unwrap();
        save_ai_provider_model_in(&mut conn, "openai", "gpt-5-mini").unwrap();

        let provider: String = conn
            .query_row(
                "SELECT value FROM settings WHERE key = 'ai_provider'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let model: String = conn
            .query_row(
                "SELECT value FROM settings WHERE key = 'ai_model'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(provider, "openai");
        assert_eq!(model, "gpt-5-mini");
        assert!(save_ai_provider_model_in(&mut conn, "unknown", "model").is_err());
        assert_eq!(
            conn.query_row(
                "SELECT value FROM settings WHERE key = 'ai_provider'",
                [],
                |row| row.get::<_, String>(0),
            )
            .unwrap(),
            "openai"
        );
    }

    #[test]
    fn link_preview_uses_local_cache_without_fetch_when_disabled() {
        let dir = tempfile::tempdir().unwrap();
        let url = "http://127.0.0.1/private";
        let hash = scrypuppy_core::crypto::sha256_hex(url.as_bytes());
        std::fs::write(dir.path().join(format!("{hash}.title")), "Local title").unwrap();

        let preview = load_link_preview(dir.path(), url, false);

        assert_eq!(preview.title.as_deref(), Some("Local title"));
    }

    #[test]
    fn create_context_writes_database_row_and_encrypted_export() {
        let mut conn = rusqlite::Connection::open_in_memory().unwrap();
        scrypuppy_core::schema::migrate(&conn).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let id = create_context_in(
            &mut conn,
            dir.path(),
            &"34".repeat(32),
            "Pesquisa de produto".into(),
        )
        .unwrap();

        let context = scrypuppy_core::library::list_contexts_from_conn(&conn)
            .unwrap()
            .into_iter()
            .find(|context| context.id == id)
            .unwrap();
        assert_eq!(context.slug, "pesquisa-de-produto");
        let export = dir
            .path()
            .join("markdown/contexts")
            .join(format!("{}.scryppy", context.slug));
        assert!(export.is_file());
        assert!(create_context_in(
            &mut conn,
            dir.path(),
            &"34".repeat(32),
            "Pesquisa de produto".into(),
        )
        .is_err());
    }

    #[test]
    fn context_rename_and_delete_keep_capture_and_export_state_consistent() {
        let mut conn = rusqlite::Connection::open_in_memory().unwrap();
        scrypuppy_core::schema::migrate(&conn).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let context_key = "56".repeat(32);
        let context_id =
            create_context_in(&mut conn, dir.path(), &context_key, "Pesquisa".into()).unwrap();
        let capture_id = scrypuppy_core::capture_store::save_clipboard_capture(
            &mut conn,
            dir.path(),
            &scrypuppy_core::clipboard::ClipboardSnapshot {
                representations: vec![
                    scrypuppy_core::clipboard::ClipboardRepresentation::PlainText(
                        "nota de pesquisa".into(),
                    ),
                ],
                formats: Vec::new(),
            },
            "capture",
        )
        .unwrap();
        scrypuppy_core::contexts::add_capture_contexts(
            &mut conn,
            &capture_id,
            std::slice::from_ref(&context_id),
            || "2026-01-01T00:00:00Z".into(),
        )
        .unwrap();
        scrypuppy_core::search_index::refresh_search_document(
            &conn,
            &capture_id,
            "2026-01-01T00:00:00Z",
        )
        .unwrap();

        rename_context_in(
            &mut conn,
            dir.path(),
            &context_key,
            &context_id,
            "Pesquisa de produto",
        )
        .unwrap();
        let export_dir = dir.path().join("markdown/contexts");
        assert!(export_dir.join("pesquisa-de-produto.scryppy").is_file());
        assert!(!export_dir.join("pesquisa.scryppy").exists());
        let indexed_context: String = conn
            .query_row(
                "SELECT metadata FROM capture_search_documents WHERE capture_id = ?",
                [&capture_id],
                |row| row.get(0),
            )
            .unwrap();
        assert!(indexed_context.contains("Pesquisa de produto"));

        delete_context_in(&mut conn, dir.path(), &context_id).unwrap();
        assert!(!export_dir.join("pesquisa-de-produto.scryppy").exists());
        assert_eq!(
            conn.query_row(
                "SELECT count(*) FROM captures WHERE id = ?",
                [&capture_id],
                |row| { row.get::<_, i64>(0) }
            )
            .unwrap(),
            1
        );
        assert_eq!(
            conn.query_row(
                "SELECT count(*) FROM capture_contexts WHERE capture_id = ?",
                [&capture_id],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
            0
        );
    }

    #[test]
    fn workspace_reads_existing_encrypted_library_and_filters_it() {
        let dir = tempfile::tempdir().unwrap();
        let key = "12".repeat(32);
        let conn = open_or_create(dir.path(), &key).unwrap();
        conn.execute_batch("\
            CREATE TABLE captures(id TEXT, content_text TEXT, captured_at TEXT, source_app_name TEXT, source_app_id TEXT, source_process_id INTEGER, source_process_path TEXT, window_title TEXT, window_id TEXT, platform TEXT, metadata_json TEXT, capture_kind TEXT);\
            CREATE TABLE capture_assets(capture_id TEXT, id TEXT, kind TEXT, path TEXT, status TEXT, error TEXT, created_at TEXT);\
            CREATE TABLE capture_representations(capture_id TEXT, id TEXT, kind TEXT, format_name TEXT, mime_type TEXT, text_content TEXT, asset_path TEXT, size_bytes INTEGER, sha256 TEXT, restorable INTEGER, metadata_json TEXT, ordinal INTEGER);\
            CREATE TABLE capture_file_entries(capture_id TEXT, id TEXT, representation_id TEXT, ordinal INTEGER, display_name TEXT, original_path TEXT, local_path TEXT, entry_kind TEXT, extension TEXT, size_bytes INTEGER, sha256 TEXT, availability TEXT, metadata_json TEXT);\
            CREATE TABLE capture_clipboard_formats(capture_id TEXT, format_id INTEGER, format_name TEXT, supported INTEGER);\
            CREATE TABLE capture_tags(capture_id TEXT, tag TEXT);\
            CREATE TABLE capture_entities(capture_id TEXT, id TEXT, kind TEXT, value TEXT, source TEXT, confidence REAL);\
            CREATE TABLE capture_ocr(capture_id TEXT, status TEXT, text TEXT, error TEXT, updated_at TEXT);\
            CREATE TABLE contexts(id TEXT, name TEXT, normalized_name TEXT, slug TEXT, created_at TEXT, updated_at TEXT);\
            CREATE TABLE context_rules(id TEXT, context_id TEXT, name TEXT, enabled INTEGER, match_mode TEXT, created_at TEXT, updated_at TEXT);\
            CREATE TABLE context_rule_conditions(id TEXT, rule_id TEXT, ordinal INTEGER, field TEXT, operator TEXT, value TEXT, created_at TEXT);\
            CREATE TABLE capture_contexts(capture_id TEXT, context_id TEXT, assignment_origin TEXT, confidence REAL, created_at TEXT);\
            CREATE TABLE capture_search_documents(capture_id TEXT PRIMARY KEY, content TEXT, ocr TEXT, metadata TEXT, content_hash TEXT, updated_at TEXT);\
            CREATE TABLE capture_embeddings(capture_id TEXT PRIMARY KEY, content_hash TEXT);\
            CREATE TABLE settings(key TEXT PRIMARY KEY, value TEXT);\
            INSERT INTO captures VALUES ('1','hello world','2026-01-01','Editor',NULL,NULL,NULL,NULL,NULL,'windows','{}','capture');\
            INSERT INTO capture_representations VALUES ('1','r1','plain_text','Unicode',NULL,'hello world',NULL,11,NULL,1,'{}',0);\
            INSERT INTO contexts VALUES ('work','Trabalho','trabalho','trabalho','2026','2026');\
            INSERT INTO settings VALUES ('iced_theme_dark','false');\
            INSERT INTO capture_contexts VALUES ('1','work','manual',NULL,'2026');\
        ").unwrap();
        let image_dir = dir.path().join("assets").join("clipboard-images");
        std::fs::create_dir_all(&image_dir).unwrap();
        let image_path = image_dir.join("sample.png");
        ::image::RgbaImage::from_raw(1, 1, vec![12, 34, 56, 255])
            .unwrap()
            .save(&image_path)
            .unwrap();
        conn.execute("INSERT INTO captures VALUES ('2','image sample','2026-01-02','Editor',NULL,NULL,NULL,NULL,NULL,'windows','{}','capture')", []).unwrap();
        conn.execute("INSERT INTO capture_representations VALUES ('2','r2','image','PNG','image/png',NULL,?,4,NULL,1,'{}',0)", [image_path.to_string_lossy().as_ref()]).unwrap();
        for index in 3..=62 {
            conn.execute(
                &format!("INSERT INTO captures VALUES ('{index}', 'capture {index}', '2026-01-03', 'Editor',NULL,NULL,NULL,NULL,NULL,'windows','{{}}','capture')"),
                [],
            )
            .unwrap();
        }
        drop(conn);
        let path = dir.path().join(DATABASE_FILE);
        let snapshot = load_library(&path, &key, "hello".into(), Some("work".into()), 0).unwrap();
        assert_eq!(snapshot.total, 1);
        assert!(!snapshot.dark);
        assert_eq!(snapshot.contexts[0].name, "Trabalho");
        assert_eq!(snapshot.captures[0].content_kind, "plain_text");
        assert_eq!(snapshot.captures[0].contexts[0].id, "work");
        assert_eq!(snapshot.ai_provider, "deepseek");
        assert_eq!(snapshot.ai_model, "deepseek-v4-flash");
        let write_conn = open_existing_read_write(&path, &key).unwrap();
        write_conn
            .execute(
                "UPDATE settings SET value = 'true' WHERE key = 'iced_theme_dark'",
                [],
            )
            .unwrap();
        assert!(read_style_preference(&write_conn, "iced_quick_paste_style"));
        assert!(read_style_preference(
            &write_conn,
            "iced_quick_context_style"
        ));
        assert!(!read_boolean_setting(
            &write_conn,
            "clipboard_monitor_enabled",
            false
        ));
        assert!(read_boolean_setting(
            &write_conn,
            "quick_context_enabled",
            true
        ));
        assert!(!read_boolean_setting(
            &write_conn,
            "clipboard_monitor_quick_context_enabled",
            false
        ));
        save_setting_value_in(&write_conn, "clipboard_monitor_enabled", "true").unwrap();
        assert!(read_boolean_setting(
            &write_conn,
            "clipboard_monitor_enabled",
            false
        ));
        save_setting_value_in(&write_conn, "iced_quick_paste_style", "normal").unwrap();
        assert!(!read_style_preference(
            &write_conn,
            "iced_quick_paste_style"
        ));
        assert!(read_style_preference(
            &write_conn,
            "iced_quick_context_style"
        ));
        drop(write_conn);
        let filtered = load_library(&path, &key, "hello".into(), Some("work".into()), 0).unwrap();
        assert!(filtered.dark);
        assert!(!filtered.quick_paste_minimal);
        assert!(filtered.quick_context_minimal);
        let images = load_library(&path, &key, "image sample".into(), None, 0).unwrap();
        let (rendered, _) = previews(&images.asset_root, &images.captures);
        assert_eq!(rendered.len(), 1);
        assert_eq!(rendered[0].0, "2");
        assert_eq!(
            ::image::guess_format(&rendered[0].1).unwrap(),
            ::image::ImageFormat::Png
        );
        let first_page = load_library(&path, &key, String::new(), None, 0).unwrap();
        let second_page = load_library(&path, &key, String::new(), None, PAGE_SIZE).unwrap();
        assert_eq!(first_page.total, 62);
        assert_eq!(first_page.captures.len(), PAGE_SIZE);
        assert_eq!(second_page.captures.len(), 2);
        assert_eq!(
            load_library(&path, &key, "missing".into(), None, 0)
                .unwrap()
                .total,
            0
        );
        assert!(load_library(&path, &"34".repeat(32), String::new(), None, 0).is_err());
    }

    #[test]
    fn context_assignment_updates_database_search_and_encrypted_export() {
        let dir = tempfile::tempdir().unwrap();
        let key = "12".repeat(32);
        let context_key = "13".repeat(32);
        let conn = open_or_create(dir.path(), &key).unwrap();
        conn.execute_batch("\
            CREATE TABLE captures(id TEXT, content_text TEXT, captured_at TEXT, source_app_name TEXT, source_app_id TEXT, source_process_id INTEGER, source_process_path TEXT, window_title TEXT, window_id TEXT, platform TEXT, metadata_json TEXT, capture_kind TEXT);\
            CREATE TABLE capture_assets(capture_id TEXT, id TEXT, kind TEXT, path TEXT, status TEXT, error TEXT, created_at TEXT);\
            CREATE TABLE capture_representations(capture_id TEXT, id TEXT, kind TEXT, format_name TEXT, mime_type TEXT, text_content TEXT, asset_path TEXT, size_bytes INTEGER, sha256 TEXT, restorable INTEGER, metadata_json TEXT, ordinal INTEGER);\
            CREATE TABLE capture_file_entries(capture_id TEXT, id TEXT, representation_id TEXT, ordinal INTEGER, display_name TEXT, original_path TEXT, local_path TEXT, entry_kind TEXT, extension TEXT, size_bytes INTEGER, sha256 TEXT, availability TEXT, metadata_json TEXT);\
            CREATE TABLE capture_clipboard_formats(capture_id TEXT, format_id INTEGER, format_name TEXT, supported INTEGER);\
            CREATE TABLE capture_tags(capture_id TEXT, tag TEXT);\
            CREATE TABLE capture_entities(capture_id TEXT, id TEXT, kind TEXT, value TEXT, source TEXT, confidence REAL);\
            CREATE TABLE capture_ocr(capture_id TEXT, status TEXT, text TEXT, error TEXT, updated_at TEXT);\
            CREATE TABLE contexts(id TEXT, name TEXT, normalized_name TEXT, slug TEXT, created_at TEXT, updated_at TEXT);\
            CREATE TABLE capture_contexts(capture_id TEXT, context_id TEXT, assignment_origin TEXT, confidence REAL, created_at TEXT);\
            CREATE TABLE capture_search_documents(capture_id TEXT PRIMARY KEY, content TEXT, ocr TEXT, metadata TEXT, content_hash TEXT, updated_at TEXT);\
            CREATE TABLE capture_embeddings(capture_id TEXT PRIMARY KEY, content_hash TEXT);\
            INSERT INTO captures VALUES ('capture-1','hello world','2026-01-01','Editor',NULL,NULL,NULL,NULL,NULL,'windows','{}','capture'), ('capture-2','second item','2026-01-02','Browser',NULL,NULL,NULL,NULL,NULL,'windows','{}','capture');\
            INSERT INTO contexts VALUES ('work','Trabalho','trabalho','trabalho','2026','2026');\
        ").unwrap();
        drop(conn);

        assign_captures_to_context_in(
            dir.path(),
            &key,
            &context_key,
            &["capture-1".into(), "capture-2".into()],
            "work",
        )
        .unwrap();

        let conn = open_existing_read_only(&dir.path().join(DATABASE_FILE), &key).unwrap();
        assert_eq!(
            conn.query_row(
                "SELECT context_id FROM capture_contexts WHERE capture_id='capture-1'",
                [],
                |row| row.get::<_, String>(0)
            )
            .unwrap(),
            "work"
        );
        assert_eq!(
            conn.query_row(
                "SELECT count(*) FROM capture_contexts WHERE capture_id IN ('capture-1', 'capture-2') AND context_id='work'",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
            2
        );
        assert_eq!(
            conn.query_row(
                "SELECT content FROM capture_search_documents WHERE capture_id='capture-1'",
                [],
                |row| row.get::<_, String>(0)
            )
            .unwrap(),
            "hello world"
        );
        assert_eq!(
            conn.query_row(
                "SELECT content FROM capture_search_documents WHERE capture_id='capture-2'",
                [],
                |row| row.get::<_, String>(0)
            )
            .unwrap(),
            "second item"
        );
        let export = std::fs::read(dir.path().join("markdown/contexts/trabalho.scryppy")).unwrap();
        assert!(export.starts_with(scrypuppy_core::crypto::CONTEXT_FILE_MAGIC));
        assert!(!export.windows(7).any(|window| window == b"Trabalho"));

        remove_capture_from_context_in(dir.path(), &key, &context_key, "capture-1", "work")
            .unwrap();
        let conn = open_existing_read_only(&dir.path().join(DATABASE_FILE), &key).unwrap();
        assert_eq!(
            conn.query_row(
                "SELECT count(*) FROM capture_contexts WHERE capture_id='capture-1' AND context_id='work'",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
            0
        );
        let updated_export =
            std::fs::read(dir.path().join("markdown/contexts/trabalho.scryppy")).unwrap();
        assert!(updated_export.starts_with(scrypuppy_core::crypto::CONTEXT_FILE_MAGIC));
        assert_ne!(updated_export, export);
    }
}

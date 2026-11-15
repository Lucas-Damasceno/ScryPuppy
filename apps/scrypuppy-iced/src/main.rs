//! Native Iced UI for the ScryPuppy migration.
use global_hotkey::{hotkey::HotKey, GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};
use iced::widget::{
    button, column, container, image as iced_image, row, scrollable, text, text_input, Space,
};
use iced::{window, Alignment, Color, Element, Length, Size, Task, Theme};
use scrypuppy_core::library::{
    count_captures_from_conn, list_captures_from_conn, list_contexts_from_conn, CaptureDto,
    CaptureFilter, ContextDto,
};
use scrypuppy_core::preview::{image_thumbnail, read_managed_asset};
use scrypuppy_core::storage::{open_existing_read_only, open_existing_read_write, DATABASE_FILE};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    str::FromStr,
    sync::{Mutex, OnceLock},
    thread,
};

static QUICK_PASTE_HOTKEY_ID: OnceLock<u32> = OnceLock::new();
static TRAY_EVENT_SENDER: OnceLock<
    Mutex<Option<iced::futures::channel::mpsc::UnboundedSender<Message>>>,
> = OnceLock::new();
#[cfg(windows)]
thread_local! {
    static TRAY_ICON: std::cell::RefCell<Option<tray_icon::TrayIcon>> = const { std::cell::RefCell::new(None) };
}
const PAGE_SIZE: usize = 60;

fn main() -> iced::Result {
    let mut hotkey_manager = None;
    match GlobalHotKeyManager::new() {
        Ok(manager) => match HotKey::from_str("ctrl+shift+KeyV") {
            Ok(hotkey) => match manager.register(hotkey) {
                Ok(()) => {
                    let _ = QUICK_PASTE_HOTKEY_ID.set(hotkey.id());
                    hotkey_manager = Some(manager);
                }
                Err(error) => {
                    eprintln!("Não foi possível registrar o atalho global do Quick Paste: {error}")
                }
            },
            Err(error) => eprintln!("Atalho global do Quick Paste inválido: {error}"),
        },
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
            ])
        })
        .run();
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

#[derive(Clone, Debug)]
struct Snapshot {
    contexts: Vec<ContextDto>,
    captures: Vec<CaptureDto>,
    total: usize,
    asset_root: PathBuf,
    dark: bool,
    quick_paste_minimal: bool,
    quick_context_minimal: bool,
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
    offset: usize,
    selected_capture: Option<String>,
    theme_changed: bool,
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
    SelectCapture(String),
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
    ToggleQuickPasteStyle,
    ToggleQuickContextStyle,
    Loaded(u64, Result<Snapshot, String>),
    Previews(u64, Vec<(String, Vec<u8>)>, Vec<(String, String)>),
    ContextAssigned(u64, Result<(), String>),
    ContextRemoved(u64, Result<(), String>),
    Captured(u64, Result<String, String>),
    Pasted(Result<(), String>),
    ThemeSaved(Result<(), String>),
    QuickStyleSaved(Result<(), String>),
}

impl App {
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
            offset: 0,
            selected_capture: None,
            theme_changed: false,
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
            Message::MainOpened(id) if id == self.main => window::run(id, |_| {
                #[cfg(windows)]
                initialize_tray();
            })
            .discard(),
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
                    position: window::Position::Centered,
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
                    position: window::Position::Centered,
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
                self.context = id;
                self.offset = 0;
                self.selected_capture = None;
                self.reload()
            }
            Message::SelectCapture(id) => {
                self.selected_capture = Some(id);
                Task::none()
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
                        self.snapshot = Some(snapshot);
                        self.thumbnails.clear();
                        self.text_previews.clear();
                        return Task::perform(
                            async move { previews(&root, &captures) },
                            move |(images, text)| Message::Previews(revision, images, text),
                        );
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
                        return Task::done(Message::OpenQuickContext(id));
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
            Space::new().height(12),
            text("CONTEXTOS").size(11),
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
            text("Biblioteca").size(28),
            text("Biblioteca local · selecione uma captura para ver os detalhes").size(12),
            search
        ]
        .spacing(18);
        if self.loading {
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
                    if let Some(thumbnail) = self.thumbnails.get(&capture.id) {
                        body = body.push(
                            iced_image(thumbnail.clone())
                                .height(80)
                                .width(Length::Fill)
                                .content_fit(iced::ContentFit::Contain),
                        );
                    } else {
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
                    }
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
                if let Some(thumbnail) = self.thumbnails.get(&capture.id) {
                    card = card.push(
                        iced_image(thumbnail.clone())
                            .height(96)
                            .width(Length::Fill)
                            .content_fit(iced::ContentFit::Contain),
                    );
                } else {
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
                content = content.push(
                    button(
                        text(format!(
                            "{} {} · {} · {}",
                            if selected { "[x]" } else { "[ ]" },
                            capture.content_kind.to_uppercase(),
                            source,
                            excerpt
                        ))
                        .size(11),
                    )
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
                    if let Some(thumbnail) = self.thumbnails.get(&capture.id) {
                        preview = preview.push(
                            iced_image(thumbnail.clone())
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
            let expected_id = QUICK_PASTE_HOTKEY_ID.get().copied();
            let _ = thread::Builder::new()
                .name("scrypuppy-hotkey-events".into())
                .spawn(move || {
                    while let Ok(event) = GlobalHotKeyEvent::receiver().recv() {
                        if Some(event.id) != expected_id || event.state != HotKeyState::Pressed {
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
    let path = appdata.join("com.scryppy.desktop").join(DATABASE_FILE);
    if !path.is_file() {
        return Err(
            "Biblioteca existente não encontrada. A versão Iced ainda não cria novas bibliotecas."
                .into(),
        );
    }
    let entry = keyring::Entry::new("Scryppy", "database-key-v1")
        .map_err(|_| "Credencial local indisponível.")?;
    let key = entry
        .get_password()
        .map_err(|_| "Não foi possível acessar a chave da biblioteca existente.")?;
    Ok((path, key))
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
    let (path, database_key) = default_database_credentials()?;
    let conn = open_existing_read_write(&path, &database_key)?;
    save_window_style_preference_in(&conn, key, value)
}

fn save_window_style_preference_in(
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

fn assign_capture_to_context(capture_id: String, context_id: String) -> Result<(), String> {
    assign_captures_to_context(vec![capture_id], context_id)
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
    let appdata = dirs::data_dir().ok_or("Diretório de dados do Windows indisponível.")?;
    let app_dir = appdata.join("com.scryppy.desktop");
    let database_key = keyring::Entry::new("Scryppy", "database-key-v1")
        .map_err(|_| "Credencial local indisponível.")?
        .get_password()
        .map_err(|_| "Não foi possível acessar a chave da biblioteca existente.")?;
    let clipboard = scrypuppy_core::clipboard::ClipboardService::start()?;
    let read = clipboard.read();
    clipboard.shutdown();
    let snapshot = read?.ok_or("O clipboard está vazio ou indisponível.")?;
    let path = app_dir.join(DATABASE_FILE);
    let mut conn = scrypuppy_core::storage::open_existing_read_write(&path, &database_key)
        .map_err(|_| {
            "Não foi possível abrir a biblioteca com segurança para edição.".to_string()
        })?;
    scrypuppy_core::capture_store::save_clipboard_capture(&mut conn, &app_dir, &snapshot, "capture")
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
    let write = clipboard.write(snapshot);
    clipboard.shutdown();
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
    })
}

fn read_style_preference(conn: &rusqlite::Connection, key: &str) -> bool {
    conn.query_row("SELECT value FROM settings WHERE key = ?1", [key], |row| {
        row.get::<_, String>(0)
    })
    .map(|value| value != "normal")
    .unwrap_or(true)
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
        if let Some(file) = capture.files.iter().find(|file| {
            file.extension.as_deref().is_some_and(|extension| {
                extension.eq_ignore_ascii_case("txt") || extension.eq_ignore_ascii_case("docx")
            })
        }) {
            let bytes = if let Some(path) = file.local_path.as_deref() {
                read_managed_asset(root, Path::new(path))
            } else if let Some(path) = file.original_path.as_deref() {
                scrypuppy_core::preview::read_local_file_preview(Path::new(path))
            } else {
                continue;
            };
            if let Ok(bytes) = bytes {
                let excerpt = match file.extension.as_deref() {
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
        save_window_style_preference_in(&write_conn, "iced_quick_paste_style", "normal").unwrap();
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

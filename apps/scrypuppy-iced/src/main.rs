//! Native Iced UI for the ScryPuppy migration.
use global_hotkey::{hotkey::HotKey, GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};
use iced::widget::{
    button, column, container, image as iced_image, row, scrollable, text as iced_text,
    text_editor, text_input as iced_text_input, Space,
};
use iced::{window, Alignment, Color, Element, Length, Point, Size, Task, Theme};
use keyring::{Entry as CredentialEntry, Error as CredentialError};
use scrypuppy_core::library::{
    count_captures_from_conn, list_captures_from_conn, list_contexts_from_conn, CaptureDto,
    CaptureFilter, ContextDto,
};
use scrypuppy_core::local_search::{LocalSearchManager, LocalSearchPhase, LocalSearchStatus};
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
static REFERENCE_HOTKEY_ID: OnceLock<u32> = OnceLock::new();
static MAGIC_SEARCH_HOTKEY_ID: OnceLock<u32> = OnceLock::new();
static TRAY_EVENT_SENDER: OnceLock<
    Mutex<Option<iced::futures::channel::mpsc::UnboundedSender<Message>>>,
> = OnceLock::new();
static CLIPBOARD_EVENT_SENDER: OnceLock<
    Mutex<Option<iced::futures::channel::mpsc::UnboundedSender<Message>>>,
> = OnceLock::new();
static CLIPBOARD_MONITOR_ENABLED: AtomicBool = AtomicBool::new(false);
static DATA_RESET_IN_PROGRESS: AtomicBool = AtomicBool::new(false);
static OCR_WORKER_RUNNING: AtomicBool = AtomicBool::new(false);
static RETENTION_WORKER_RUNNING: AtomicBool = AtomicBool::new(false);
static AI_API_KEY_STATUS: AtomicU8 = AtomicU8::new(0);
static UI_LANGUAGE_ENGLISH: AtomicBool = AtomicBool::new(false);
static CAPTURE_COORDINATOR: OnceLock<scrypuppy_core::coordination::CaptureCoordinator> =
    OnceLock::new();
#[cfg(windows)]
thread_local! {
    static TRAY_ICON: std::cell::RefCell<Option<tray_icon::TrayIcon>> = const { std::cell::RefCell::new(None) };
}
const PAGE_SIZE: usize = 60;
const AI_KEY_CREDENTIAL: &str = "ai-api-key-v1";
const CREDENTIAL_SERVICE: &str = "Scryppy";

fn localized_ui_text(value: &str, english: bool) -> String {
    if !english {
        return value.to_owned();
    }

    for (source, target) in [
        ("Provedor: ", "Provider: "),
        ("Modelo: ", "Model: "),
        ("Apagar capturas após: ", "Delete captures after: "),
        (
            "Escopo limitado ao Contexto: ",
            "Scope limited to context: ",
        ),
        ("Origem: ", "Source: "),
        ("Janela: ", "Window: "),
        ("Tags: ", "Tags: "),
        ("Contextos: ", "Contexts: "),
        ("Texto OCR: ", "OCR text: "),
        ("Prévia · ", "Preview · "),
    ] {
        if let Some(detail) = value.strip_prefix(source) {
            return format!("{target}{detail}");
        }
    }
    if let Some(rest) = value.strip_prefix("Indexando capturas: ") {
        return format!("Indexing captures: {}", rest.replace(" de ", " of "));
    }
    if let Some(rest) = value.strip_prefix("Busca local pronta · ") {
        return format!(
            "Local search ready · {}",
            rest.replace(" capturas indexadas", " captures indexed")
        );
    }
    if let Some(rest) = value.strip_prefix("Página ") {
        return format!("Page {}", rest.replace(" de ", " of "));
    }
    if let Some(rest) = value.strip_prefix("Confiança: ") {
        return format!("Confidence: {}", rest.replace(" evidência(s)", " evidence"));
    }
    if let Some(count) = value.strip_suffix(" fonte(s) vinculada(s)") {
        if count.chars().all(|character| character.is_ascii_digit()) {
            return format!("{count} linked sources");
        }
    }
    if let Some(rest) = value.strip_prefix("Suas capturas · ") {
        return format!("Your captures · {}", rest.replace(" itens · ", " items · "));
    }
    if let Some(rest) =
        value.strip_prefix("Selecione uma ou mais capturas para adicionar a um Contexto · ")
    {
        return format!(
            "Select one or more captures to add to a context · {}",
            rest.replace(" selecionada(s)", " selected")
        );
    }
    if let Some(rest) = value.strip_prefix("Adicionar ") {
        if let Some((count, context)) = rest.split_once(" captura(s) a ") {
            if count.chars().all(|character| character.is_ascii_digit()) {
                return format!("Add {count} capture(s) to {context}");
            }
        }
    }
    if let Some(rest) = value.strip_prefix("Salvar e excluir ") {
        return format!(
            "Save and delete {}",
            rest.replace(" capturas antigas agora", " old captures now")
        );
    }
    if let Some(rest) = value.strip_suffix(" capturas seriam elegíveis agora.") {
        return format!("{rest} captures would be eligible now.");
    }
    if let Some(rest) = value.strip_prefix("A regra corresponderá a ") {
        return format!(
            "This rule will match {}",
            rest.replace(
                " capturas ainda não vinculadas a este Contexto.",
                " captures not yet assigned to this context."
            )
        );
    }
    match value {
        "Seu espaço de ideias" => "Your space for ideas".into(),
        "ScryPuppy" => "ScryPuppy".into(),
        "Voltar à biblioteca" => "Back to library".into(),
        "Configurações" => "Settings".into(),
        "Biblioteca" => "Library".into(),
        "Biblioteca local · selecione uma captura para ver os detalhes" => {
            "Local library · select a capture to see its details".into()
        }
        "Preferências da biblioteca e das capturas" => "Library and capture preferences".into(),
        "Todas as capturas" => "All captures".into(),
        "CONTEXTOS" => "CONTEXTS".into(),
        "Novo contexto" => "New context".into(),
        "Criar Contexto" => "Create context".into(),
        "Nome do Contexto" => "Context name".into(),
        "Salvar" => "Save".into(),
        "Cancelar" => "Cancel".into(),
        "Renomear Contexto" => "Rename context".into(),
        "As capturas permanecem na biblioteca sem este vínculo." => {
            "Captures stay in the library when this link is removed.".into()
        }
        "Confirmar exclusão" => "Confirm deletion".into(),
        "Excluir Contexto" => "Delete context".into(),
        "REGRAS AUTOMÁTICAS" => "AUTOMATIC RULES".into(),
        "Nome da regra" => "Rule name".into(),
        "Tipo de conteúdo" => "Content type".into(),
        "Título da janela" => "Window title".into(),
        "Extensão do arquivo" => "File extension".into(),
        "Caminho do arquivo" => "File path".into(),
        "Texto" => "Text".into(),
        "contém" => "contains".into(),
        "é igual a" => "equals".into(),
        "corresponde à expressão regular" => "matches regular expression".into(),
        "+ condição" => "+ condition".into(),
        "− condição" => "− condition".into(),
        "Combinar: todas (E)" => "Match: all (AND)".into(),
        "Combinar: qualquer (OU)" => "Match: any (OR)".into(),
        "Calculando prévia…" => "Calculating preview…".into(),
        "Prévia das capturas" => "Preview captures".into(),
        "Salvando regra…" => "Saving rule…".into(),
        "Salvar regra" => "Save rule".into(),
        "Prévia indisponível" => "Preview unavailable".into(),
        "Tipo: Todos" => "Type: All".into(),
        "Tipo: Texto" => "Type: Text".into(),
        "Tipo: Imagem" => "Type: Image".into(),
        "Tipo: Link" => "Type: Link".into(),
        "Tipo: Arquivos" => "Type: Files".into(),
        "Cada condição tem seu próprio campo e operador; novas capturas também seguem a regra." => {
            "Each condition has its own field and operator; new captures also follow the rule."
                .into()
        }
        "Buscar capturas" => "Search captures".into(),
        "Buscar" => "Search".into(),
        "Aguarde…" => "Please wait…".into(),
        "Capturar clipboard" => "Capture clipboard".into(),
        "Capturas manuais: salvar imagens" => "Manual captures: save images".into(),
        "Capturas manuais: ignorar imagens" => "Manual captures: skip images".into(),
        "Captura automática: salvar imagens" => "Automatic capture: save images".into(),
        "Captura automática: ignorar imagens" => "Automatic capture: skip images".into(),
        "Quick Context após captura manual: ligado" => "Quick Context after manual capture: on".into(),
        "Quick Context após captura manual: desligado" => "Quick Context after manual capture: off".into(),
        "Quick Context após referência: ligado" => "Quick Context after reference: on".into(),
        "Quick Context após referência: desligado" => "Quick Context after reference: off".into(),
        "Quick Context após captura automática: ligado" => "Quick Context after automatic capture: on".into(),
        "Quick Context após captura automática: desligado" => "Quick Context after automatic capture: off".into(),
        "Prévia web: ligada" => "Web previews: on".into(),
        "Prévia web: desligada" => "Web previews: off".into(),
        "Tentar novamente / reindexar" => "Retry / reindex".into(),
        "Baixar modelo e preparar busca local" => "Download model and prepare local search".into(),
        "Iniciar com o Windows: ligado" => "Start with Windows: on".into(),
        "Iniciar com o Windows: desligado" => "Start with Windows: off".into(),
        "Buscando…" => "Searching…".into(),
        "Perguntar" => "Ask".into(),
        "Salvando documento…" => "Saving document…".into(),
        "Importar arquivos" => "Import files".into(),
        "Auto: ligada" => "Auto: on".into(),
        "Auto: desligada" => "Auto: off".into(),
        "Tema claro" => "Light theme".into(),
        "Tema escuro" => "Dark theme".into(),
        "Bem-vindo ao ScryPuppy" => "Welcome to ScryPuppy".into(),
        "Suas capturas ficam na biblioteca local criptografada. Use Ctrl + Shift + C para salvar uma seleção, Ctrl + Shift + S para criar uma referência, Ctrl + Shift + F para perguntar à biblioteca e Ctrl + Shift + V para abrir o Quick Paste." => {
            "Your captures stay in the encrypted local library. Use Ctrl + Shift + C to save a selection, Ctrl + Shift + S to create a reference, Ctrl + Shift + F to ask your library, and Ctrl + Shift + V to open Quick Paste.".into()
        }
        "A captura automática começa desligada. Você pode controlar imagens, Quick Context e prévias de links em Configurações." => {
            "Automatic capture starts off. You can control images, Quick Context, and link previews in Settings.".into()
        }
        "Continuar para a biblioteca" => "Continue to library".into(),
        "Aplicativo" => "Application".into(),
        "Tag exata" => "Exact tag".into(),
        "De · AAAA-MM-DD" => "From · YYYY-MM-DD".into(),
        "Até · AAAA-MM-DD" => "To · YYYY-MM-DD".into(),
        "Limpar filtros" => "Clear filters".into(),
        "Pergunte à biblioteca" => "Ask your library".into(),
        "Pergunte sobre suas capturas" => "Ask about your captures".into(),
        "App (opcional)" => "App (optional)".into(),
        "Consultando capturas locais…" => "Searching local captures…".into(),
        "Documentos salvos" => "Saved documents".into(),
        "Salvar resposta como documento" => "Save answer as document".into(),
        "Gerar documento com IA" => "Generate document with AI".into(),
        "Salvar resposta atual como nova versão" => "Save current answer as a new version".into(),
        "Exportar Markdown" => "Export Markdown".into(),
        "Excluir documento e versões" => "Delete document and versions".into(),
        "Prévia da primeira página do PDF indisponível." => "PDF first-page preview unavailable.".into(),
        "Prévia visual do documento Word indisponível." => "Word document visual preview unavailable.".into(),
        "Prévia de texto do documento Word indisponível." => "Word document text preview unavailable.".into(),
        "Prévia do arquivo de texto indisponível." => "Text file preview unavailable.".into(),
        "Prévia do site indisponível; o link continua identificado." => "Website preview unavailable; the link is still identified.".into(),
        "Prévia indisponível para este tipo de captura." => "Preview unavailable for this capture type.".into(),
        "Confirmar exclusão das versões" => "Confirm deletion of versions".into(),
        "Reabrir tutorial de introdução" => "Reopen introduction".into(),
        "Captura e privacidade" => "Capture and privacy".into(),
        "Controla se imagens copiadas são mantidas como capturas locais." => {
            "Controls whether copied images are kept as local captures.".into()
        }
        "Capturas novas são salvas localmente; imagens seguem a preferência de captura de telas." => {
            "New captures are saved locally; images follow the screenshot capture preference.".into()
        }
        "A captura automática continua desativada até ser ligada na barra da biblioteca." => {
            "Automatic capture stays off until enabled from the library toolbar.".into()
        }
        "Quando ligada, a busca acessa sites públicos para obter título, favicon e imagem dos links salvos. Endereços locais e privados são bloqueados; a busca pode ser desligada, mantendo as prévias já salvas em cache local." => {
            "When enabled, ScryPuppy visits public websites to fetch titles, favicons, and link images. Local and private addresses are blocked. You can turn off fetching while keeping previews already saved in the local cache.".into()
        }
        "Busca inteligente local" => "Local smart search".into(),
        "O modelo multilíngue é baixado sob demanda. As capturas e seus vetores permanecem na biblioteca local criptografada; a indexação não envia capturas a serviços externos." => {
            "The multilingual model is downloaded on demand. Captures and their vectors stay in the encrypted local library; indexing does not send captures to external services.".into()
        }
        "Inteligência artificial" => "Artificial intelligence".into(),
        "Idioma da interface e das respostas: Português" => {
            "Interface and assistant language: Portuguese".into()
        }
        "Idioma da interface e das respostas: English" => {
            "Interface and assistant language: English".into()
        }
        "Modelo local não baixado" => "Local model not downloaded".into(),
        "Remover modelo de busca baixado" => "Remove downloaded search model".into(),
        "Busca local encontrou um erro" => "Local search encountered an error".into(),
        "Removendo o modelo local…" => "Removing local model…".into(),
        "Contexto selecionado" => "Selected context".into(),
        "Aplicativo desconhecido" => "Unknown application".into(),
        "Inbox" => "Inbox".into(),
        "Quick Context · demo" => "Quick Context · demo".into(),
        "Arquivo" => "File".into(),
        "Controla o idioma da interface, das respostas e dos documentos gerados." => {
            "Sets the language used by the interface, answers, and generated documents.".into()
        }
        "Chave de API" => "API key".into(),
        "Salvar chave" => "Save key".into(),
        "Remover chave" => "Remove key".into(),
        "A chave fica fora do banco local. Perguntas e fontes podem ser salvas como documentos Markdown na biblioteca criptografada." => {
            "The key is stored outside the local database. Questions and sources can be saved as Markdown documents in the encrypted library.".into()
        }
        "Adiciona ou remove ScryPuppy da inicialização da sua conta do Windows." => {
            "Add or remove ScryPuppy from your Windows account startup.".into()
        }
        "Retenção da biblioteca" => "Library retention".into(),
        "A prévia exclui referências e arquivos importados. A limpeza automática é executada no máximo uma vez por dia." => {
            "The preview excludes references and imported files. Automatic cleanup runs at most once a day.".into()
        }
        "Salvar e manter as capturas existentes" => "Save and keep existing captures".into(),
        "Calculando prévia de retenção…" => "Calculating retention preview…".into(),
        "Apagar capturas, anexos, documentos e credencial de IA? Esta ação não pode ser desfeita. A chave criptográfica da biblioteca será mantida." => {
            "Delete captures, attachments, documents, and the AI credential? This cannot be undone. The library encryption key will be kept.".into()
        }
        "Apagar todos os dados" => "Delete all data".into(),
        "Limpar todos os dados" => "Clear all data".into(),
        "Carregando biblioteca…" => "Loading library…".into(),
        "Baixando o modelo local…" => "Downloading local model…".into(),
        "Indexando capturas: {} de {}" => "Indexing captures: {} of {}".into(),
        "Nenhuma captura encontrada." => "No captures found.".into(),
        "Anterior" => "Previous".into(),
        "Próxima" => "Next".into(),
        "Detalhes da captura" => "Capture details".into(),
        "Adicionar tag" => "Add tag".into(),
        "Adicionar" => "Add".into(),
        "Abrir arquivo" => "Open file".into(),
        "Salvando Contexto…" => "Saving context…".into(),
        "Excluir esta captura e seus arquivos locais? Essa ação não pode ser desfeita." => {
            "Delete this capture and its local files? This cannot be undone.".into()
        }
        "Excluir captura" => "Delete capture".into(),
        "Buscar no histórico" => "Search history".into(),
        "Quick Paste" => "Quick Paste".into(),
        "Minimalista" => "Minimal".into(),
        "Fechar" => "Close".into(),
        "Ctrl + Shift + V para abrir · escolha uma captura para colar" => {
            "Press Ctrl + Shift + V · choose a capture to paste".into()
        }
        "Carregando histórico…" => "Loading history…".into(),
        "Organizar capturas" => "Organize captures".into(),
        "Marque ao menos uma captura para continuar." => "Select at least one capture to continue.".into(),
        "As capturas selecionadas já estão nos Contextos disponíveis." => {
            "The selected captures are already in the available contexts.".into()
        }
        "Carregando a captura salva…" => "Loading saved capture…".into(),
        "Salvando…" => "Saving…".into(),
        "Concluir depois" => "Finish later".into(),
        "‹" | "›" | "×" => value.to_owned(),
        _ => value.to_owned(),
    }
}

fn text<'a>(value: impl ToString) -> iced::widget::Text<'a> {
    iced_text(localized_ui_text(
        &value.to_string(),
        UI_LANGUAGE_ENGLISH.load(Ordering::Relaxed),
    ))
}

fn text_input<'a, InputMessage: Clone>(
    placeholder: &str,
    value: &'a str,
) -> iced::widget::TextInput<'a, InputMessage> {
    iced_text_input(
        &localized_ui_text(placeholder, UI_LANGUAGE_ENGLISH.load(Ordering::Relaxed)),
        value,
    )
}

type ImagePreview = (String, Vec<u8>);
type TextPreview = (String, String);

fn main() -> iced::Result {
    #[cfg(windows)]
    let debug_demo = cfg!(debug_assertions) && std::env::args_os().any(|arg| arg == "--demo");
    #[cfg(windows)]
    let _single_instance_guard = match acquire_single_instance() {
        Ok(Some(guard)) => Some(guard),
        Ok(None) => return Ok(()),
        Err(error) => {
            eprintln!("Não foi possível verificar outra instância: {error}");
            None
        }
    };
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
                ("ctrl+shift+KeyS", "referência", &REFERENCE_HOTKEY_ID),
                ("ctrl+shift+KeyF", "Magic Search", &MAGIC_SEARCH_HOTKEY_ID),
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
        .subscription(|app: &App| {
            let mut subscriptions = vec![
                window::close_events().map(Message::Closed),
                window::close_requests().map(Message::CloseRequested),
                window::resize_events().map(|(id, size)| Message::WindowResized(id, size)),
                iced::event::listen_with(|event, _status, id| match event {
                    iced::Event::Keyboard(iced::keyboard::Event::KeyPressed {
                        key: iced::keyboard::Key::Named(iced::keyboard::key::Named::Tab),
                        modifiers,
                        ..
                    }) => Some(Message::FocusTab(id, modifiers.shift())),
                    iced::Event::Keyboard(iced::keyboard::Event::KeyPressed {
                        key: iced::keyboard::Key::Named(iced::keyboard::key::Named::Escape),
                        ..
                    }) => Some(Message::QuickPasteEscape(id)),
                    iced::Event::Keyboard(iced::keyboard::Event::KeyPressed {
                        key: iced::keyboard::Key::Character(value),
                        modifiers,
                        ..
                    }) if modifiers.control()
                        && modifiers.shift()
                        && value.eq_ignore_ascii_case("m") =>
                    {
                        Some(Message::QuickPasteToggleStyle(id))
                    }
                    iced::Event::Keyboard(iced::keyboard::Event::KeyPressed {
                        key: iced::keyboard::Key::Named(iced::keyboard::key::Named::PageDown),
                        modifiers,
                        ..
                    }) if modifiers.control() => Some(Message::QuickPastePage(id, true)),
                    iced::Event::Keyboard(iced::keyboard::Event::KeyPressed {
                        key: iced::keyboard::Key::Named(iced::keyboard::key::Named::PageUp),
                        modifiers,
                        ..
                    }) if modifiers.control() => Some(Message::QuickPastePage(id, false)),
                    _ => None,
                }),
                iced::Subscription::run(hotkey_events),
                iced::Subscription::run(tray_events),
                iced::Subscription::run(clipboard_events),
            ];
            if app.local_search.operation_running() {
                subscriptions.push(iced::Subscription::run(local_search_ticks));
            }
            iced::Subscription::batch(subscriptions)
        })
        .run();
    clipboard_monitor_handle.shutdown();
    drop(hotkey_manager);
    result
}

#[cfg(windows)]
struct SingleInstanceGuard(std::ptr::NonNull<std::ffi::c_void>);

#[cfg(windows)]
impl Drop for SingleInstanceGuard {
    fn drop(&mut self) {
        unsafe {
            close_kernel_handle(self.0.as_ptr());
        }
    }
}

#[cfg(windows)]
#[link(name = "kernel32")]
unsafe extern "system" {
    #[link_name = "CreateMutexW"]
    fn create_named_mutex(
        attributes: *mut std::ffi::c_void,
        initial_owner: i32,
        name: *const u16,
    ) -> *mut std::ffi::c_void;
    #[link_name = "GetLastError"]
    fn get_last_error() -> u32;
    #[link_name = "CloseHandle"]
    fn close_kernel_handle(handle: *mut std::ffi::c_void) -> i32;
}

#[cfg(windows)]
#[link(name = "user32")]
unsafe extern "system" {
    #[link_name = "FindWindowW"]
    fn find_window_w(class_name: *const u16, window_name: *const u16) -> *mut std::ffi::c_void;
    #[link_name = "ShowWindow"]
    fn show_native_window(window: *mut std::ffi::c_void, command: i32) -> i32;
    #[link_name = "SetForegroundWindow"]
    fn set_foreground_window(window: *mut std::ffi::c_void) -> i32;
}

#[cfg(windows)]
fn acquire_single_instance() -> Result<Option<SingleInstanceGuard>, String> {
    use std::{os::windows::ffi::OsStrExt, ptr::NonNull};

    const ERROR_ALREADY_EXISTS: u32 = 183;
    let name = std::ffi::OsStr::new("Local\\ScryPuppy-Iced-SingleInstance-v1")
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let handle = unsafe { create_named_mutex(std::ptr::null_mut(), 1, name.as_ptr()) };
    let Some(handle) = NonNull::new(handle) else {
        return Err("Windows não criou o mutex de instância única.".into());
    };

    if unsafe { get_last_error() } == ERROR_ALREADY_EXISTS {
        unsafe { close_kernel_handle(handle.as_ptr()) };
        activate_existing_instance();
        return Ok(None);
    }

    Ok(Some(SingleInstanceGuard(handle)))
}

#[cfg(windows)]
fn activate_existing_instance() {
    use std::{os::windows::ffi::OsStrExt, time::Duration};

    let title = std::ffi::OsStr::new("ScryPuppy — Iced")
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    for _ in 0..20 {
        let window = unsafe { find_window_w(std::ptr::null(), title.as_ptr()) };
        if !window.is_null() {
            unsafe {
                show_native_window(window, 9);
                set_foreground_window(window);
            }
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn app_theme_variant(dark: bool, transparent: bool) -> Theme {
    let rgb = Color::from_rgb8;
    let (mut background, text, primary) = if dark {
        (rgb(22, 26, 35), rgb(241, 243, 247), rgb(232, 128, 130))
    } else {
        (rgb(247, 248, 250), rgb(37, 42, 54), rgb(232, 128, 130))
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
    documents: Vec<scrypuppy_core::documents::DocumentListItem>,
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
    quick_context_after_reference: bool,
    onboarding_completed: bool,
    launch_at_startup: bool,
    retention_policy: String,
    assistant_language: String,
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
    local_search: Arc<LocalSearchManager>,
    local_search_status: LocalSearchStatus,
    local_search_refresh_pending: bool,
    quick_window: Option<window::Id>,
    quick_context_window: Option<window::Id>,
    quick_context_selected: Vec<String>,
    quick_search_id: iced::widget::Id,
    assistant_query_id: iced::widget::Id,
    paste_target: Option<isize>,
    dark: bool,
    query: String,
    library_app_filter: String,
    library_date_from: String,
    library_date_to: String,
    library_tag_filter: String,
    library_kind_filter: String,
    assistant_query: String,
    assistant_app_filter: String,
    assistant_date_from: String,
    assistant_date_to: String,
    assistant_busy: bool,
    assistant_revision: u64,
    assistant_answer: Option<scrypuppy_core::assistant::ChatAnswer>,
    assistant_capture: Option<CaptureDto>,
    selected_document: Option<scrypuppy_core::documents::Document>,
    pending_delete_document: Option<String>,
    document_editor: text_editor::Content,
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
    assistant_language: String,
    assistant_language_changed: bool,
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
    clear_all_data_confirmation: bool,
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
    quick_context_after_reference: bool,
    quick_context_after_reference_changed: bool,
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
    WindowResized(window::Id, Size),
    FocusTab(window::Id, bool),
    MainOpened(window::Id),
    ShowMain,
    OpenMagicSearch,
    QuitFromTray,
    LocalSearchTick,
    PrepareLocalSearch,
    RemoveLocalSearch,
    LocalSearchFinished(Result<LocalSearchStatus, String>),
    LocalSearchRefreshFinished(Result<LocalSearchStatus, String>),
    QueryChanged(String),
    LibraryAppFilterChanged(String),
    LibraryDateFromChanged(String),
    LibraryDateToChanged(String),
    LibraryTagFilterChanged(String),
    ClearLibraryFilters,
    CycleLibraryKindFilter,
    AssistantQueryChanged(String),
    AssistantAppFilterChanged(String),
    AssistantDateFromChanged(String),
    AssistantDateToChanged(String),
    AskAssistant,
    AssistantAnswered(u64, Result<scrypuppy_core::assistant::ChatAnswer, String>),
    SaveAnswerAsDocument,
    AnswerSavedAsDocument(Result<scrypuppy_core::documents::Document, String>),
    SaveAnswerAsNewVersion,
    AnswerSavedAsNewVersion(Result<scrypuppy_core::documents::Document, String>),
    GenerateAiDocument,
    AiDocumentGenerated(Result<scrypuppy_core::documents::Document, String>),
    OpenSavedDocument(String),
    SavedDocumentLoaded(Result<scrypuppy_core::documents::Document, String>),
    RequestDeleteDocument,
    CancelDeleteDocument,
    DeleteDocument(String),
    DocumentDeleted(String, Result<(), String>),
    EditDocument(text_editor::Action),
    SaveEditedDocument,
    EditedDocumentSaved(Result<(), String>),
    ExportDocument,
    DocumentExported(Result<Option<String>, String>),
    OpenAssistantEvidence(String),
    AssistantEvidenceLoaded(u64, String, Result<Box<CaptureDto>, String>),
    Search,
    PreviousPage,
    NextPage,
    SelectContext(Option<String>),
    NewContextNameChanged(String),
    CreateContext,
    NewTagChanged(String),
    CycleAssistantLanguage,
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
    OpenCaptureFile(PathBuf),
    CaptureFileOpened(Result<(), String>),
    RequestDeleteCapture(String),
    CancelDeleteCapture,
    DeleteCapture(String),
    CaptureDeleted(Result<(), String>),
    ImportFiles,
    FilesImported(Result<usize, String>),
    RequestClearAllData,
    CancelClearAllData,
    ConfirmClearAllData,
    DataCleared(bool, Result<(), String>),
    AssignContext(String, String),
    RemoveContext(String, String),
    CaptureClipboard,
    CaptureSelection(bool),
    OpenQuickPaste(isize),
    QuickPasteOpened(window::Id),
    FinishQuickPaste,
    QuickPasteEscape(window::Id),
    QuickPasteToggleStyle(window::Id),
    QuickPastePage(window::Id, bool),
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
    ToggleQuickContextAfterReference,
    CompleteOnboarding,
    ReopenOnboarding,
    ClipboardChanged(u32),
    OcrUpdated(String),
    ScreenshotSaved(Result<(), String>),
    ClipboardCaptured(Result<Option<(String, bool)>, String>),
    ClipboardMonitorSaved(Result<(), String>),
    ToggleQuickPasteStyle,
    ToggleQuickContextStyle,
    Loaded(u64, Result<Snapshot, String>),
    Previews(u64, Vec<ImagePreview>, Vec<TextPreview>),
    LinkPreviews(u64, Vec<(String, LinkPreviewData)>),
    ContextAssigned(u64, Result<(), String>),
    ContextRemoved(u64, Result<(), String>),
    Captured(u64, Result<String, String>),
    SelectedCaptureSaved(u64, bool, Result<String, String>),
    Pasted(Result<(), String>),
    ThemeSaved(Result<(), String>),
    QuickStyleSaved(Result<(), String>),
    LinkPreviewsSaved(Result<(), String>),
    AutostartSaved(bool, Result<(), String>),
    SettingSaved(&'static str, Result<(), String>),
}

impl App {
    fn refresh_local_search(&mut self) -> Task<Message> {
        if !self.local_search.installed() {
            return Task::none();
        }
        if self.local_search.operation_running() {
            self.local_search_refresh_pending = true;
            return Task::none();
        }
        if self.local_search.status().phase != LocalSearchPhase::Ready {
            return Task::none();
        }
        if !self.local_search.try_begin_operation() {
            self.local_search_refresh_pending = true;
            return Task::none();
        }
        self.local_search_refresh_pending = false;
        self.local_search
            .set_phase(LocalSearchPhase::Indexing, None);
        self.local_search_status = self.local_search.status();
        let manager = self.local_search.clone();
        Task::perform(
            async move { refresh_local_search_data(manager) },
            Message::LocalSearchRefreshFinished,
        )
    }

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
            size: Size::new(850.0, 720.0),
            min_size: Some(Size::new(820.0, 560.0)),
            exit_on_close_request: false,
            ..window::Settings::default()
        });
        let app_dir = dirs::data_dir()
            .unwrap_or_else(std::env::temp_dir)
            .join("com.scryppy.desktop");
        let local_search = Arc::new(LocalSearchManager::new(&app_dir));
        let local_search_status = local_search.status();
        let mut app = Self {
            main,
            local_search,
            local_search_status,
            local_search_refresh_pending: false,
            quick_window: None,
            quick_context_window: None,
            quick_context_selected: Vec::new(),
            quick_search_id: iced::widget::Id::unique(),
            assistant_query_id: iced::widget::Id::unique(),
            paste_target: None,
            dark: true,
            query: String::new(),
            library_app_filter: String::new(),
            library_date_from: String::new(),
            library_date_to: String::new(),
            library_tag_filter: String::new(),
            library_kind_filter: String::new(),
            assistant_query: String::new(),
            assistant_app_filter: String::new(),
            assistant_date_from: String::new(),
            assistant_date_to: String::new(),
            assistant_busy: false,
            assistant_revision: 0,
            assistant_answer: None,
            assistant_capture: None,
            selected_document: None,
            pending_delete_document: None,
            document_editor: text_editor::Content::new(),
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
            assistant_language: "pt-BR".into(),
            assistant_language_changed: false,
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
            clear_all_data_confirmation: false,
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
            quick_context_after_reference: false,
            quick_context_after_reference_changed: false,
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
        let mut tasks = vec![open.map(Message::MainOpened), load];
        let local_search = app.local_search.clone();
        if local_search.installed() && local_search.try_begin_operation() {
            local_search.set_phase(LocalSearchPhase::Indexing, None);
            tasks.push(Task::perform(
                async move { prepare_local_search(local_search, false) },
                Message::LocalSearchFinished,
            ));
        }
        (app, Task::batch(tasks))
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
            let app_filter = self.library_app_filter.trim().to_lowercase();
            if !app_filter.is_empty() {
                snapshot.captures.retain(|capture| {
                    capture
                        .source_app_name
                        .as_deref()
                        .unwrap_or_default()
                        .to_lowercase()
                        .contains(&app_filter)
                });
            }
            let tag_filter = self.library_tag_filter.trim();
            if !tag_filter.is_empty() {
                snapshot
                    .captures
                    .retain(|capture| capture.tags.iter().any(|tag| tag == tag_filter));
            }
            if !self.library_kind_filter.is_empty() {
                snapshot.captures.retain(|capture| {
                    matches_library_content_kind(capture, &self.library_kind_filter)
                });
            }
            if let Ok(Some(date_from)) =
                normalize_assistant_date_filter(&self.library_date_from, false)
            {
                snapshot
                    .captures
                    .retain(|capture| capture.captured_at.as_str() >= date_from.as_str());
            }
            if let Ok(Some(date_to)) = normalize_assistant_date_filter(&self.library_date_to, true)
            {
                snapshot
                    .captures
                    .retain(|capture| capture.captured_at.as_str() < date_to.as_str());
            }
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
        let filter = CaptureFilter {
            context_id: self.context.clone(),
            search: (!self.query.trim().is_empty()).then(|| self.query.clone()),
            tag: (!self.library_tag_filter.trim().is_empty())
                .then(|| self.library_tag_filter.clone()),
            app: (!self.library_app_filter.trim().is_empty())
                .then(|| self.library_app_filter.clone()),
            date_from: (!self.library_date_from.trim().is_empty())
                .then(|| self.library_date_from.clone()),
            date_to: (!self.library_date_to.trim().is_empty())
                .then(|| self.library_date_to.clone()),
            content_kind: (!self.library_kind_filter.is_empty())
                .then(|| self.library_kind_filter.clone()),
            limit: Some(PAGE_SIZE),
            offset: Some(self.offset),
            exclude_references: false,
        };
        Task::perform(async move { load_default_library(filter) }, move |result| {
            Message::Loaded(revision, result)
        })
    }

    fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::CloseRequested(id) if id == self.main => hide_window(id),
            Message::CloseRequested(_) => Task::none(),
            Message::WindowResized(id, size)
                if self.quick_window == Some(id) && size.width > 0.0 && size.height > 0.0 =>
            {
                set_quick_paste_hit_region(id, self.quick_paste_minimal)
            }
            Message::WindowResized(_, _) => Task::none(),
            Message::FocusTab(_id, reverse) => {
                if reverse {
                    iced::widget::operation::focus_previous()
                } else {
                    iced::widget::operation::focus_next()
                }
            }
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
            Message::OpenMagicSearch => {
                self.settings_open = false;
                Task::batch([
                    show_window(self.main),
                    iced::widget::operation::focus(self.assistant_query_id.clone()),
                ])
            }
            Message::QuitFromTray => iced::exit(),
            Message::LocalSearchTick => {
                self.local_search_status = self.local_search.status();
                Task::none()
            }
            Message::PrepareLocalSearch => {
                if !self.local_search.try_begin_operation() {
                    self.notice = Some("A busca local já está ocupada.".into());
                    return Task::none();
                }
                let phase = if self.local_search.installed() {
                    LocalSearchPhase::Indexing
                } else {
                    LocalSearchPhase::Downloading
                };
                self.local_search.set_phase(phase, None);
                self.local_search_status = self.local_search.status();
                let manager = self.local_search.clone();
                Task::perform(
                    async move { prepare_local_search(manager, true) },
                    Message::LocalSearchFinished,
                )
            }
            Message::RemoveLocalSearch => {
                if !self.local_search.try_begin_operation() {
                    self.notice = Some("A busca local já está ocupada.".into());
                    return Task::none();
                }
                self.local_search
                    .set_phase(LocalSearchPhase::Removing, None);
                self.local_search_status = self.local_search.status();
                let manager = self.local_search.clone();
                Task::perform(
                    async move { remove_local_search(manager) },
                    Message::LocalSearchFinished,
                )
            }
            Message::LocalSearchFinished(result) => {
                self.local_search_status = self.local_search.status();
                if let Err(error) = result {
                    self.notice = Some(format!("Busca local: {error}"));
                } else {
                    self.notice = Some(match self.local_search_status.phase {
                        LocalSearchPhase::Ready => "Busca local pronta.".into(),
                        LocalSearchPhase::NotDownloaded => "Modelo de busca local removido.".into(),
                        _ => "Busca local atualizada.".into(),
                    });
                }
                if self.local_search_refresh_pending
                    && self.local_search_status.phase == LocalSearchPhase::Ready
                {
                    return self.refresh_local_search();
                }
                Task::none()
            }
            Message::LocalSearchRefreshFinished(result) => {
                self.local_search_status = self.local_search.status();
                if let Err(error) = result {
                    self.notice = Some(format!("Atualização da busca local: {error}"));
                }
                if self.local_search_refresh_pending
                    && self.local_search_status.phase == LocalSearchPhase::Ready
                {
                    return self.refresh_local_search();
                }
                Task::none()
            }
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
                self.library_app_filter.clear();
                self.library_date_from.clear();
                self.library_date_to.clear();
                self.library_tag_filter.clear();
                self.library_kind_filter.clear();
                self.offset = 0;
                Task::batch([open.map(Message::QuickPasteOpened), self.reload()])
            }
            Message::QuickPasteOpened(id) => Task::batch([
                window::gain_focus(id),
                iced::widget::operation::focus(self.quick_search_id.clone()),
                set_quick_paste_hit_region(id, self.quick_paste_minimal),
            ]),
            Message::FinishQuickPaste => {
                if let Some(id) = self.quick_window.take() {
                    self.paste_target = None;
                    window::close(id)
                } else {
                    Task::none()
                }
            }
            Message::QuickPasteEscape(id) => {
                if self.quick_window == Some(id) {
                    Task::done(Message::FinishQuickPaste)
                } else {
                    Task::none()
                }
            }
            Message::QuickPasteToggleStyle(id) => {
                if self.quick_window == Some(id) {
                    Task::done(Message::ToggleQuickPasteStyle)
                } else {
                    Task::none()
                }
            }
            Message::QuickPastePage(id, next) => {
                if self.quick_window != Some(id) {
                    Task::none()
                } else if next {
                    Task::done(Message::NextPage)
                } else {
                    Task::done(Message::PreviousPage)
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
                let date_from =
                    match normalize_assistant_date_filter(&self.library_date_from, false) {
                        Ok(value) => value,
                        Err(error) => {
                            self.notice = Some(error);
                            return Task::none();
                        }
                    };
                let date_to = match normalize_assistant_date_filter(&self.library_date_to, true) {
                    Ok(value) => value,
                    Err(error) => {
                        self.notice = Some(error);
                        return Task::none();
                    }
                };
                if date_from
                    .as_ref()
                    .zip(date_to.as_ref())
                    .is_some_and(|(from, to)| from >= to)
                {
                    self.notice = Some("A data inicial deve ser anterior à data final.".into());
                    return Task::none();
                }
                self.notice = None;
                self.offset = 0;
                self.reload()
            }
            Message::LibraryAppFilterChanged(value) => {
                self.library_app_filter = value.chars().take(120).collect();
                Task::none()
            }
            Message::LibraryDateFromChanged(value) => {
                self.library_date_from = value.chars().take(10).collect();
                Task::none()
            }
            Message::LibraryDateToChanged(value) => {
                self.library_date_to = value.chars().take(10).collect();
                Task::none()
            }
            Message::LibraryTagFilterChanged(value) => {
                self.library_tag_filter = value.chars().take(120).collect();
                Task::none()
            }
            Message::ClearLibraryFilters => {
                self.library_app_filter.clear();
                self.library_date_from.clear();
                self.library_date_to.clear();
                self.library_tag_filter.clear();
                self.library_kind_filter.clear();
                self.offset = 0;
                self.notice = None;
                self.reload()
            }
            Message::CycleLibraryKindFilter => {
                self.library_kind_filter = match self.library_kind_filter.as_str() {
                    "" => "text",
                    "text" => "image",
                    "image" => "link",
                    "link" => "files",
                    _ => "",
                }
                .into();
                self.offset = 0;
                self.reload()
            }
            Message::AssistantQueryChanged(value) => {
                self.assistant_query = value.chars().take(500).collect();
                self.assistant_revision = self.assistant_revision.wrapping_add(1);
                self.assistant_busy = false;
                self.assistant_answer = None;
                Task::none()
            }
            Message::AssistantAppFilterChanged(value) => {
                self.assistant_app_filter = value.chars().take(120).collect();
                self.assistant_revision = self.assistant_revision.wrapping_add(1);
                self.assistant_busy = false;
                self.assistant_answer = None;
                Task::none()
            }
            Message::AssistantDateFromChanged(value) => {
                self.assistant_date_from = value.chars().take(10).collect();
                self.assistant_revision = self.assistant_revision.wrapping_add(1);
                self.assistant_busy = false;
                self.assistant_answer = None;
                Task::none()
            }
            Message::AssistantDateToChanged(value) => {
                self.assistant_date_to = value.chars().take(10).collect();
                self.assistant_revision = self.assistant_revision.wrapping_add(1);
                self.assistant_busy = false;
                self.assistant_answer = None;
                Task::none()
            }
            Message::AskAssistant => {
                let query = self.assistant_query.trim().to_string();
                if query.is_empty() {
                    self.notice = Some("Digite uma pergunta para buscar nas capturas.".into());
                    return Task::none();
                }
                let app_filter = self.assistant_app_filter.trim().to_string();
                let date_from =
                    match normalize_assistant_date_filter(&self.assistant_date_from, false) {
                        Ok(date) => date,
                        Err(error) => {
                            self.notice = Some(error);
                            return Task::none();
                        }
                    };
                let date_to = match normalize_assistant_date_filter(&self.assistant_date_to, true) {
                    Ok(date) => date,
                    Err(error) => {
                        self.notice = Some(error);
                        return Task::none();
                    }
                };
                if date_from
                    .as_deref()
                    .zip(date_to.as_deref())
                    .is_some_and(|(from, to)| from > to)
                {
                    self.notice = Some("A data inicial deve ser anterior à data final.".into());
                    return Task::none();
                }
                #[cfg(debug_assertions)]
                if self.demo {
                    self.assistant_answer = Some(demo_assistant_answer(&query));
                    self.assistant_busy = false;
                    return Task::none();
                }
                if self.assistant_busy {
                    return Task::none();
                }
                let context_id = self.context.clone();
                let local_search = self.local_search.clone();
                self.assistant_revision = self.assistant_revision.wrapping_add(1);
                let revision = self.assistant_revision;
                self.assistant_busy = true;
                self.assistant_answer = None;
                Task::perform(
                    async move {
                        let (sender, receiver) = iced::futures::channel::oneshot::channel();
                        thread::spawn(move || {
                            let _ = sender.send(answer_library_question(
                                &query,
                                context_id,
                                app_filter,
                                date_from,
                                date_to,
                                local_search,
                            ));
                        });
                        receiver
                            .await
                            .unwrap_or_else(|_| Err("A busca foi interrompida.".into()))
                    },
                    move |result| Message::AssistantAnswered(revision, result),
                )
            }
            Message::AssistantAnswered(revision, result) => {
                if revision != self.assistant_revision {
                    return Task::none();
                }
                self.assistant_busy = false;
                match result {
                    Ok(answer) => self.assistant_answer = Some(answer),
                    Err(error) => self.notice = Some(error),
                }
                Task::none()
            }
            Message::SaveAnswerAsDocument => {
                let (Some(answer), query) = (
                    self.assistant_answer.clone(),
                    self.assistant_query.trim().to_string(),
                ) else {
                    return Task::none();
                };
                if self.is_demo_mode() {
                    self.notice =
                        Some("Documentos ficam desativados no modo de demonstração.".into());
                    return Task::none();
                }
                self.action_busy = true;
                self.notice = Some("Salvando resposta e fontes…".into());
                Task::perform(
                    async move { save_answer_as_document(&query, &answer) },
                    Message::AnswerSavedAsDocument,
                )
            }
            Message::AnswerSavedAsDocument(result) => {
                self.action_busy = false;
                match result {
                    Ok(document) => {
                        self.notice = Some(format!("Documento salvo: {}", document.title));
                        return self.reload();
                    }
                    Err(error) => self.error = Some(error),
                }
                Task::none()
            }
            Message::SaveAnswerAsNewVersion => {
                let (Some(answer), Some(document), query) = (
                    self.assistant_answer.clone(),
                    self.selected_document.clone(),
                    self.assistant_query.trim().to_string(),
                ) else {
                    return Task::none();
                };
                if self.is_demo_mode() {
                    self.notice = Some("Versionamento desativado no modo de demonstração.".into());
                    return Task::none();
                }
                self.action_busy = true;
                self.notice = Some("Salvando nova versão do documento…".into());
                Task::perform(
                    async move { save_answer_as_new_document_version(&query, &answer, &document.id) },
                    Message::AnswerSavedAsNewVersion,
                )
            }
            Message::AnswerSavedAsNewVersion(result) => {
                self.action_busy = false;
                match result {
                    Ok(document) => {
                        self.document_editor = text_editor::Content::with_text(&document.markdown);
                        self.selected_document = Some(document.clone());
                        self.notice =
                            Some(format!("Versão {} do documento salva.", document.version));
                        return self.reload();
                    }
                    Err(error) => self.error = Some(error),
                }
                Task::none()
            }
            Message::GenerateAiDocument => {
                let (Some(answer), query) = (
                    self.assistant_answer.clone(),
                    self.assistant_query.trim().to_string(),
                ) else {
                    return Task::none();
                };
                if self.is_demo_mode() {
                    self.notice = Some("Geração com IA desativada no modo de demonstração.".into());
                    return Task::none();
                }
                self.action_busy = true;
                self.notice = Some("Gerando documento a partir das fontes selecionadas…".into());
                Task::perform(
                    async move {
                        let (sender, receiver) = iced::futures::channel::oneshot::channel();
                        thread::spawn(move || {
                            let _ = sender.send(generate_ai_document_as_document(&query, &answer));
                        });
                        receiver.await.unwrap_or_else(|_| {
                            Err("A geração do documento foi interrompida.".into())
                        })
                    },
                    Message::AiDocumentGenerated,
                )
            }
            Message::AiDocumentGenerated(result) => {
                self.action_busy = false;
                match result {
                    Ok(document) => {
                        self.notice = Some(format!("Documento gerado: {}", document.title));
                        return self.reload();
                    }
                    Err(error) => self.error = Some(error),
                }
                Task::none()
            }
            Message::OpenSavedDocument(id) => Task::perform(
                async move { load_saved_document(&id) },
                Message::SavedDocumentLoaded,
            ),
            Message::SavedDocumentLoaded(result) => {
                self.pending_delete_document = None;
                match result {
                    Ok(document) => {
                        self.document_editor = text_editor::Content::with_text(&document.markdown);
                        self.selected_document = Some(document);
                    }
                    Err(error) => self.error = Some(error),
                }
                Task::none()
            }
            Message::RequestDeleteDocument => {
                self.pending_delete_document = self
                    .selected_document
                    .as_ref()
                    .map(|document| document.root_id.clone());
                Task::none()
            }
            Message::CancelDeleteDocument => {
                self.pending_delete_document = None;
                Task::none()
            }
            Message::DeleteDocument(root_id) => {
                if self.pending_delete_document.as_deref() != Some(root_id.as_str()) {
                    return Task::none();
                }
                self.action_busy = true;
                Task::perform(
                    async move { delete_saved_document_root(&root_id).map(|()| root_id.clone()) },
                    |result| match result {
                        Ok(root_id) => Message::DocumentDeleted(root_id, Ok(())),
                        Err(error) => Message::DocumentDeleted(String::new(), Err(error)),
                    },
                )
            }
            Message::DocumentDeleted(root_id, result) => {
                self.action_busy = false;
                self.pending_delete_document = None;
                match result {
                    Ok(()) => {
                        if self
                            .selected_document
                            .as_ref()
                            .is_some_and(|document| document.root_id == root_id)
                        {
                            self.selected_document = None;
                            self.document_editor = text_editor::Content::new();
                        }
                        self.notice = Some("Documento e versões excluídos.".into());
                        return self.reload();
                    }
                    Err(error) => self.error = Some(error),
                }
                Task::none()
            }
            Message::EditDocument(action) => {
                self.document_editor.perform(action);
                Task::none()
            }
            Message::SaveEditedDocument => {
                let Some(document) = &self.selected_document else {
                    return Task::none();
                };
                if self.is_demo_mode() {
                    self.notice = Some("Edição desativada no modo de demonstração.".into());
                    return Task::none();
                }
                self.action_busy = true;
                let id = document.id.clone();
                let markdown = self.document_editor.text();
                Task::perform(
                    async move { save_document_markdown(&id, &markdown) },
                    Message::EditedDocumentSaved,
                )
            }
            Message::EditedDocumentSaved(result) => {
                self.action_busy = false;
                match result {
                    Ok(()) => {
                        if let Some(document) = &mut self.selected_document {
                            document.markdown = self.document_editor.text();
                        }
                        self.notice = Some("Documento atualizado.".into());
                    }
                    Err(error) => self.error = Some(error),
                }
                Task::none()
            }
            Message::ExportDocument => {
                let Some(document) = self.selected_document.clone() else {
                    return Task::none();
                };
                self.action_busy = true;
                Task::perform(
                    async move {
                        let suggested_name = document_export_filename(&document);
                        let dialog = rfd::AsyncFileDialog::new()
                            .add_filter("Markdown", &["md"])
                            .set_file_name(&suggested_name);
                        let Some(handle) = dialog.save_file().await else {
                            return Ok(None);
                        };
                        export_document_markdown(&document, handle.path()).map(Some)
                    },
                    Message::DocumentExported,
                )
            }
            Message::DocumentExported(result) => {
                self.action_busy = false;
                match result {
                    Ok(Some(path)) => self.notice = Some(format!("Markdown exportado em {path}")),
                    Ok(None) => {}
                    Err(error) => self.error = Some(error),
                }
                Task::none()
            }
            Message::OpenAssistantEvidence(capture_id) => {
                self.assistant_revision = self.assistant_revision.wrapping_add(1);
                let revision = self.assistant_revision;
                self.selected_capture = Some(capture_id.clone());
                self.assistant_capture = None;
                let result_capture_id = capture_id.clone();
                Task::perform(
                    async move { load_capture_by_id(&capture_id).map(Box::new) },
                    move |result| {
                        Message::AssistantEvidenceLoaded(revision, result_capture_id, result)
                    },
                )
            }
            Message::AssistantEvidenceLoaded(revision, capture_id, result) => {
                if revision == self.assistant_revision
                    && self.selected_capture.as_deref() == Some(capture_id.as_str())
                {
                    match result {
                        Ok(capture) => self.assistant_capture = Some(*capture),
                        Err(error) => self.notice = Some(error),
                    }
                }
                Task::none()
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
                    self.assistant_revision = self.assistant_revision.wrapping_add(1);
                    self.assistant_busy = false;
                    self.assistant_answer = None;
                    self.assistant_capture = None;
                    self.assistant_capture = None;
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
                self.assistant_capture = None;
                self.assistant_answer = None;
                let refresh = match result {
                    Ok(()) => {
                        self.new_tag.clear();
                        self.refresh_local_search()
                    }
                    Err(error) => {
                        self.notice = Some(error);
                        Task::none()
                    }
                };
                Task::batch([self.reload(), refresh])
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
            Message::CycleAssistantLanguage => {
                #[cfg(debug_assertions)]
                if self.demo {
                    self.assistant_language = if self.assistant_language == "pt-BR" {
                        "en"
                    } else {
                        "pt-BR"
                    }
                    .into();
                    return Task::none();
                }
                self.assistant_language = if self.assistant_language == "pt-BR" {
                    "en"
                } else {
                    "pt-BR"
                }
                .into();
                self.assistant_language_changed = true;
                let value = self.assistant_language.clone();
                Task::perform(
                    async move { save_setting_value("language", &value) },
                    |result| Message::SettingSaved("language", result),
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
                let refresh = result.is_ok().then(|| self.refresh_local_search());
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
                let mut tasks = vec![self.reload()];
                if let Some(refresh) = refresh {
                    tasks.push(refresh);
                }
                Task::batch(tasks)
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
                let refresh = result.is_ok().then(|| self.refresh_local_search());
                if let Err(error) = result {
                    self.notice = Some(error);
                }
                let mut tasks = vec![self.reload()];
                if let Some(refresh) = refresh {
                    tasks.push(refresh);
                }
                Task::batch(tasks)
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
                let refresh = result.is_ok().then(|| self.refresh_local_search());
                match result {
                    Ok(id) => {
                        self.context = Some(id);
                        self.notice = Some("Contexto renomeado.".into());
                    }
                    Err(error) => self.notice = Some(error),
                }
                let mut tasks = vec![self.reload()];
                if let Some(refresh) = refresh {
                    tasks.push(refresh);
                }
                Task::batch(tasks)
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
                let refresh = result.is_ok().then(|| self.refresh_local_search());
                match result {
                    Ok(()) => {
                        self.context = None;
                        self.offset = 0;
                        self.notice =
                            Some("Contexto excluído; as capturas permanecem na biblioteca.".into());
                    }
                    Err(error) => self.notice = Some(error),
                }
                let mut tasks = vec![self.reload()];
                if let Some(refresh) = refresh {
                    tasks.push(refresh);
                }
                Task::batch(tasks)
            }
            Message::SelectCapture(id) => {
                self.selected_capture = Some(id);
                self.assistant_capture = None;
                Task::none()
            }
            Message::OpenCaptureFile(path) => Task::perform(
                async move {
                    let (sender, receiver) = iced::futures::channel::oneshot::channel();
                    thread::spawn(move || {
                        let _ = sender.send(open_capture_file(&path));
                    });
                    receiver
                        .await
                        .unwrap_or_else(|_| Err("A abertura do arquivo foi interrompida.".into()))
                },
                Message::CaptureFileOpened,
            ),
            Message::CaptureFileOpened(result) => {
                match result {
                    Ok(()) => self.notice = Some("Arquivo aberto no aplicativo padrão.".into()),
                    Err(error) => self.error = Some(error),
                }
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
                        self.assistant_capture = None;
                        self.assistant_answer = None;
                        self.notice = Some("Captura excluída.".into());
                    }
                    Err(error) => self.notice = Some(error),
                }
                self.reload()
            }
            Message::ImportFiles => {
                if self.action_busy || self.is_demo_mode() {
                    if self.is_demo_mode() {
                        self.notice = Some("Importação desativada no modo de demonstração.".into());
                    }
                    return Task::none();
                }
                self.action_busy = true;
                self.notice = None;
                Task::perform(
                    async {
                        let Some(files) = rfd::AsyncFileDialog::new().pick_files().await else {
                            return Ok(0);
                        };
                        import_file_paths(
                            files.iter().map(|file| file.path().to_path_buf()).collect(),
                        )
                    },
                    Message::FilesImported,
                )
            }
            Message::FilesImported(result) => {
                self.action_busy = false;
                match result {
                    Ok(0) => Task::none(),
                    Ok(count) => {
                        self.notice = Some(format!("{count} arquivo(s) importado(s)."));
                        self.query.clear();
                        self.context = None;
                        self.offset = 0;
                        let refresh = self.refresh_local_search();
                        Task::batch([self.reload(), refresh])
                    }
                    Err(error) => {
                        self.notice = Some(error);
                        let refresh = self.refresh_local_search();
                        Task::batch([self.reload(), refresh])
                    }
                }
            }
            Message::RequestClearAllData => {
                self.clear_all_data_confirmation = true;
                Task::none()
            }
            Message::CancelClearAllData => {
                self.clear_all_data_confirmation = false;
                Task::none()
            }
            Message::ConfirmClearAllData => {
                #[cfg(debug_assertions)]
                if self.demo {
                    self.clear_all_data_confirmation = false;
                    self.notice = Some("Limpeza desativada no modo de demonstração.".into());
                    return Task::none();
                }
                if self.action_busy || self.assistant_busy || self.loading {
                    self.notice =
                        Some("Aguarde as operações atuais antes de limpar os dados.".into());
                    return Task::none();
                }
                let monitor_was_enabled = self.clipboard_monitor_enabled;
                DATA_RESET_IN_PROGRESS.store(true, Ordering::Release);
                CLIPBOARD_MONITOR_ENABLED.store(false, Ordering::Release);
                self.clear_all_data_confirmation = false;
                self.action_busy = true;
                let manager = self.local_search.clone();
                Task::perform(
                    async move { delete_all_library_data(manager) },
                    move |result| Message::DataCleared(monitor_was_enabled, result),
                )
            }
            Message::DataCleared(monitor_was_enabled, result) => {
                DATA_RESET_IN_PROGRESS.store(false, Ordering::Release);
                self.action_busy = false;
                match result {
                    Ok(()) => {
                        self.clipboard_monitor_enabled = false;
                        self.clipboard_monitor_changed = false;
                        CLIPBOARD_MONITOR_ENABLED.store(false, Ordering::Release);
                        self.theme_changed = false;
                        self.quick_paste_style_changed = false;
                        self.quick_context_style_changed = false;
                        self.link_preview_fetch_changed = false;
                        self.capture_screenshots_changed = false;
                        self.clipboard_monitor_capture_screenshots_changed = false;
                        self.clipboard_monitor_quick_context_changed = false;
                        self.quick_context_enabled_changed = false;
                        self.quick_context_after_reference_changed = false;
                        self.quick_context_after_reference = false;
                        self.onboarding_changed = false;
                        self.autostart_changed = false;
                        self.retention_changed = false;
                        self.ai_provider_changed = false;
                        self.ai_model_changed = false;
                        self.assistant_language_changed = false;
                        self.local_search_refresh_pending = false;
                        self.launch_at_startup = false;
                        self.local_search_status = self.local_search.status();
                        AI_API_KEY_STATUS.store(1, Ordering::Release);
                        self.ai_api_key_configured = false;
                        self.ai_api_key_input.clear();
                        self.context = None;
                        self.offset = 0;
                        self.selected_capture = None;
                        self.assistant_capture = None;
                        self.assistant_answer = None;
                        self.quick_context_selected.clear();
                        self.snapshot = None;
                        self.notice = Some(
                            "Dados locais apagados. A biblioteca criptografada foi recriada."
                                .into(),
                        );
                        let mut tasks = vec![self.reload()];
                        if let Some(id) = self.quick_window.take() {
                            tasks.push(window::close(id));
                        }
                        if let Some(id) = self.quick_context_window.take() {
                            tasks.push(window::close(id));
                        }
                        Task::batch(tasks)
                    }
                    Err(error) => {
                        self.clipboard_monitor_enabled = monitor_was_enabled;
                        CLIPBOARD_MONITOR_ENABLED.store(monitor_was_enabled, Ordering::Release);
                        self.notice =
                            Some(format!("Não foi possível limpar os dados locais: {error}"));
                        Task::none()
                    }
                }
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
                if DATA_RESET_IN_PROGRESS.load(Ordering::Acquire) {
                    return Task::none();
                }
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
            Message::CaptureSelection(is_reference) => {
                if DATA_RESET_IN_PROGRESS.load(Ordering::Acquire) {
                    return Task::none();
                }
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
                Task::perform(
                    async move {
                        capture_selected_content(if is_reference { "reference" } else { "capture" })
                    },
                    move |result| Message::SelectedCaptureSaved(revision, is_reference, result),
                )
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
            Message::ReopenOnboarding => {
                self.onboarding_completed = false;
                self.onboarding_changed = true;
                self.settings_open = false;
                persist_boolean_setting("onboarding_completed", false, self.is_demo_mode())
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
            Message::ToggleQuickContextAfterReference => {
                self.quick_context_after_reference = !self.quick_context_after_reference;
                self.quick_context_after_reference_changed = true;
                persist_boolean_setting(
                    "quick_context_after_reference",
                    self.quick_context_after_reference,
                    self.is_demo_mode(),
                )
            }
            Message::ClipboardChanged(sequence) => {
                if DATA_RESET_IN_PROGRESS.load(Ordering::Acquire) {
                    return Task::none();
                }
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
                let refresh = self.refresh_local_search();
                Task::batch([self.reload(), refresh])
            }
            Message::ScreenshotSaved(result) => {
                if let Err(error) = result {
                    self.notice = Some(format!("Screenshot não disponível: {error}"));
                }
                self.reload()
            }
            Message::ClipboardCaptured(result) => match result {
                Ok(Some((id, open_quick_context))) => {
                    self.notice = Some("Captura automática salva.".into());
                    let refresh = self.refresh_local_search();
                    if open_quick_context {
                        self.query.clear();
                        self.context = None;
                        self.offset = 0;
                        self.selected_capture = Some(id.clone());
                        Task::batch([Task::done(Message::OpenQuickContext(id)), refresh])
                    } else {
                        Task::batch([self.reload(), refresh])
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
                    #[cfg(windows)]
                    if let Some(id) = self.quick_window {
                        return set_quick_paste_hit_region(id, self.quick_paste_minimal);
                    }
                    return Task::none();
                }
                let value = if self.quick_paste_minimal {
                    "minimal"
                } else {
                    "normal"
                };
                let save = Task::perform(
                    async move { save_window_style_preference("iced_quick_paste_style", value) },
                    Message::QuickStyleSaved,
                );
                #[cfg(windows)]
                if let Some(id) = self.quick_window {
                    return Task::batch([
                        save,
                        set_quick_paste_hit_region(id, self.quick_paste_minimal),
                    ]);
                }
                save
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
                        if !self.quick_context_after_reference_changed {
                            self.quick_context_after_reference =
                                snapshot.quick_context_after_reference;
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
                        if !self.assistant_language_changed {
                            self.assistant_language = snapshot.assistant_language.clone();
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
                        let hit_region = self
                            .quick_window
                            .map(|id| set_quick_paste_hit_region(id, self.quick_paste_minimal))
                            .unwrap_or_else(Task::none);
                        return Task::batch([local, links, hit_region]);
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
                let refresh = result.is_ok().then(|| self.refresh_local_search());
                let close_quick_context = match result {
                    Ok(()) => {
                        self.assistant_capture = None;
                        self.assistant_answer = None;
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
                let mut tasks = vec![self.reload()];
                if let Some(refresh) = refresh {
                    tasks.push(refresh);
                }
                if let Some(id) = close_quick_context {
                    tasks.push(window::close(id));
                    Task::batch(tasks)
                } else {
                    Task::batch(tasks)
                }
            }
            Message::ContextRemoved(revision, result) => {
                self.action_busy = false;
                if revision != self.revision {
                    return Task::none();
                }
                let refresh = result.is_ok().then(|| self.refresh_local_search());
                match result {
                    Ok(()) => {
                        self.assistant_capture = None;
                        self.assistant_answer = None;
                        self.notice = Some("Captura removida do Contexto.".into());
                    }
                    Err(error) => self.notice = Some(error),
                }
                let mut tasks = vec![self.reload()];
                if let Some(refresh) = refresh {
                    tasks.push(refresh);
                }
                Task::batch(tasks)
            }
            Message::Captured(revision, result) => {
                self.action_busy = false;
                if revision != self.revision {
                    return Task::none();
                }
                match result {
                    Ok(id) => {
                        self.notice = Some("Captura salva.".into());
                        let refresh = self.refresh_local_search();
                        self.query.clear();
                        self.context = None;
                        self.offset = 0;
                        self.selected_capture = Some(id.clone());
                        if self.quick_context_enabled {
                            return Task::batch([
                                Task::done(Message::OpenQuickContext(id)),
                                refresh,
                            ]);
                        }
                        return Task::batch([self.reload(), refresh]);
                    }
                    Err(error) => self.notice = Some(error),
                }
                self.reload()
            }
            Message::SelectedCaptureSaved(revision, is_reference, result) => {
                self.action_busy = false;
                if revision != self.revision {
                    return Task::none();
                }
                match result {
                    Ok(id) => {
                        self.notice = Some(if is_reference {
                            "Referência salva na Knowledge Base.".into()
                        } else {
                            "Seleção salva.".into()
                        });
                        self.selected_capture = Some(id.clone());
                        let refresh = self.refresh_local_search();
                        if self.quick_context_enabled
                            && (!is_reference || self.quick_context_after_reference)
                        {
                            return Task::batch([
                                Task::done(Message::OpenQuickContext(id)),
                                refresh,
                            ]);
                        }
                        return Task::batch([self.reload(), refresh]);
                    }
                    Err(error) => self.notice = Some(error),
                }
                self.reload()
            }
        }
    }

    fn view(&self, id: window::Id) -> Element<'_, Message> {
        UI_LANGUAGE_ENGLISH.store(self.assistant_language == "en", Ordering::Relaxed);
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
            button(text("Todas as capturas"))
                .on_press(Message::SelectContext(None))
                .style(move |_, status| navigation_style(dark, all_selected, status))
                .width(Length::Fill),
            button(text(if self.settings_open {
                "Voltar à biblioteca"
            } else {
                "Configurações"
            }))
            .on_press(Message::ToggleSettings)
            .width(Length::Fill),
            Space::new().height(12),
            text("CONTEXTOS").size(11),
            text_input("Novo contexto", &self.new_context_name)
                .on_input(Message::NewContextNameChanged)
                .on_submit(Message::CreateContext)
                .padding(8),
            button(text("Criar Contexto"))
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
                        button(text("Quick Context · demo"))
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
                            button(text("Salvar"))
                                .on_press(Message::SaveRenameContext)
                                .width(Length::Fill),
                            button(text("Cancelar"))
                                .on_press(Message::CancelRenameContext)
                                .width(Length::Fill),
                        ]
                        .spacing(6),
                    );
                } else {
                    navigation = navigation.push(
                        button(text("Renomear Contexto"))
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
                            button(text("Confirmar exclusão"))
                                .on_press(Message::DeleteContext(context_id.to_string()))
                                .width(Length::Fill),
                            button(text("Cancelar"))
                                .on_press(Message::CancelDeleteContext)
                                .width(Length::Fill),
                        ]
                        .spacing(6),
                    );
                } else {
                    navigation = navigation.push(
                        button(text("Excluir Contexto"))
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
                            button(text("+ condição")).on_press(Message::AddRuleCondition).width(Length::Fill),
                            button(text("− condição")).on_press(Message::RemoveRuleCondition).width(Length::Fill),
                        ].spacing(5))
                        .push(button(text(if self.new_rule_match_mode == "all" { "Combinar: todas (E)" } else { "Combinar: qualquer (OU)" }))
                            .on_press(Message::ToggleRuleMatchMode))
                        .push(text("Cada condição tem seu próprio campo e operador; novas capturas também seguem a regra.").size(10))
                        .push(button(text(if self.rule_preview_loading { "Calculando prévia…" } else { "Prévia das capturas" }))
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
                        button(text(if self.action_busy {
                            "Salvando regra…"
                        } else {
                            "Salvar regra"
                        }))
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
                                        button(text("×"))
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
                .padding(10)
                .width(Length::Fixed(460.0)),
            button(text("Buscar")).on_press(Message::Search),
        ]
        .spacing(8)
        .align_y(Alignment::Center)
        .width(Length::Fill);
        let library_actions = row![
            button(text(if self.action_busy {
                "Aguarde…"
            } else {
                "Capturar clipboard"
            }))
            .on_press(Message::CaptureClipboard),
            button(text("Importar arquivos"))
                .on_press_maybe((!self.action_busy).then_some(Message::ImportFiles)),
            button(text(if self.clipboard_monitor_enabled {
                "Auto: ligada"
            } else {
                "Auto: desligada"
            }))
            .on_press(Message::ToggleClipboardMonitor),
            button(text(if self.dark {
                "Tema claro"
            } else {
                "Tema escuro"
            }))
            .on_press(Message::ToggleTheme)
            .style(move |_, status| navigation_style(dark, false, status)),
        ]
        .spacing(8)
        .align_y(Alignment::Center)
        .width(Length::Fill)
        .wrap();
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
        .spacing(18)
        .width(Length::Fill);
        if !self.onboarding_completed && !self.settings_open {
            content = content.push(
                container(
                    column![
                        text("Bem-vindo ao ScryPuppy").size(20),
                        text("Suas capturas ficam na biblioteca local criptografada. Use Ctrl + Shift + C para salvar uma seleção, Ctrl + Shift + S para criar uma referência, Ctrl + Shift + F para perguntar à biblioteca e Ctrl + Shift + V para abrir o Quick Paste.").size(13).width(Length::Fill),
                        text("A captura automática começa desligada. Você pode controlar imagens, Quick Context e prévias de links em Configurações.").size(12).width(Length::Fill),
                        button(text("Continuar para a biblioteca")).on_press(Message::CompleteOnboarding),
                    ]
                    .spacing(10)
                    .width(Length::Fill),
                )
                .width(Length::Fill)
                .padding(18)
                .style(move |_| panel_style(dark, false)),
            );
        }
        if !self.settings_open {
            content = content.push(search).push(library_actions);
            content = content.push(
                text_input("Aplicativo", &self.library_app_filter)
                    .on_input(Message::LibraryAppFilterChanged)
                    .on_submit(Message::Search)
                    .padding(8)
                    .width(Length::Fill),
            );
            content = content.push(
                row![
                    text_input("Tag exata", &self.library_tag_filter)
                        .on_input(Message::LibraryTagFilterChanged)
                        .on_submit(Message::Search)
                        .padding(8)
                        .width(140),
                    text_input("De · AAAA-MM-DD", &self.library_date_from)
                        .on_input(Message::LibraryDateFromChanged)
                        .on_submit(Message::Search)
                        .padding(8)
                        .width(160),
                    text_input("Até · AAAA-MM-DD", &self.library_date_to)
                        .on_input(Message::LibraryDateToChanged)
                        .on_submit(Message::Search)
                        .padding(8)
                        .width(160),
                ]
                .spacing(8)
                .width(Length::Fill),
            );
            content = content.push(
                row![
                    button(library_kind_filter_label(&self.library_kind_filter))
                        .on_press(Message::CycleLibraryKindFilter),
                    button(text("Limpar filtros")).on_press(Message::ClearLibraryFilters),
                ]
                .spacing(8)
                .width(Length::Fill),
            );
            content = content.push(text("Pergunte à biblioteca").size(18)).push(
                row![
                    text_input("Pergunte sobre suas capturas", &self.assistant_query)
                        .on_input(Message::AssistantQueryChanged)
                        .on_submit(Message::AskAssistant)
                        .id(self.assistant_query_id.clone())
                        .padding(10)
                        .width(Length::Fixed(460.0)),
                    button(text(if self.assistant_busy {
                        "Buscando…"
                    } else {
                        "Perguntar"
                    }))
                    .on_press_maybe(
                        (!self.assistant_busy && !self.action_busy)
                            .then_some(Message::AskAssistant),
                    ),
                ]
                .spacing(8)
                .width(Length::Fill),
            );
            content = content.push(
                row![
                    text_input("App (opcional)", &self.assistant_app_filter)
                        .on_input(Message::AssistantAppFilterChanged)
                        .padding(8),
                    text_input("De · AAAA-MM-DD", &self.assistant_date_from)
                        .on_input(Message::AssistantDateFromChanged)
                        .padding(8),
                    text_input("Até · AAAA-MM-DD", &self.assistant_date_to)
                        .on_input(Message::AssistantDateToChanged)
                        .padding(8),
                ]
                .spacing(8)
                .width(Length::Fill)
                .wrap(),
            );
            if let Some(context_id) = &self.context {
                let context_name = self
                    .snapshot
                    .as_ref()
                    .and_then(|snapshot| {
                        snapshot
                            .contexts
                            .iter()
                            .find(|context| &context.id == context_id)
                    })
                    .map(|context| context.name.as_str())
                    .unwrap_or("Contexto selecionado");
                content = content
                    .push(text(format!("Escopo limitado ao Contexto: {context_name}")).size(10));
            }
            if self.assistant_busy {
                content = content.push(text("Consultando capturas locais…").size(12));
            }
            if let Some(answer) = &self.assistant_answer {
                let mut answer_content = column![
                    text(answer.answer.as_str()).size(14),
                    text(format!(
                        "Confiança: {} · {} evidência(s)",
                        answer.confidence,
                        answer.evidence.len()
                    ))
                    .size(10),
                ]
                .spacing(8);
                answer_content = answer_content.push(
                    row![
                        button(text("Salvar resposta como documento")).on_press_maybe(
                            (!self.action_busy).then_some(Message::SaveAnswerAsDocument),
                        ),
                        button(text("Gerar documento com IA")).on_press_maybe(
                            (!self.action_busy).then_some(Message::GenerateAiDocument),
                        ),
                    ]
                    .spacing(8),
                );
                for evidence in &answer.evidence {
                    let context = if evidence.context_names.is_empty() {
                        "Inbox".to_string()
                    } else {
                        evidence.context_names.join(", ")
                    };
                    answer_content = answer_content.push(
                        button(text(format!(
                            "{} · {} · {} · {}",
                            evidence
                                .app_name
                                .as_deref()
                                .unwrap_or("Aplicativo desconhecido"),
                            context,
                            evidence.captured_at,
                            evidence.excerpt,
                        )))
                        .on_press(Message::OpenAssistantEvidence(evidence.capture_id.clone()))
                        .width(Length::Fill),
                    );
                }
                content = content.push(
                    container(scrollable(answer_content))
                        .height(180)
                        .width(Length::Fill)
                        .padding(14)
                        .style(move |_| panel_style(dark, false)),
                );
            }
            if let Some(snapshot) = &self.snapshot {
                if !snapshot.documents.is_empty() {
                    let mut saved = column![text("Documentos salvos").size(16)].spacing(6);
                    for document in snapshot.documents.iter().take(8) {
                        saved = saved.push(
                            button(text(format!(
                                "{} · {} fonte(s) · v{}",
                                document.title, document.evidence_count, document.version
                            )))
                            .on_press(Message::OpenSavedDocument(document.id.clone()))
                            .width(Length::Fill),
                        );
                    }
                    content = content.push(saved);
                }
            }
            if let Some(document) = &self.selected_document {
                let mut document_content = column![
                    text(document.title.as_str()).size(18),
                    text_editor(&self.document_editor)
                        .on_action(Message::EditDocument)
                        .height(Length::Fixed(220.0)),
                    button(text("Salvar resposta atual como nova versão")).on_press_maybe(
                        (self.assistant_answer.is_some() && !self.action_busy)
                            .then_some(Message::SaveAnswerAsNewVersion),
                    ),
                    button(text(if self.action_busy {
                        "Salvando documento…"
                    } else {
                        "Salvar edição"
                    }))
                    .on_press_maybe((!self.action_busy).then_some(Message::SaveEditedDocument),),
                    button(text("Exportar Markdown"))
                        .on_press_maybe((!self.action_busy).then_some(Message::ExportDocument)),
                    button(text("Excluir documento e versões")).on_press_maybe(
                        (!self.action_busy).then_some(Message::RequestDeleteDocument)
                    ),
                    text(format!("{} fonte(s) vinculada(s)", document.evidence.len())).size(11),
                ]
                .spacing(8);
                if self.pending_delete_document.as_deref() == Some(document.root_id.as_str()) {
                    document_content = document_content.push(
                        text(format!(
                            "Excluir {} e todas as versões? Esta ação não pode ser desfeita.",
                            document.title
                        ))
                        .size(12),
                    );
                    document_content = document_content.push(
                        row![
                            button(text("Confirmar exclusão das versões")).on_press_maybe(
                                (!self.action_busy)
                                    .then_some(Message::DeleteDocument(document.root_id.clone()))
                            ),
                            button(text("Cancelar")).on_press(Message::CancelDeleteDocument),
                        ]
                        .spacing(8),
                    );
                }
                for evidence in &document.evidence {
                    let source = format!(
                        "{} · {} · {}",
                        evidence
                            .app_name
                            .as_deref()
                            .unwrap_or("Aplicativo desconhecido"),
                        evidence.captured_at,
                        evidence.excerpt
                    );
                    document_content = document_content.push(
                        button(text(source).size(11))
                            .on_press_maybe(evidence.available.then_some(
                                Message::OpenAssistantEvidence(evidence.capture_id.clone()),
                            ))
                            .width(Length::Fill),
                    );
                }
                content = content.push(
                    container(document_content)
                        .width(Length::Fill)
                        .padding(14)
                        .style(move |_| panel_style(dark, false)),
                );
            }
        }
        if self.clipboard_monitor_enabled && !self.settings_open {
            content = content.push(
                text("Capturas novas são salvas localmente; imagens seguem a preferência de captura de telas.").size(11),
            );
        }
        if self.settings_open {
            let local_search_label = match self.local_search_status.phase {
                LocalSearchPhase::NotDownloaded => "Modelo local não baixado".to_string(),
                LocalSearchPhase::Downloading => "Baixando o modelo local…".to_string(),
                LocalSearchPhase::Indexing => format!(
                    "Indexando capturas: {} de {}",
                    self.local_search_status.indexed_count, self.local_search_status.total_count
                ),
                LocalSearchPhase::Ready => format!(
                    "Busca local pronta · {} capturas indexadas",
                    self.local_search_status.indexed_count
                ),
                LocalSearchPhase::Error => "Busca local encontrou um erro".to_string(),
                LocalSearchPhase::Removing => "Removendo o modelo local…".to_string(),
            };
            let local_search_busy = self.local_search.operation_running();
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
                .push(button(text("Reabrir tutorial de introdução")).on_press(Message::ReopenOnboarding))
                .push(text("Captura e privacidade").size(20))
                .push(
                    button(text(if self.capture_screenshots {
                        "Capturas manuais: salvar imagens"
                    } else {
                        "Capturas manuais: ignorar imagens"
                    }))
                    .on_press(Message::ToggleCaptureScreenshots),
                )
                .push(text("Controla se imagens copiadas são mantidas como capturas locais.").size(12))
                .push(
                    button(text(if self.clipboard_monitor_capture_screenshots {
                        "Captura automática: salvar imagens"
                    } else {
                        "Captura automática: ignorar imagens"
                    }))
                    .on_press(Message::ToggleMonitorScreenshots),
                )
                .push(text("A captura automática continua desativada até ser ligada na barra da biblioteca.").size(12))
                .push(
                    button(text(if self.quick_context_enabled {
                        "Quick Context após captura manual: ligado"
                    } else {
                        "Quick Context após captura manual: desligado"
                    }))
                    .on_press(Message::ToggleQuickContext),
                )
                .push(
                    button(text(if self.quick_context_after_reference {
                        "Quick Context após referência: ligado"
                    } else {
                        "Quick Context após referência: desligado"
                    }))
                    .on_press(Message::ToggleQuickContextAfterReference),
                )
                .push(
                    button(text(if self.clipboard_monitor_quick_context_enabled {
                        "Quick Context após captura automática: ligado"
                    } else {
                        "Quick Context após captura automática: desligado"
                    }))
                    .on_press(Message::ToggleMonitorQuickContext),
                )
                .push(
                    button(text(if self.link_preview_fetch_enabled {
                        "Prévia web: ligada"
                    } else {
                        "Prévia web: desligada"
                    }))
                    .on_press(Message::ToggleLinkPreviewFetch),
                )
                .push(text("Quando ligada, a busca acessa sites públicos para obter título, favicon e imagem dos links salvos. Endereços locais e privados são bloqueados; a busca pode ser desligada, mantendo as prévias já salvas em cache local.").size(12))
                .push(text("Busca inteligente local").size(20))
                .push(text(local_search_label).size(12))
                .push(text("O modelo multilíngue é baixado sob demanda. As capturas e seus vetores permanecem na biblioteca local criptografada; a indexação não envia capturas a serviços externos.").size(12));
            if self.local_search_status.can_download || self.local_search_status.can_retry {
                content = content.push(
                    button(text(if self.local_search_status.can_retry {
                        "Tentar novamente / reindexar"
                    } else {
                        "Baixar modelo e preparar busca local"
                    }))
                    .on_press_maybe((!local_search_busy).then_some(Message::PrepareLocalSearch)),
                );
            }
            if self.local_search_status.can_remove {
                content = content.push(
                    button(text("Remover modelo de busca baixado"))
                        .on_press_maybe((!local_search_busy).then_some(Message::RemoveLocalSearch)),
                );
            }
            content = content
                .push(text("Inteligência artificial").size(20))
                .push(
                    button(text(if self.assistant_language == "pt-BR" {
                        "Idioma da interface e das respostas: Português"
                    } else {
                        "Idioma da interface e das respostas: English"
                    }))
                    .on_press(Message::CycleAssistantLanguage),
                )
                .push(text("Controla o idioma da interface, das respostas e dos documentos gerados.").size(12))
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
                    button(text("Salvar chave"))
                        .on_press_maybe((!self.action_busy).then_some(Message::SaveAiApiKey))
                        .width(Length::Fill),
                    button(text("Remover chave"))
                        .on_press_maybe((self.ai_api_key_configured && !self.action_busy).then_some(Message::ClearAiApiKey))
                        .width(Length::Fill),
                ].spacing(6))
                .push(text(if self.ai_api_key_configured {
                    "A chave está configurada no Gerenciador de Credenciais do Windows."
                } else {
                    "Nenhuma chave de API está configurada."
                }).size(12))
                .push(text("A chave fica fora do banco local. Perguntas e fontes podem ser salvas como documentos Markdown na biblioteca criptografada.").size(12))
                .push(button(text(if self.launch_at_startup { "Iniciar com o Windows: ligado" } else { "Iniciar com o Windows: desligado" }))
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
                        button(text("Salvar e manter as capturas existentes")).on_press_maybe(
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
            if self.clear_all_data_confirmation {
                content = content
                    .push(text("Apagar capturas, anexos, documentos e credencial de IA? Esta ação não pode ser desfeita. A chave criptográfica da biblioteca será mantida.").size(12))
                    .push(row![
                        button(text("Apagar todos os dados"))
                            .on_press_maybe((!self.action_busy).then_some(Message::ConfirmClearAllData)),
                        button(text("Cancelar"))
                            .on_press_maybe((!self.action_busy).then_some(Message::CancelClearAllData)),
                    ].spacing(8));
            } else {
                content =
                    content.push(button(text("Limpar todos os dados")).on_press_maybe(
                        (!self.action_busy).then_some(Message::RequestClearAllData),
                    ));
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
            for group in snapshot.captures.chunks(2) {
                let mut cards = row![].spacing(12).width(Length::Fill);
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
                        .width(Length::Fixed(250.0)),
                    );
                }
                for _ in group.len()..2 {
                    cards = cards.push(Space::new().width(Length::FillPortion(1)));
                }
                content = content.push(cards);
            }
            if snapshot.total > PAGE_SIZE {
                content = content.push(
                    row![
                        button(text("Anterior"))
                            .on_press_maybe((self.offset > 0).then_some(Message::PreviousPage)),
                        text(format!(
                            "Página {} de {}",
                            self.offset / PAGE_SIZE + 1,
                            snapshot.total.div_ceil(PAGE_SIZE)
                        ))
                        .size(12),
                        button(text("Próxima")).on_press_maybe(
                            (self.offset + PAGE_SIZE < snapshot.total).then_some(Message::NextPage)
                        ),
                    ]
                    .spacing(12)
                    .align_y(Alignment::Center),
                );
            }
            let selected_capture = snapshot
                .captures
                .iter()
                .find(|capture| self.selected_capture.as_deref() == Some(capture.id.as_str()))
                .or_else(|| {
                    self.assistant_capture.as_ref().filter(|capture| {
                        self.selected_capture.as_deref() == Some(capture.id.as_str())
                    })
                });
            if let Some(capture) = selected_capture {
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
                        button(text("Adicionar")).on_press_maybe(
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
                    let mut file_details = column![text(format!(
                        "Arquivo: {} · {}",
                        file.display_name, file.availability
                    ))
                    .size(12),]
                    .spacing(4);
                    if file.availability == "available" {
                        if let Some(path) = file.local_path.as_ref().or(file.original_path.as_ref())
                        {
                            file_details = file_details.push(
                                button(text("Abrir arquivo"))
                                    .on_press(Message::OpenCaptureFile(PathBuf::from(path))),
                            );
                        }
                    }
                    details = details.push(file_details);
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
                            button(text("Confirmar exclusão"))
                                .on_press(Message::DeleteCapture(capture.id.clone())),
                            button(text("Cancelar")).on_press(Message::CancelDeleteCapture),
                        ]
                        .spacing(8),
                    );
                } else {
                    details = details.push(
                        button(text("Excluir captura"))
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
        let mut items = if minimal {
            column![search].spacing(0)
        } else {
            column![
                row![
                    text("Quick Paste").size(20),
                    Space::new().width(Length::Fill),
                    button(text("Minimalista")).on_press(Message::ToggleQuickPasteStyle),
                    button(text("Fechar")).on_press(Message::FinishQuickPaste)
                ]
                .align_y(Alignment::Center),
                text("Ctrl + Shift + V para abrir · escolha uma captura para colar").size(11),
                search
            ]
            .spacing(12)
        };
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
            if !minimal && snapshot.total > PAGE_SIZE {
                items = items.push(
                    row![
                        button(text("‹"))
                            .on_press_maybe((self.offset > 0).then_some(Message::PreviousPage)),
                        text(format!(
                            "{} / {}",
                            self.offset / PAGE_SIZE + 1,
                            snapshot.total.div_ceil(PAGE_SIZE)
                        ))
                        .size(11),
                        button(text("›")).on_press_maybe(
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
            .padding(if minimal { 8 } else { 14 })
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
                button(text(if minimal { "Minimalista" } else { "Normal" }))
                    .on_press(Message::ToggleQuickContextStyle),
                button(text("Fechar")).on_press(Message::FinishQuickContext),
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
        content =
            content.push(button(text("Concluir depois")).on_press(Message::FinishQuickContext));
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

fn local_search_ticks() -> impl iced::futures::Stream<Item = Message> {
    iced::stream::channel(
        8,
        |mut output: iced::futures::channel::mpsc::Sender<Message>| async move {
            use iced::futures::{channel::mpsc, sink::SinkExt, StreamExt};
            let (sender, mut receiver) = mpsc::unbounded();
            thread::spawn(move || loop {
                thread::sleep(std::time::Duration::from_millis(500));
                if sender.unbounded_send(Message::LocalSearchTick).is_err() {
                    break;
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

fn hotkey_events() -> impl iced::futures::Stream<Item = Message> {
    iced::stream::channel(
        8,
        |mut output: iced::futures::channel::mpsc::Sender<Message>| async move {
            use iced::futures::{channel::mpsc, sink::SinkExt, StreamExt};
            let (sender, mut receiver) = mpsc::unbounded();
            let quick_paste_id = QUICK_PASTE_HOTKEY_ID.get().copied();
            let capture_id = CAPTURE_HOTKEY_ID.get().copied();
            let reference_id = REFERENCE_HOTKEY_ID.get().copied();
            let magic_search_id = MAGIC_SEARCH_HOTKEY_ID.get().copied();
            let _ = thread::Builder::new()
                .name("scrypuppy-hotkey-events".into())
                .spawn(move || {
                    while let Ok(event) = GlobalHotKeyEvent::receiver().recv() {
                        if event.state != HotKeyState::Pressed {
                            continue;
                        }
                        if Some(event.id) == capture_id {
                            if sender
                                .unbounded_send(Message::CaptureSelection(false))
                                .is_err()
                            {
                                break;
                            }
                            continue;
                        }
                        if Some(event.id) == reference_id {
                            if sender
                                .unbounded_send(Message::CaptureSelection(true))
                                .is_err()
                            {
                                break;
                            }
                            continue;
                        }
                        if Some(event.id) == magic_search_id {
                            if sender.unbounded_send(Message::OpenMagicSearch).is_err() {
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

#[cfg(windows)]
fn set_quick_paste_hit_region(id: window::Id, minimal: bool) -> Task<Message> {
    use raw_window_handle::RawWindowHandle;
    use windows::Win32::{
        Foundation::{HWND, RECT},
        Graphics::Gdi::{CreateRoundRectRgn, DeleteObject, SetWindowRgn, HGDIOBJ},
        UI::{HiDpi::GetDpiForWindow, WindowsAndMessaging::GetClientRect},
    };

    window::run(id, move |window| {
        let Ok(handle) = window.window_handle() else {
            return;
        };
        let RawWindowHandle::Win32(handle) = handle.as_raw() else {
            return;
        };
        let hwnd = HWND(handle.hwnd.get() as *mut std::ffi::c_void);
        unsafe {
            if !minimal {
                let _ = SetWindowRgn(hwnd, None, true);
                return;
            }

            let mut client = RECT::default();
            if GetClientRect(hwnd, &mut client).is_err() {
                return;
            }
            let width = client.right - client.left;
            let height = client.bottom - client.top;
            let dpi = GetDpiForWindow(hwnd);
            let Some(bounds) = quick_paste_hit_region_bounds(width, height, dpi) else {
                return;
            };

            let region = CreateRoundRectRgn(
                bounds.left,
                bounds.top,
                bounds.right,
                bounds.bottom,
                bounds.corner_radius,
                bounds.corner_radius,
            );
            if SetWindowRgn(hwnd, Some(region), true) == 0 {
                let _ = DeleteObject(HGDIOBJ(region.0));
            }
        }
    })
    .discard()
}

#[cfg(not(windows))]
fn set_quick_paste_hit_region(_id: window::Id, _minimal: bool) -> Task<Message> {
    Task::none()
}

#[derive(Debug, PartialEq, Eq)]
struct QuickPasteHitRegionBounds {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
    corner_radius: i32,
}

fn quick_paste_hit_region_bounds(
    width: i32,
    height: i32,
    dpi: u32,
) -> Option<QuickPasteHitRegionBounds> {
    let scale = dpi.max(1) as f32 / 96.0;
    let margin = (14.0 * scale).round() as i32;
    if width <= margin * 2 || height <= margin * 2 {
        return None;
    }
    let corner_radius = (36.0 * scale).round() as i32;
    Some(QuickPasteHitRegionBounds {
        left: margin,
        top: margin,
        right: width - margin,
        bottom: height - margin,
        corner_radius,
    })
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
        documents: Vec::new(),
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
        quick_context_after_reference: false,
        onboarding_completed: true,
        launch_at_startup: false,
        retention_policy: "3_months".into(),
        assistant_language: "pt-BR".into(),
        ai_provider: "deepseek".into(),
        ai_model: "deepseek-v4-flash".into(),
        ai_api_key_configured: false,
        context_rules: Vec::new(),
    }
}

#[cfg(debug_assertions)]
fn demo_assistant_answer(query: &str) -> scrypuppy_core::assistant::ChatAnswer {
    let evidence = demo_snapshot()
        .captures
        .into_iter()
        .take(2)
        .map(|capture| scrypuppy_core::assistant::EvidenceItem {
            capture_id: capture.id,
            captured_at: capture.captured_at,
            context_names: vec!["Trabalho".into()],
            app_name: capture.source_app_name,
            application_id: None,
            window_title: None,
            excerpt: capture.content_text,
            matched_fields: vec!["demonstração".into()],
        })
        .collect();
    scrypuppy_core::assistant::ChatAnswer {
        answer: format!(
            "Resposta simulada para ‘{query}’; nenhum dado real ou provedor externo foi acessado."
        ),
        confidence: "demo".into(),
        evidence,
    }
}

fn prepare_local_search(
    manager: Arc<LocalSearchManager>,
    allow_download: bool,
) -> Result<LocalSearchStatus, String> {
    let was_installed = manager.installed();
    let result = (|| {
        let (path, key) = default_database_credentials()?;
        let mut conn = open_existing_read_write(&path, &key)?;
        manager.load_model(allow_download)?;
        if !was_installed {
            manager.mark_installed()?;
        }
        manager.set_phase(LocalSearchPhase::Indexing, None);
        scrypuppy_core::local_search::index_embeddings(&mut conn, &manager, |_| {})?;
        manager.set_phase(LocalSearchPhase::Ready, None);
        Ok(())
    })();
    manager.finish_operation();
    if let Err(error) = result {
        manager.set_phase(
            LocalSearchPhase::Error,
            Some(scrypuppy_core::app_error::AppNotice::new(
                "local_search.operation_failed",
            )),
        );
        return Err(error);
    }
    Ok(manager.status())
}

fn remove_local_search(manager: Arc<LocalSearchManager>) -> Result<LocalSearchStatus, String> {
    let result = manager.remove_model();
    manager.finish_operation();
    if let Err(error) = result {
        manager.set_phase(
            LocalSearchPhase::Error,
            Some(scrypuppy_core::app_error::AppNotice::new(
                "local_search.remove_failed",
            )),
        );
        return Err(error);
    }
    Ok(manager.status())
}

fn delete_all_library_data(manager: Arc<LocalSearchManager>) -> Result<(), String> {
    manager.request_cancel();
    for _ in 0..500 {
        if !manager.operation_running() {
            break;
        }
        thread::sleep(std::time::Duration::from_millis(10));
    }
    if manager.operation_running() {
        return Err(
            "A busca local ainda está ocupada. Tente novamente quando a indexação terminar.".into(),
        );
    }

    let coordinator =
        CAPTURE_COORDINATOR.get_or_init(scrypuppy_core::coordination::CaptureCoordinator::default);
    let _capture_session = coordinator.begin()?;
    let (path, database_key) = default_database_credentials()?;
    let app_dir = path
        .parent()
        .ok_or("Diretório da biblioteca indisponível.")?;
    set_autostart_preference(false)?;
    secure_remove_dir(&app_dir.join("assets"))?;
    secure_remove_dir(&app_dir.join("markdown"))?;
    secure_remove_dir(&app_dir.join("preview-cache"))?;
    for suffix in [
        "",
        "-wal",
        "-shm",
        ".plaintext-migration-backup",
        ".encrypted-tmp",
    ] {
        secure_remove_file_safely(&PathBuf::from(format!("{}{}", path.display(), suffix)))?;
    }
    clear_ai_api_key()?;
    AI_API_KEY_STATUS.store(1, Ordering::Release);

    let conn = scrypuppy_core::storage::open_or_create(app_dir, &database_key)?;
    scrypuppy_core::schema::migrate(&conn).map_err(|error| error.to_string())?;
    save_setting_value_in(&conn, "magic_search_engine", "local")?;
    manager.set_index_progress(0, 0);
    manager.set_phase(
        if manager.installed() {
            LocalSearchPhase::Ready
        } else {
            LocalSearchPhase::NotDownloaded
        },
        None,
    );
    Ok(())
}

fn secure_remove_dir(path: &Path) -> Result<(), String> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.to_string()),
    };
    if metadata.file_type().is_symlink() {
        return Err(format!(
            "Recusando limpar um link simbólico: {}",
            path.display()
        ));
    }
    if !metadata.is_dir() {
        return Err(format!(
            "O caminho de limpeza não é uma pasta: {}",
            path.display()
        ));
    }
    for entry in std::fs::read_dir(path).map_err(|error| error.to_string())? {
        let entry = entry.map_err(|error| error.to_string())?;
        let entry_path = entry.path();
        let file_type = entry.file_type().map_err(|error| error.to_string())?;
        if file_type.is_symlink() {
            return Err(format!(
                "Recusando limpar um link simbólico: {}",
                entry_path.display()
            ));
        }
        if file_type.is_dir() {
            secure_remove_dir(&entry_path)?;
        } else {
            secure_remove_file_safely(&entry_path)?;
        }
    }
    std::fs::remove_dir(path).map_err(|error| error.to_string())
}

fn secure_remove_file_safely(path: &Path) -> Result<(), String> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(format!(
            "Recusando apagar um link simbólico: {}",
            path.display()
        )),
        Ok(_) => scrypuppy_core::context_exports::secure_remove_file(path),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.to_string()),
    }
}

fn refresh_local_search_data(
    manager: Arc<LocalSearchManager>,
) -> Result<LocalSearchStatus, String> {
    let result = (|| {
        let (path, key) = default_database_credentials()?;
        let mut conn = open_existing_read_write(&path, &key)?;
        manager.load_model(false)?;
        manager.set_phase(LocalSearchPhase::Indexing, None);
        scrypuppy_core::local_search::index_embeddings(&mut conn, &manager, |_| {})?;
        manager.set_phase(LocalSearchPhase::Ready, None);
        Ok(())
    })();
    manager.finish_operation();
    if let Err(error) = result {
        manager.set_phase(
            LocalSearchPhase::Error,
            Some(scrypuppy_core::app_error::AppNotice::new(
                "local_search.operation_failed",
            )),
        );
        return Err(error);
    }
    Ok(manager.status())
}

fn load_default_library(mut filter: CaptureFilter) -> Result<Snapshot, String> {
    let (path, key) = default_database_credentials()?;
    filter.date_from = filter
        .date_from
        .as_deref()
        .map(|value| normalize_assistant_date_filter(value, false))
        .transpose()?
        .flatten();
    filter.date_to = filter
        .date_to
        .as_deref()
        .map(|value| normalize_assistant_date_filter(value, true))
        .transpose()?
        .flatten();
    load_library(&path, &key, filter)
        .map_err(|_| "Não foi possível abrir a biblioteca existente com segurança.".into())
}

fn library_kind_filter_label(kind: &str) -> &'static str {
    match kind {
        "text" => "Tipo: Texto",
        "image" => "Tipo: Imagem",
        "link" => "Tipo: Link",
        "files" => "Tipo: Arquivos",
        _ => "Tipo: Todos",
    }
}

#[cfg(debug_assertions)]
fn matches_library_content_kind(capture: &CaptureDto, filter: &str) -> bool {
    match filter {
        "text" => matches!(
            capture.content_kind.as_str(),
            "plain_text" | "rich_text" | "html"
        ),
        "image" => capture.content_kind == "image",
        "link" => capture.kind == "reference" || capture.content_kind == "url",
        "files" => capture.content_kind == "files" || !capture.files.is_empty(),
        _ => true,
    }
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

fn answer_library_question(
    query: &str,
    context_id: Option<String>,
    app: String,
    date_from: Option<String>,
    date_to: Option<String>,
    local_search: Arc<LocalSearchManager>,
) -> Result<scrypuppy_core::assistant::ChatAnswer, String> {
    let (path, database_key) = default_database_credentials()?;
    let conn = open_existing_read_only(&path, &database_key)?;
    let setting = |key: &str, fallback: &str| {
        conn.query_row("SELECT value FROM settings WHERE key = ?1", [key], |row| {
            row.get::<_, String>(0)
        })
        .unwrap_or_else(|_| fallback.into())
    };
    let provider = setting("ai_provider", "deepseek");
    let model = setting("ai_model", &scrypuppy_core::ai::default_model(&provider));
    let api_key = CredentialEntry::new(CREDENTIAL_SERVICE, AI_KEY_CREDENTIAL)
        .ok()
        .and_then(|entry| entry.get_password().ok())
        .unwrap_or_default();
    let settings = scrypuppy_core::assistant::AssistantSettings {
        language: setting("language", "en"),
        provider,
        model,
        api_key,
    };
    let ranked_ids = scrypuppy_core::local_search::rank_capture_ids(&conn, &local_search, query)?;
    scrypuppy_core::assistant::answer_chat_with_ranked_ids(
        &conn,
        &settings,
        &scrypuppy_core::assistant::ChatRequest {
            query: query.into(),
            context_id,
            app: (!app.is_empty()).then_some(app),
            date_from,
            date_to,
            limit: Some(12),
        },
        Some(&ranked_ids),
    )
}

fn normalize_assistant_date_filter(
    value: &str,
    end_of_day: bool,
) -> Result<Option<String>, String> {
    let value = value.trim();
    if value.is_empty() {
        return Ok(None);
    }
    let date = chrono::NaiveDate::parse_from_str(value, "%Y-%m-%d")
        .map_err(|_| "Use datas no formato AAAA-MM-DD.".to_string())?;
    let (date, time) = if end_of_day {
        (
            date.succ_opt()
                .ok_or_else(|| "A data final não pode passar do último dia válido.".to_string())?,
            chrono::NaiveTime::from_hms_opt(0, 0, 0),
        )
    } else {
        (date, chrono::NaiveTime::from_hms_opt(0, 0, 0))
    };
    let time = time.ok_or_else(|| "Data inválida.".to_string())?;
    Ok(Some(
        date.and_time(time)
            .format("%Y-%m-%dT%H:%M:%S%.f")
            .to_string(),
    ))
}

fn load_capture_by_id(capture_id: &str) -> Result<CaptureDto, String> {
    let (path, database_key) = default_database_credentials()?;
    let conn = open_existing_read_only(&path, &database_key)?;
    scrypuppy_core::library::get_capture_by_id(&conn, capture_id)
}

#[cfg(windows)]
#[link(name = "shell32")]
unsafe extern "system" {
    #[link_name = "ShellExecuteW"]
    fn shell_execute_w(
        window: *mut std::ffi::c_void,
        operation: *const u16,
        file: *const u16,
        parameters: *const u16,
        directory: *const u16,
        show_command: i32,
    ) -> *mut std::ffi::c_void;
}

#[cfg(windows)]
fn open_capture_file(path: &Path) -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt;

    if !path.is_absolute() {
        return Err("O caminho do arquivo não é absoluto.".into());
    }
    let operation = std::ffi::OsStr::new("open")
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let file = path
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let result = unsafe {
        shell_execute_w(
            std::ptr::null_mut(),
            operation.as_ptr(),
            file.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            1,
        )
    } as isize;
    if result <= 32 {
        Err(format!(
            "O Windows não conseguiu abrir o arquivo ({result})."
        ))
    } else {
        Ok(())
    }
}

#[cfg(not(windows))]
fn open_capture_file(_: &Path) -> Result<(), String> {
    Err("A abertura de arquivos está disponível apenas no Windows.".into())
}

fn save_answer_as_document(
    query: &str,
    answer: &scrypuppy_core::assistant::ChatAnswer,
) -> Result<scrypuppy_core::documents::Document, String> {
    let (path, database_key) = default_database_credentials()?;
    let mut conn = open_existing_read_write(&path, &database_key)?;
    let transaction = conn.transaction().map_err(|error| error.to_string())?;
    let document = scrypuppy_core::documents::save_answer(&transaction, query, answer, None)?;
    transaction.commit().map_err(|error| error.to_string())?;
    Ok(document)
}

fn save_answer_as_new_document_version(
    query: &str,
    answer: &scrypuppy_core::assistant::ChatAnswer,
    previous_id: &str,
) -> Result<scrypuppy_core::documents::Document, String> {
    let (path, database_key) = default_database_credentials()?;
    let mut conn = open_existing_read_write(&path, &database_key)?;
    let transaction = conn.transaction().map_err(|error| error.to_string())?;
    let document = scrypuppy_core::documents::save_answer_revision(
        &transaction,
        query,
        answer,
        None,
        Some(previous_id),
    )?;
    transaction.commit().map_err(|error| error.to_string())?;
    Ok(document)
}

fn generate_ai_document_as_document(
    query: &str,
    answer: &scrypuppy_core::assistant::ChatAnswer,
) -> Result<scrypuppy_core::documents::Document, String> {
    let (path, database_key) = default_database_credentials()?;
    let mut conn = open_existing_read_write(&path, &database_key)?;
    let setting = |key: &str, fallback: &str| {
        conn.query_row("SELECT value FROM settings WHERE key = ?1", [key], |row| {
            row.get::<_, String>(0)
        })
        .unwrap_or_else(|_| fallback.into())
    };
    let provider = setting("ai_provider", "deepseek");
    let model = setting("ai_model", &scrypuppy_core::ai::default_model(&provider));
    let api_key = CredentialEntry::new(CREDENTIAL_SERVICE, AI_KEY_CREDENTIAL)
        .ok()
        .and_then(|entry| entry.get_password().ok())
        .unwrap_or_default();
    let settings = scrypuppy_core::assistant::AssistantSettings {
        language: setting("language", "en"),
        provider,
        model,
        api_key,
    };
    let markdown = scrypuppy_core::assistant::generate_document_markdown(&settings, query, answer)?;
    let generated_answer = scrypuppy_core::assistant::ChatAnswer {
        answer: markdown,
        confidence: answer.confidence.clone(),
        evidence: answer.evidence.clone(),
    };
    let transaction = conn.transaction().map_err(|error| error.to_string())?;
    let document =
        scrypuppy_core::documents::save_answer(&transaction, query, &generated_answer, None)?;
    transaction.commit().map_err(|error| error.to_string())?;
    Ok(document)
}

fn load_saved_document(id: &str) -> Result<scrypuppy_core::documents::Document, String> {
    let (path, database_key) = default_database_credentials()?;
    let conn = open_existing_read_only(&path, &database_key)?;
    scrypuppy_core::documents::get(&conn, id)
}

fn save_document_markdown(id: &str, markdown: &str) -> Result<(), String> {
    let (path, database_key) = default_database_credentials()?;
    let conn = open_existing_read_write(&path, &database_key)?;
    scrypuppy_core::documents::update_markdown(&conn, id, markdown)
}

fn delete_saved_document_root(root_id: &str) -> Result<(), String> {
    let (path, database_key) = default_database_credentials()?;
    let mut conn = open_existing_read_write(&path, &database_key)?;
    let transaction = conn.transaction().map_err(|error| error.to_string())?;
    scrypuppy_core::documents::delete_root(&transaction, root_id)?;
    transaction.commit().map_err(|error| error.to_string())
}

fn document_export_filename(document: &scrypuppy_core::documents::Document) -> String {
    let slug = document
        .title
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character.to_ascii_lowercase()
            } else if character.is_whitespace() || character == '-' || character == '_' {
                '-'
            } else {
                '\0'
            }
        })
        .filter(|character| *character != '\0')
        .collect::<String>()
        .split('-')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    format!(
        "{}-{}.md",
        if slug.is_empty() { "documento" } else { &slug },
        &document.id[..8.min(document.id.len())]
    )
}

fn export_document_markdown(
    document: &scrypuppy_core::documents::Document,
    selected_path: &Path,
) -> Result<String, String> {
    let mut path = selected_path.to_path_buf();
    if path.extension().is_none() {
        path.set_extension("md");
    }
    std::fs::write(&path, &document.markdown).map_err(|error| error.to_string())?;
    Ok(path.to_string_lossy().into_owned())
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
    save_setting_value("launch_at_startup", if enabled { "true" } else { "false" })
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
    scrypuppy_core::context_exports::write_context_export(conn, &export_dir, &context, context_key)
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
    let _capture_session = CAPTURE_COORDINATOR
        .get_or_init(scrypuppy_core::coordination::CaptureCoordinator::default)
        .begin()?;
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
    let _ = scrypuppy_core::preview::remove_cached_previews(
        &app_dir.join("preview-cache"),
        &capture_id,
    );

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

fn import_file_paths(paths: Vec<PathBuf>) -> Result<usize, String> {
    if paths.is_empty() {
        return Ok(0);
    }
    let _capture_session = CAPTURE_COORDINATOR
        .get_or_init(scrypuppy_core::coordination::CaptureCoordinator::default)
        .begin()?;
    if DATA_RESET_IN_PROGRESS.load(Ordering::Acquire) {
        return Err("A limpeza da biblioteca está em andamento.".into());
    }
    let (database_path, database_key) = default_database_credentials()?;
    let app_dir = database_path
        .parent()
        .ok_or("Diretório da biblioteca indisponível.")?;
    let mut conn = open_existing_read_write(&database_path, &database_key).map_err(|_| {
        "Não foi possível abrir a biblioteca com segurança para edição.".to_string()
    })?;
    let mut imported = 0usize;
    for source_path in paths {
        if !source_path.exists() {
            return Err(format!(
                "O caminho selecionado não existe: {}",
                source_path.display()
            ));
        }
        let file = scrypuppy_core::clipboard::ClipboardFile::physical(source_path);
        let snapshot = scrypuppy_core::clipboard::ClipboardSnapshot {
            representations: vec![scrypuppy_core::clipboard::ClipboardRepresentation::Files(
                vec![file],
            )],
            formats: Vec::new(),
        };
        let id = scrypuppy_core::capture_store::save_clipboard_capture_with_window_metadata(
            &mut conn,
            app_dir,
            &snapshot,
            "capture",
            scrypuppy_core::capture_store::CaptureSourceMetadata {
                capture_origin: "file_import",
                source_app_name: Some("Windows Explorer"),
                ..scrypuppy_core::capture_store::CaptureSourceMetadata::default()
            },
        )?;
        let context_ids = scrypuppy_core::context_rules::apply_enabled_rules(&conn, &id)?;
        sync_context_exports_for_capture(&conn, app_dir, &context_ids)?;
        imported += 1;
    }
    Ok(imported)
}

fn capture_current_clipboard() -> Result<String, String> {
    let _capture_session = CAPTURE_COORDINATOR
        .get_or_init(scrypuppy_core::coordination::CaptureCoordinator::default)
        .begin()?;
    if DATA_RESET_IN_PROGRESS.load(Ordering::Acquire) {
        return Err("A limpeza da biblioteca está em andamento.".into());
    }
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
        scrypuppy_core::capture_store::CaptureSourceMetadata {
            capture_origin: "explicit_hotkey",
            source_app_name: active.app_name.as_deref(),
            source_app_id: active.app_id.as_deref(),
            source_process_id: active.process_id,
            source_process_path: active.process_path.as_deref(),
            window_title: active.window_title.as_deref(),
            window_id: active.window_id.as_deref(),
        },
    )?;
    if include_images {
        queue_window_screenshot(id.clone(), active);
    }
    let context_ids = scrypuppy_core::context_rules::apply_enabled_rules(&conn, &id)?;
    sync_context_exports_for_capture(&conn, app_dir, &context_ids)?;
    if scrypuppy_core::ocr::enqueue_capture(&conn, &id)? {
        start_ocr_worker();
    }
    Ok(id)
}

fn capture_selected_content(capture_kind: &str) -> Result<String, String> {
    use enigo::{Direction, Enigo, Key, Keyboard, Settings};
    use std::{thread, time::Duration};

    let coordinator =
        CAPTURE_COORDINATOR.get_or_init(scrypuppy_core::coordination::CaptureCoordinator::default);
    let _capture_session = coordinator.begin()?;
    if DATA_RESET_IN_PROGRESS.load(Ordering::Acquire) {
        return Err("A limpeza da biblioteca está em andamento.".into());
    }
    let suppression = coordinator.suppress_monitor();
    let clipboard = scrypuppy_core::clipboard::ClipboardService::start()?;
    let previous = clipboard.read().ok().flatten();
    let marker = format!("__SCRY_PUPPY_REFERENCE_MARKER_{}__", uuid::Uuid::new_v4());
    if let Err(error) = clipboard.write(scrypuppy_core::clipboard::ClipboardSnapshot::text(&marker))
    {
        mark_internal_clipboard_sequence();
        clipboard.shutdown();
        return Err(error);
    }

    let copy_result = (|| {
        let mut enigo = Enigo::new(&Settings::default()).map_err(|error| error.to_string())?;
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
        thread::sleep(Duration::from_millis(120));
        #[cfg(target_os = "macos")]
        let modifier = Key::Meta;
        #[cfg(not(target_os = "macos"))]
        let modifier = Key::Control;
        enigo
            .key(modifier, Direction::Press)
            .map_err(|error| error.to_string())?;
        let copy = enigo.key(Key::Unicode('c'), Direction::Click);
        let release = enigo.key(modifier, Direction::Release);
        copy.map_err(|error| error.to_string())?;
        release.map_err(|error| error.to_string())?;

        for _ in 0..8 {
            thread::sleep(Duration::from_millis(50));
            if let Some(snapshot) = clipboard.read()? {
                if snapshot.plain_text() == Some(marker.as_str())
                    && snapshot.representations.len() == 1
                {
                    continue;
                }
                return Ok(snapshot);
            }
        }
        Err("Nenhum conteúdo foi copiado. Selecione texto ou uma imagem antes do atalho.".into())
    })();

    let snapshot = match copy_result {
        Ok(snapshot) => snapshot,
        Err(error) => {
            if let Some(previous) = previous.as_ref() {
                let _ = clipboard.write(previous.clone());
            }
            mark_internal_clipboard_sequence();
            clipboard.shutdown();
            return Err(error);
        }
    };
    clipboard.shutdown();
    if capture_kind == "reference" && snapshot.files().is_some() {
        if let Some(previous) = previous.as_ref() {
            let _ = restore_reference_clipboard(previous);
        }
        return Err("A Knowledge Base aceita texto e imagens selecionados, não arquivos.".into());
    }
    if snapshot.content_text().trim().is_empty() && snapshot.image().is_none() {
        if let Some(previous) = previous.as_ref() {
            let _ = restore_reference_clipboard(previous);
        }
        return Err(if capture_kind == "reference" {
            "Selecione texto ou uma imagem antes do atalho de referência.".into()
        } else {
            "Não havia conteúdo selecionado para salvar.".into()
        });
    }

    mark_internal_clipboard_sequence();
    let (path, database_key) = default_database_credentials()?;
    let app_dir = path
        .parent()
        .ok_or("Diretório da biblioteca indisponível.")?;
    let settings_conn = open_existing_read_only(&path, &database_key)?;
    let include_screenshot = read_boolean_setting(&settings_conn, "capture_screenshots", true);
    drop(settings_conn);
    let active = active_window_metadata();
    let mut conn = open_existing_read_write(&path, &database_key).map_err(|_| {
        "Não foi possível abrir a biblioteca com segurança para edição.".to_string()
    })?;
    let id = scrypuppy_core::capture_store::save_clipboard_capture_with_window_metadata(
        &mut conn,
        app_dir,
        &snapshot,
        capture_kind,
        scrypuppy_core::capture_store::CaptureSourceMetadata {
            capture_origin: "explicit_hotkey",
            source_app_name: active.app_name.as_deref(),
            source_app_id: active.app_id.as_deref(),
            source_process_id: active.process_id,
            source_process_path: active.process_path.as_deref(),
            window_title: active.window_title.as_deref(),
            window_id: active.window_id.as_deref(),
        },
    )?;
    if capture_kind == "capture" && include_screenshot {
        queue_window_screenshot(id.clone(), active);
    }
    let context_ids = scrypuppy_core::context_rules::apply_enabled_rules(&conn, &id)?;
    sync_context_exports_for_capture(&conn, app_dir, &context_ids)?;
    if scrypuppy_core::ocr::enqueue_capture(&conn, &id)? {
        start_ocr_worker();
    }
    drop(suppression);
    Ok(id)
}

fn restore_reference_clipboard(
    snapshot: &scrypuppy_core::clipboard::ClipboardSnapshot,
) -> Result<(), String> {
    let service = scrypuppy_core::clipboard::ClipboardService::start()?;
    let result = service.write(snapshot.clone());
    service.shutdown();
    result?;
    mark_internal_clipboard_sequence();
    Ok(())
}

fn mark_internal_clipboard_sequence() {
    #[cfg(windows)]
    CAPTURE_COORDINATOR
        .get_or_init(scrypuppy_core::coordination::CaptureCoordinator::default)
        .mark_internal_sequence(unsafe {
            windows::Win32::System::DataExchange::GetClipboardSequenceNumber()
        });
}

fn save_window_screenshot(
    conn: &rusqlite::Connection,
    app_dir: &Path,
    capture_id: &str,
    active: &ActiveWindowMetadata,
) -> Result<(), String> {
    let capture_exists: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM captures WHERE id = ?1)",
            [capture_id],
            |row| row.get(0),
        )
        .map_err(|error| error.to_string())?;
    if !capture_exists {
        return Err("A captura foi removida antes de concluir o screenshot.".into());
    }
    let directory = app_dir.join("assets").join("screenshots");
    std::fs::create_dir_all(&directory).map_err(|error| error.to_string())?;
    let path = directory.join(format!("{capture_id}.png"));
    let (saved_path, status, error) = match capture_active_window_image(active, &path) {
        Ok(()) => (Some(path.clone()), "saved", None),
        Err(error) => (None, "unavailable", Some(error)),
    };
    let now = chrono::Utc::now().to_rfc3339();
    let insert = conn.execute(
        "INSERT INTO capture_assets (id, capture_id, kind, path, status, error, created_at)
         VALUES (?1, ?2, 'screenshot', ?3, ?4, ?5, ?6)",
        rusqlite::params![
            uuid::Uuid::new_v4().to_string(),
            capture_id,
            saved_path.as_ref().map(|path| path.display().to_string()),
            status,
            error,
            now
        ],
    );
    if let Err(error) = insert {
        if let Some(path) = saved_path {
            let _ = std::fs::remove_file(path);
        }
        return Err(error.to_string());
    }
    Ok(())
}

fn queue_window_screenshot(capture_id: String, active: ActiveWindowMetadata) {
    let _ = thread::Builder::new()
        .name("scrypuppy-window-screenshot".into())
        .spawn(move || {
            let result = (|| -> Result<(), String> {
                let _capture_session = CAPTURE_COORDINATOR
                    .get_or_init(scrypuppy_core::coordination::CaptureCoordinator::default)
                    .begin()?;
                if DATA_RESET_IN_PROGRESS.load(Ordering::Acquire) {
                    return Ok(());
                }
                let (path, database_key) = default_database_credentials()?;
                let app_dir = path
                    .parent()
                    .ok_or("Diretório da biblioteca indisponível.")?;
                let conn = open_existing_read_write(&path, &database_key).map_err(|_| {
                    "Não foi possível abrir a biblioteca com segurança para edição.".to_string()
                })?;
                save_window_screenshot(&conn, app_dir, &capture_id, &active)?;
                if scrypuppy_core::ocr::enqueue_capture(&conn, &capture_id)? {
                    start_ocr_worker();
                }
                Ok(())
            })();
            send_clipboard_message(Message::ScreenshotSaved(result));
        });
}

#[cfg(windows)]
fn capture_active_window_image(active: &ActiveWindowMetadata, path: &Path) -> Result<(), String> {
    let windows = xcap::Window::all().map_err(|error| error.to_string())?;
    let active_pid = active.process_id.map(|pid| pid as u32);
    let active_title = active.window_title.as_deref().unwrap_or_default();
    let active_app = active.app_name.as_deref().unwrap_or_default();
    let window = windows
        .into_iter()
        .filter_map(|window| {
            let pid_matches = active_pid
                .and_then(|pid| window.pid().ok().map(|window_pid| window_pid == pid))
                .unwrap_or(false);
            let title_matches = window
                .title()
                .ok()
                .map(|title| {
                    !active_title.trim().is_empty()
                        && normalize_window_label(&title) == normalize_window_label(active_title)
                })
                .unwrap_or(false);
            let app_matches = window
                .app_name()
                .ok()
                .map(|app| {
                    !active_app.trim().is_empty()
                        && normalize_window_label(&app) == normalize_window_label(active_app)
                })
                .unwrap_or(false);
            let area = u64::from(window.width().unwrap_or_default())
                .saturating_mul(u64::from(window.height().unwrap_or_default()));
            let score = if pid_matches && title_matches {
                400
            } else if title_matches && app_matches {
                300
            } else if pid_matches && app_matches && area > 20_000 {
                100
            } else if pid_matches && area > 20_000 {
                50
            } else {
                0
            };
            (score > 0).then_some((score, area, window))
        })
        .max_by(|left, right| left.0.cmp(&right.0).then_with(|| left.1.cmp(&right.1)))
        .map(|(_, _, window)| window)
        .ok_or_else(|| {
            "Não foi possível identificar a janela ativa para o screenshot.".to_string()
        })?;
    if window.is_minimized().unwrap_or(false) {
        return Err("A janela ativa está minimizada.".into());
    }
    window
        .capture_image()
        .map_err(|error| error.to_string())?
        .save(path)
        .map_err(|error| error.to_string())
}

#[cfg(not(windows))]
fn capture_active_window_image(_active: &ActiveWindowMetadata, _path: &Path) -> Result<(), String> {
    Err("A captura da janela está disponível somente no Windows.".into())
}

fn normalize_window_label(value: &str) -> String {
    value.trim().to_lowercase()
}

#[derive(Clone, Default)]
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
    let capture_window_screenshot = include_images;
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
        scrypuppy_core::capture_store::CaptureSourceMetadata {
            capture_origin: "clipboard_monitor",
            source_app_name: active.app_name.as_deref(),
            source_app_id: active.app_id.as_deref(),
            source_process_id: active.process_id,
            source_process_path: active.process_path.as_deref(),
            window_title: active.window_title.as_deref(),
            window_id: active.window_id.as_deref(),
        },
    ) {
        Ok(id) => {
            if capture_window_screenshot {
                queue_window_screenshot(id.clone(), active);
            }
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
    let mut conn =
        scrypuppy_core::storage::open_existing_read_write(&path, database_key).map_err(|_| {
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

fn load_library(path: &Path, key: &str, filter: CaptureFilter) -> Result<Snapshot, String> {
    let conn = open_existing_read_only(path, key)?;
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
    let quick_context_after_reference =
        read_boolean_setting(&conn, "quick_context_after_reference", false);
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
    let assistant_language = conn
        .query_row(
            "SELECT value FROM settings WHERE key = 'language'",
            [],
            |row| row.get::<_, String>(0),
        )
        .unwrap_or_else(|_| "en".into());
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
        documents: scrypuppy_core::documents::list(&conn)?,
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
        quick_context_after_reference,
        onboarding_completed,
        launch_at_startup,
        retention_policy,
        assistant_language,
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

fn previews(root: &Path, captures: &[CaptureDto]) -> (Vec<ImagePreview>, Vec<TextPreview>) {
    let mut images = Vec::new();
    let mut text = Vec::new();
    let cache_root = root.parent().unwrap_or(root).join("preview-cache");
    for capture in captures {
        if let Some(path) = capture
            .representations
            .iter()
            .find(|representation| representation.kind == "image")
            .and_then(|representation| representation.asset_path.as_deref())
        {
            if let Ok(bytes) = read_managed_asset(root, Path::new(path)) {
                if let Ok(preview) = scrypuppy_core::preview::cached_preview(
                    &cache_root,
                    &capture.id,
                    "clipboard-image",
                    &bytes,
                    12 * 1024 * 1024,
                    || image_thumbnail(&bytes).map(|preview| preview.png),
                ) {
                    images.push((capture.id.clone(), preview));
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
                    if let Ok(source) = read_managed_asset(root, path) {
                        if let Ok(preview) = scrypuppy_core::preview::cached_preview(
                            &cache_root,
                            &capture.id,
                            "pdf-first-page",
                            &source,
                            12 * 1024 * 1024,
                            || {
                                render_pdf_first_page(path).map_err(|_| {
                                    scrypuppy_core::preview::PreviewError::InvalidDocument
                                })
                            },
                        ) {
                            images.push((capture.id.clone(), preview));
                        }
                    }
                }
                continue;
            }
            if !extension.eq_ignore_ascii_case("txt")
                && !extension.eq_ignore_ascii_case("doc")
                && !extension.eq_ignore_ascii_case("docx")
            {
                continue;
            }
            if let Ok(bytes) = bytes {
                if extension.eq_ignore_ascii_case("docx") {
                    if let Ok(preview) = scrypuppy_core::preview::cached_preview(
                        &cache_root,
                        &capture.id,
                        "docx-first-page",
                        &bytes,
                        12 * 1024 * 1024,
                        || scrypuppy_core::preview::docx_first_page_preview(&bytes),
                    ) {
                        images.push((capture.id.clone(), preview));
                    }
                }
                let kind = if extension.eq_ignore_ascii_case("txt") {
                    "plain-text-excerpt"
                } else if extension.eq_ignore_ascii_case("doc") {
                    "doc-text-excerpt"
                } else {
                    "docx-text-excerpt"
                };
                let excerpt = scrypuppy_core::preview::cached_preview(
                    &cache_root,
                    &capture.id,
                    kind,
                    &bytes,
                    8 * 1024,
                    || {
                        let excerpt = if extension.eq_ignore_ascii_case("txt") {
                            scrypuppy_core::preview::text_excerpt(&bytes)
                        } else if extension.eq_ignore_ascii_case("doc") {
                            scrypuppy_core::preview::doc_first_page_excerpt(&bytes)
                        } else {
                            scrypuppy_core::preview::docx_excerpt(&bytes)
                        }?;
                        Ok(excerpt.into_bytes())
                    },
                );
                if let Ok(excerpt) = excerpt.and_then(|bytes| {
                    String::from_utf8(bytes)
                        .map_err(|_| scrypuppy_core::preview::PreviewError::InvalidText)
                }) {
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
        "doc" => "Prévia de texto do documento Word indisponível.".into(),
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
    use scrypuppy_core::library::CaptureFileDto;
    use scrypuppy_core::storage::open_or_create;

    #[test]
    fn ui_localization_translates_labels_and_dynamic_statuses_without_translating_user_text() {
        assert_eq!(localized_ui_text("Configurações", true), "Settings");
        assert_eq!(
            localized_ui_text(
                "Controla o idioma da interface, das respostas e dos documentos gerados.",
                true
            ),
            "Sets the language used by the interface, answers, and generated documents."
        );
        assert_eq!(
            localized_ui_text("3 fonte(s) vinculada(s)", true),
            "3 linked sources"
        );
        assert_eq!(
            localized_ui_text("Busca local pronta · 12 capturas indexadas", true),
            "Local search ready · 12 captures indexed"
        );
        assert_eq!(
            localized_ui_text("Notas pessoais do usuário", true),
            "Notas pessoais do usuário"
        );
        assert_eq!(localized_ui_text("Configurações", false), "Configurações");
    }

    #[test]
    fn legacy_doc_file_is_shown_as_a_cached_first_page_excerpt() {
        let directory = tempfile::tempdir().unwrap();
        let asset_root = directory.path().join("assets");
        std::fs::create_dir_all(&asset_root).unwrap();
        let source = asset_root.join("legacy-word-sample.doc");
        std::fs::write(
            &source,
            include_bytes!("../../../crates/scrypuppy-core/tests/fixtures/legacy-word-sample.doc"),
        )
        .unwrap();
        let capture = CaptureDto {
            id: "legacy-doc-capture".into(),
            content_text: "Documento Word".into(),
            captured_at: "2026-11-16T12:00:00Z".into(),
            source_app_name: None,
            source_app_id: None,
            source_process_id: None,
            source_process_path: None,
            window_title: None,
            window_id: None,
            platform: "windows".into(),
            kind: "capture".into(),
            content_kind: "file".into(),
            metadata: serde_json::json!({}),
            assets: Vec::new(),
            representations: Vec::new(),
            files: vec![CaptureFileDto {
                id: "legacy-doc-file".into(),
                representation_id: "legacy-doc-representation".into(),
                ordinal: 0,
                display_name: "legacy-word-sample.doc".into(),
                original_path: None,
                local_path: Some(source.to_string_lossy().into_owned()),
                entry_kind: "file".into(),
                extension: Some("doc".into()),
                size_bytes: None,
                sha256: None,
                availability: "available".into(),
                metadata: serde_json::json!({}),
            }],
            clipboard_formats: Vec::new(),
            tags: Vec::new(),
            entities: Vec::new(),
            ocr: None,
            contexts: Vec::new(),
        };

        let (images, text) = previews(&asset_root, &[capture]);

        assert!(images.is_empty());
        assert_eq!(text.len(), 1);
        assert_eq!(text[0].0, "legacy-doc-capture");
        assert!(text[0].1.contains("Concert du soir"));
        assert!(directory.path().join("preview-cache").exists());
    }

    fn load_library_for_test(
        path: &Path,
        key: &str,
        query: String,
        context: Option<String>,
        offset: usize,
    ) -> Result<Snapshot, String> {
        load_library(
            path,
            key,
            CaptureFilter {
                context_id: context,
                search: (!query.trim().is_empty()).then_some(query),
                tag: None,
                app: None,
                date_from: None,
                date_to: None,
                content_kind: None,
                limit: Some(PAGE_SIZE),
                offset: Some(offset),
                exclude_references: false,
            },
        )
    }

    #[test]
    fn quick_paste_hit_region_scales_with_dpi_and_rejects_tiny_windows() {
        assert_eq!(
            quick_paste_hit_region_bounds(430, 570, 96),
            Some(QuickPasteHitRegionBounds {
                left: 14,
                top: 14,
                right: 416,
                bottom: 556,
                corner_radius: 36,
            })
        );
        assert_eq!(
            quick_paste_hit_region_bounds(645, 855, 144),
            Some(QuickPasteHitRegionBounds {
                left: 21,
                top: 21,
                right: 624,
                bottom: 834,
                corner_radius: 54,
            })
        );
        assert_eq!(quick_paste_hit_region_bounds(24, 570, 96), None);
    }

    #[test]
    fn assistant_date_filters_cover_the_whole_requested_day() {
        assert_eq!(
            normalize_assistant_date_filter("2026-11-15", false).unwrap(),
            Some("2026-11-15T00:00:00".into())
        );
        assert_eq!(
            normalize_assistant_date_filter("2026-11-15", true).unwrap(),
            Some("2026-11-16T00:00:00".into())
        );
        assert!(normalize_assistant_date_filter("15/11/2026", false).is_err());
        assert_eq!(normalize_assistant_date_filter("", false).unwrap(), None);
    }

    #[test]
    fn document_export_uses_the_selected_path_and_adds_markdown_extension() {
        let document = scrypuppy_core::documents::Document {
            id: "12345678-90ab-cdef-1234-567890abcdef".into(),
            root_id: "12345678-90ab-cdef-1234-567890abcdef".into(),
            version: 1,
            title: "Notas da reunião".into(),
            query: "reunião".into(),
            markdown: "# Notas da reunião".into(),
            created_at: "2026-11-15T12:00:00Z".into(),
            evidence: Vec::new(),
        };
        let directory = tempfile::tempdir().unwrap();
        let selected_path = directory.path().join("anotacoes");

        assert_eq!(
            document_export_filename(&document),
            "notas-da-reunio-12345678.md"
        );
        let written_path = export_document_markdown(&document, &selected_path).unwrap();
        assert!(written_path.ends_with("anotacoes.md"));
        assert_eq!(
            std::fs::read_to_string(directory.path().join("anotacoes.md")).unwrap(),
            document.markdown
        );
    }

    #[test]
    fn secure_remove_dir_clears_nested_managed_files() {
        let directory = tempfile::tempdir().unwrap();
        let managed = directory.path().join("managed");
        let nested = managed.join("nested");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(nested.join("capture.bin"), b"sensitive clipboard data").unwrap();

        secure_remove_dir(&managed).unwrap();

        assert!(!managed.exists());
    }

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
    fn reference_shortcut_is_registered_as_ctrl_shift_s() {
        let hotkey = HotKey::from_str("ctrl+shift+KeyS").unwrap();
        assert_ne!(hotkey.id(), 0);
    }

    #[test]
    fn magic_search_shortcut_is_registered_as_ctrl_shift_f() {
        let hotkey = HotKey::from_str("ctrl+shift+KeyF").unwrap();
        assert_ne!(hotkey.id(), 0);
    }

    #[test]
    fn ai_provider_and_model_preferences_share_the_library_schema() {
        let mut conn = rusqlite::Connection::open_in_memory().unwrap();
        scrypuppy_core::schema::migrate(&conn).unwrap();
        save_ai_provider_model_in(&mut conn, "openai", "gpt-5-mini").unwrap();
        save_setting_value_in(&conn, "language", "pt-BR").unwrap();

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
        assert_eq!(
            conn.query_row(
                "SELECT value FROM settings WHERE key = 'language'",
                [],
                |row| row.get::<_, String>(0),
            )
            .unwrap(),
            "pt-BR"
        );
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

    #[cfg(debug_assertions)]
    #[test]
    fn assistant_demo_returns_synthetic_evidence_without_opening_the_library() {
        let answer = demo_assistant_answer("planejamento");
        assert!(answer.answer.contains("nenhum dado real"));
        assert_eq!(answer.confidence, "demo");
        assert!(!answer.evidence.is_empty());
        assert!(answer.evidence[0]
            .capture_id
            .chars()
            .all(|character| character.is_ascii_digit()));
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
        let snapshot =
            load_library_for_test(&path, &key, "hello".into(), Some("work".into()), 0).unwrap();
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
        let filtered =
            load_library_for_test(&path, &key, "hello".into(), Some("work".into()), 0).unwrap();
        assert!(filtered.dark);
        assert!(!filtered.quick_paste_minimal);
        assert!(filtered.quick_context_minimal);
        let images = load_library_for_test(&path, &key, "image sample".into(), None, 0).unwrap();
        let (rendered, _) = previews(&images.asset_root, &images.captures);
        assert_eq!(rendered.len(), 1);
        assert_eq!(rendered[0].0, "2");
        assert_eq!(
            ::image::guess_format(&rendered[0].1).unwrap(),
            ::image::ImageFormat::Png
        );
        let first_page = load_library_for_test(&path, &key, String::new(), None, 0).unwrap();
        let second_page =
            load_library_for_test(&path, &key, String::new(), None, PAGE_SIZE).unwrap();
        assert_eq!(first_page.total, 62);
        assert_eq!(first_page.captures.len(), PAGE_SIZE);
        assert_eq!(second_page.captures.len(), 2);
        assert_eq!(
            load_library_for_test(&path, &key, "missing".into(), None, 0)
                .unwrap()
                .total,
            0
        );
        assert!(load_library_for_test(&path, &"34".repeat(32), String::new(), None, 0).is_err());
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

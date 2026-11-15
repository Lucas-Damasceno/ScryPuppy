//! Isolated feasibility probe. This executable never opens the user's database.
use iced::widget::{button, column, container, row, text, text_input};
use iced::{window, Color, Element, Length, Size, Task, Theme};

fn main() -> iced::Result {
    iced::daemon(Probe::new, Probe::update, Probe::view)
        .title(|_: &Probe, _: window::Id| String::from("ScryPuppy — teste Iced"))
        .theme(|_: &Probe, _: window::Id| Theme::Dark)
        .style(|_: &Probe, theme: &Theme| iced::theme::Style {
            background_color: Color::TRANSPARENT,
            text_color: theme.palette().text,
        })
        .subscription(|_: &Probe| window::close_events().map(Message::Closed))
        .run()
}

struct Probe {
    main: window::Id,
    overlay: Option<window::Id>,
    query: String,
}

#[derive(Debug, PartialEq, Eq)]
struct Rect {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

fn interactive_regions(dpi: u32) -> [Rect; 5] {
    let px = |n: f32| (n * dpi as f32 / 96.0).round() as i32;
    let card = |n: usize| {
        let left = 20.0 + n as f32 * 192.0;
        Rect {
            left: px(left),
            top: px(86.0),
            right: px(left + 180.0),
            bottom: px(211.0),
        }
    };
    [
        Rect {
            left: px(20.0),
            top: px(20.0),
            right: px(800.0),
            bottom: px(68.0),
        },
        card(0),
        card(1),
        card(2),
        card(3),
    ]
}

#[derive(Debug, Clone)]
enum Message {
    OpenOverlay,
    OverlayOpened(window::Id),
    RegionApplied(bool),
    Closed(window::Id),
    QueryChanged(String),
}

impl Probe {
    fn new() -> (Self, Task<Message>) {
        let (main, open) = window::open(window::Settings {
            size: Size::new(920.0, 600.0),
            ..window::Settings::default()
        });
        (
            Self {
                main,
                overlay: None,
                query: String::new(),
            },
            open.discard(),
        )
    }

    fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::OpenOverlay if self.overlay.is_none() => {
                let (id, open) = window::open(window::Settings {
                    size: Size::new(820.0, 270.0),
                    decorations: false,
                    transparent: true,
                    resizable: false,
                    level: window::Level::AlwaysOnTop,
                    ..window::Settings::default()
                });
                self.overlay = Some(id);
                open.map(Message::OverlayOpened)
            }
            Message::OverlayOpened(id) => Task::batch([
                window::gain_focus(id),
                window::run(id, install_input_region).map(Message::RegionApplied),
            ]),
            Message::RegionApplied(ok) => {
                if !ok {
                    eprintln!("Could not apply the overlay window region");
                }
                Task::none()
            }
            Message::Closed(id) if id == self.main => iced::exit(),
            Message::Closed(id) => {
                if self.overlay == Some(id) {
                    self.overlay = None;
                }
                Task::none()
            }
            Message::QueryChanged(value) => {
                self.query = value;
                Task::none()
            }
            Message::OpenOverlay => Task::none(),
        }
    }

    fn view(&self, id: window::Id) -> Element<'_, Message> {
        if self.overlay == Some(id) {
            let card = |label| {
                container(text(label).size(16))
                    .padding(20)
                    .width(Length::Fixed(180.0))
                    .height(Length::Fixed(125.0))
                    .style(container::rounded_box)
            };
            column![
                text_input("Buscar no histórico…", &self.query)
                    .on_input(Message::QueryChanged)
                    .padding(14),
                row![card("Link"), card("Texto"), card("Imagem"), card("Arquivo")].spacing(12)
            ]
            .spacing(18)
            .padding(20)
            .into()
        } else {
            container(
                column![
                    text("ScryPuppy · prova técnica Iced").size(28),
                    text("Protótipo isolado: não acessa capturas nem configurações."),
                    button("Abrir janela transparente").on_press(Message::OpenOverlay),
                ]
                .spacing(24),
            )
            .padding(32)
            .width(Length::Fill)
            .height(Length::Fill)
            .style(container::rounded_box)
            .into()
        }
    }
}

// This probe fixes the window size, so it can use the known widget geometry.
// A production implementation must recompute regions on layout and DPI changes.
#[cfg(windows)]
fn install_input_region(window: &dyn window::Window) -> bool {
    use iced::window::raw_window_handle::RawWindowHandle;
    use windows::Win32::{
        Foundation::HWND,
        Graphics::Gdi::{CombineRgn, CreateRoundRectRgn, DeleteObject, SetWindowRgn, RGN_OR},
        UI::HiDpi::GetDpiForWindow,
    };
    let Ok(handle) = window.window_handle() else {
        return false;
    };
    let RawWindowHandle::Win32(raw) = handle.as_raw() else {
        return false;
    };
    let hwnd = HWND(raw.hwnd.get() as *mut std::ffi::c_void);
    unsafe {
        let dpi = GetDpiForWindow(hwnd);
        if dpi == 0 {
            return false;
        }
        let radius = (12.0 * dpi as f32 / 96.0).round() as i32;
        let regions = interactive_regions(dpi);
        let r = &regions[0];
        let total = CreateRoundRectRgn(r.left, r.top, r.right, r.bottom, radius, radius);
        if total.is_invalid() {
            return false;
        }
        for r in &regions[1..] {
            let card = CreateRoundRectRgn(r.left, r.top, r.right, r.bottom, radius, radius);
            if card.is_invalid() {
                let _ = DeleteObject(total.into());
                return false;
            }
            let result = CombineRgn(Some(total), Some(total), Some(card), RGN_OR);
            let _ = DeleteObject(card.into());
            if result.0 == 0 {
                let _ = DeleteObject(total.into());
                return false;
            }
        }
        if SetWindowRgn(hwnd, Some(total), true) == 0 {
            let _ = DeleteObject(total.into());
            return false;
        }
        // On success Windows owns `total` and releases it with the window.
        true
    }
}

#[cfg(not(windows))]
fn install_input_region(_: &dyn window::Window) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn regions_leave_gaps_and_scale_with_dpi() {
        let standard = interactive_regions(96);
        assert_eq!(
            standard[0],
            Rect {
                left: 20,
                top: 20,
                right: 800,
                bottom: 68
            }
        );
        assert!(standard[1].right < standard[2].left);
        assert!(standard[0].bottom < standard[1].top);
        let scaled = interactive_regions(144);
        assert_eq!(
            scaled[0],
            Rect {
                left: 30,
                top: 30,
                right: 1200,
                bottom: 102
            }
        );
        assert_eq!(scaled[1].left, 30);
        assert_eq!(scaled[2].left, 318);
    }
}

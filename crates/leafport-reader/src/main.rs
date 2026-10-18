//! Full-layout EPUB chapter viewer: a native window hosting a WebView that renders the
//! chapter's real HTML and CSS. Run as a separate process by the desktop app so its event
//! loop does not conflict with the main eframe/egui loop. Argument: a file path (or URL).
use tao::{
    event::{Event, WindowEvent},
    event_loop::{ControlFlow, EventLoop},
    window::WindowBuilder,
};
use wry::WebViewBuilder;

fn main() -> wry::Result<()> {
    let target = std::env::args().nth(1).unwrap_or_default();
    if target.is_empty() {
        eprintln!("usage: leafport-reader <file-or-url>");
        std::process::exit(2);
    }
    let url = if target.starts_with("http://")
        || target.starts_with("https://")
        || target.starts_with("file://")
    {
        target
    } else {
        format!("file://{target}")
    };
    let event_loop = EventLoop::new();
    let window = WindowBuilder::new()
        .with_title("Full layout — Leafport")
        .build(&event_loop)
        .expect("window");
    let builder = WebViewBuilder::new().with_url(&url);
    // On Linux/GTK the WebView attaches to the window's GTK container, not a raw handle.
    #[cfg(target_os = "linux")]
    let _webview = {
        use tao::platform::unix::WindowExtUnix;
        use wry::WebViewBuilderExtUnix;
        builder.build_gtk(window.default_vbox().expect("gtk vbox"))?
    };
    #[cfg(not(target_os = "linux"))]
    let _webview = builder.build(&window)?;
    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::Wait;
        if let Event::WindowEvent {
            event: WindowEvent::CloseRequested,
            ..
        } = event
        {
            *control_flow = ControlFlow::Exit;
        }
    });
}

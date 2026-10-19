//! Full-layout EPUB chapter viewer: a native window hosting a WebView that renders the
//! chapter's real HTML and CSS. Run as a separate process by the desktop app so its event
//! loop does not conflict with the main eframe/egui loop. Arguments: a local file path, optional `--root <dir>` limiting navigation.
use tao::{
    event::{Event, WindowEvent},
    event_loop::{ControlFlow, EventLoop},
    window::WindowBuilder,
};
use wry::WebViewBuilder;

fn main() -> wry::Result<()> {
    let mut target = String::new();
    let mut root_arg: Option<String> = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        if arg == "--root" {
            root_arg = args.next();
        } else if target.is_empty() {
            target = arg;
        }
    }
    if target.is_empty() || target.starts_with("http://") || target.starts_with("https://") {
        eprintln!("usage: leafport-reader <file> [--root <dir>]");
        std::process::exit(2);
    }
    let path = if target.starts_with("file://") {
        match url::Url::parse(&target).ok().and_then(|u| u.to_file_path().ok()) {
            Some(p) => p,
            None => {
                eprintln!("leafport-reader: invalid file URL: {target}");
                std::process::exit(2);
            }
        }
    } else {
        std::path::PathBuf::from(&target)
    };
    let abs = match std::fs::canonicalize(&path) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("leafport-reader: cannot open {}: {e}", path.display());
            std::process::exit(2);
        }
    };
    let root = match root_arg {
        Some(r) => std::fs::canonicalize(&r).unwrap_or_else(|e| {
            eprintln!("leafport-reader: bad --root {r}: {e}");
            std::process::exit(2);
        }),
        None => abs.parent().map(|p| p.to_path_buf()).unwrap_or_else(|| abs.clone()),
    };
    if !abs.starts_with(&root) {
        eprintln!("leafport-reader: target is outside root: {}", root.display());
        std::process::exit(2);
    }
    let url = match url::Url::from_file_path(&abs) {
        Ok(u) => u.to_string(),
        Err(()) => {
            eprintln!("leafport-reader: cannot convert path to file URL: {}", abs.display());
            std::process::exit(2);
        }
    };
    let event_loop = EventLoop::new();
    let window = WindowBuilder::new()
        .with_title("Full layout — Leafport")
        .build(&event_loop)
        .expect("window");
    let builder = WebViewBuilder::new()
        .with_url(&url)
        .with_javascript_disabled()
        .with_devtools(false)
        .with_new_window_req_handler(|_| false)
        .with_navigation_handler(move |nav| {
            url::Url::parse(&nav)
                .ok()
                .filter(|u| u.scheme() == "file")
                .and_then(|u| u.to_file_path().ok())
                .and_then(|p| std::fs::canonicalize(p).ok())
                .is_some_and(|p| p.starts_with(&root))
        });
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

// Embed a Win32 application manifest so the GUI uses Common Controls v6
// (modern look, themed widgets) and declares Per-Monitor V2 DPI awareness.
// Without this the app would render like a Windows 95 dialog.

fn main() {
    if std::env::var_os("CARGO_CFG_WINDOWS").is_some() {
        embed_resource::compile("app.rc", embed_resource::NONE);
    }
}

//! Embeds the bitchatd icon into the Windows executable (Explorer thumbnail,
//! taskbar pinned shortcut, Alt-Tab) so it matches the window/taskbar icon
//! set at runtime by the `node_gui` example.

fn main() {
    #[cfg(target_os = "windows")]
    {
        let mut resource = winresource::WindowsResource::new();
        resource.set_icon("assets/bitchatd.ico");
        resource.compile().expect("compile Windows icon resource");
    }
}

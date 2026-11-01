//! Make the application icon available even when it is not installed
//! (`cargo run`, AppImage): extract it into the runtime directory and add
//! that directory to the icon theme search path.

const APP_SVG: &[u8] =
    include_bytes!("../../../data/icons/scalable/net.davidotek.hassdash4linux.svg");
const SYMBOLIC_SVG: &[u8] =
    include_bytes!("../../../data/icons/symbolic/net.davidotek.hassdash4linux-symbolic.svg");

pub fn install() {
    gtk::Window::set_default_icon_name(hd_core::APP_ID);
    let Some(display) = gtk::gdk::Display::default() else {
        return;
    };
    let theme = gtk::IconTheme::for_display(&display);
    if theme.has_icon(hd_core::APP_ID) {
        return;
    }
    let base = gtk::glib::user_runtime_dir()
        .join("hassdash4linux")
        .join("icons");
    let apps = base.join("hicolor").join("scalable").join("apps");
    let res = (|| {
        std::fs::create_dir_all(&apps)?;
        std::fs::write(apps.join(format!("{}.svg", hd_core::APP_ID)), APP_SVG)?;
        std::fs::write(
            apps.join(format!("{}-symbolic.svg", hd_core::APP_ID)),
            SYMBOLIC_SVG,
        )?;
        Ok::<_, std::io::Error>(())
    })();
    match res {
        Ok(()) => theme.add_search_path(&base),
        Err(e) => tracing::warn!("could not install the application icon: {e}"),
    }
}

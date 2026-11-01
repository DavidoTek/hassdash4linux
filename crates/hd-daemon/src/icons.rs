//! Tray and notification icons, embedded as pre-rendered PNGs.
//!
//! AppImage and Flatpak cannot rely on the host's icon theme, so the tray
//! uses pixmaps (ARGB32, network byte order).

use std::sync::OnceLock;

/// The tray icon variants.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayIcon {
    Connected,
    Disconnected,
    Error,
}

macro_rules! sizes {
    ($state:literal) => {
        [
            include_bytes!(concat!("../../../data/icons/tray/", $state, "-16.png")).as_slice(),
            include_bytes!(concat!("../../../data/icons/tray/", $state, "-22.png")).as_slice(),
            include_bytes!(concat!("../../../data/icons/tray/", $state, "-24.png")).as_slice(),
            include_bytes!(concat!("../../../data/icons/tray/", $state, "-32.png")).as_slice(),
            include_bytes!(concat!("../../../data/icons/tray/", $state, "-48.png")).as_slice(),
            include_bytes!(concat!("../../../data/icons/tray/", $state, "-64.png")).as_slice(),
        ]
    };
}

const CONNECTED: [&[u8]; 6] = sizes!("connected");
const DISCONNECTED: [&[u8]; 6] = sizes!("disconnected");
const ERROR: [&[u8]; 6] = sizes!("error");
const APP_48: &[u8] =
    include_bytes!("../../../data/icons/hicolor/48x48/apps/net.davidotek.hassdash4linux.png");

/// Decode a PNG into (width, height, RGBA8 pixels).
pub fn decode_rgba(bytes: &[u8]) -> Option<(i32, i32, Vec<u8>)> {
    let mut decoder = png::Decoder::new(std::io::Cursor::new(bytes));
    decoder.set_transformations(
        png::Transformations::normalize_to_color8() | png::Transformations::ALPHA,
    );
    let mut reader = decoder.read_info().ok()?;
    let mut buf = vec![0; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf).ok()?;
    buf.truncate(info.buffer_size());
    let (w, h) = (info.width as usize, info.height as usize);
    let rgba = match info.color_type {
        png::ColorType::Rgba => buf,
        png::ColorType::GrayscaleAlpha => buf
            .chunks(2)
            .flat_map(|p| [p[0], p[0], p[0], p[1]])
            .collect(),
        _ => return None,
    };
    (rgba.len() == w * h * 4).then_some((w as i32, h as i32, rgba))
}

fn to_ksni(bytes: &[u8]) -> Option<ksni::Icon> {
    let (width, height, rgba) = decode_rgba(bytes)?;
    let data = rgba
        .chunks(4)
        .flat_map(|p| [p[3], p[0], p[1], p[2]])
        .collect();
    Some(ksni::Icon {
        width,
        height,
        data,
    })
}

/// All sizes of a tray icon variant (decoded once).
pub fn tray(icon: TrayIcon) -> Vec<ksni::Icon> {
    static CACHE: OnceLock<[Vec<ksni::Icon>; 3]> = OnceLock::new();
    let all = CACHE.get_or_init(|| {
        let conv = |set: [&[u8]; 6]| set.iter().filter_map(|b| to_ksni(b)).collect::<Vec<_>>();
        [conv(CONNECTED), conv(DISCONNECTED), conv(ERROR)]
    });
    all[icon as usize].clone()
}

/// The 48 px application icon for notifications (RGBA).
pub fn notification_image() -> Option<(i32, i32, Vec<u8>)> {
    decode_rgba(APP_48)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn icons_decode() {
        for i in [TrayIcon::Connected, TrayIcon::Disconnected, TrayIcon::Error] {
            let icons = tray(i);
            assert_eq!(icons.len(), 6);
            for ic in icons {
                assert_eq!(ic.data.len() as i32, ic.width * ic.height * 4);
            }
        }
        assert!(notification_image().is_some());
    }
}

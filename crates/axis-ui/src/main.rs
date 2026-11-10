#![windows_subsystem = "windows"]

mod theme;

use slint::ComponentHandle;

slint::include_modules!();

fn main() -> Result<(), slint::PlatformError> {
    let app = App::new()?;
    let [r, g, b] = theme::accent_rgb("laranja");
    app.global::<Tokens>()
        .set_accent(slint::Color::from_rgb_u8(r, g, b));
    app.run()
}

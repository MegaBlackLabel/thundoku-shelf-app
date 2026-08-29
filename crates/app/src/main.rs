//! Thundoku Shelf desktop binary.

use gpui::*;
use gpui_component::*;

use thundoku_shelf::app_state::AppState;
use thundoku_shelf::icons::AppAssets;
use thundoku_shelf::workspace::{Workspace, app_menus};

fn main() {
    // Windows（Wine/CrossOver）ではコンソール出力が抑制されるためファイルにも出す
    #[cfg(windows)]
    {
        std::panic::set_hook(Box::new(|info| {
            use std::io::Write;
            if let Ok(mut f) =
                std::fs::OpenOptions::new().append(true).open("C:\\thundoku.log")
            {
                let _ = writeln!(f, "PANIC: {info}");
            }
        }));
        env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("debug"))
            .target(env_logger::Target::Pipe(Box::new(
                std::fs::File::create("C:\\thundoku.log").expect("create thundoku.log"),
            )))
            .init();
    }
    #[cfg(not(windows))]
    env_logger::init();
    gpui_platform::application()
        .with_assets(AppAssets)
        .run(move |cx| {
            // Must be called before using any GPUI Component features.
            gpui_component::init(cx);
            cx.set_app_identity("com.megablacklabel.thundoku-shelf", "Thundoku Shelf");
            Theme::sync_system_appearance(None, cx);
            cx.set_menus(app_menus());
            cx.activate(true);

            AppState::init(cx);

            cx.spawn(async move |cx| {
                cx.open_window(TitleBar::window_options(), |window, cx| {
                    let workspace = cx.new(Workspace::new);
                    cx.new(|cx| Root::new(workspace, window, cx).bg(cx.theme().background))
                })
                .unwrap_or_else(|e| panic!("failed to open window: {e:?}"));
            })
            .detach();
        });
}

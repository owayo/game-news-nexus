//! 複数の画面で使う共通の部品。見た目を揃えるため、同じ表示はここの部品で作る。
//!
//! 色はテーマ (`cx.theme()`) と、テーマにない色の [`Palette`](crate::ui::theme::Palette) から取る。
//! 間隔は 4・8・12・16・24・32px、角丸はカード 12px・行と札 6〜8px を基本にする。

mod avatar;
mod layout;
mod marks;
mod nav;
mod run_result;
mod schedule;

pub use avatar::{AppLogo, SourceAvatar};
pub use layout::{
    Card, CodeBlock, EmptyState, FormRow, PageHeader, SettingRow, StatItem, segmented,
};
pub use marks::{Badge, BadgeTone, CountPill, StatusIcon};
pub use nav::NavItem;
pub use run_result::RunResult;
pub use schedule::{MaintenanceMark, MoreChip, ScheduleCard, ScheduleChip};

use gpui_kit::App;

/// ブラウザで開く。http と https 以外は開かない
/// (取得した記事の URL で `ms-settings:` などの手元のアプリを起動されないように)。
pub fn open_web_url(url: &str, cx: &mut App) {
    match url::Url::parse(url) {
        Ok(parsed) if matches!(parsed.scheme(), "http" | "https") => cx.open_url(parsed.as_str()),
        _ => tracing::warn!("http / https 以外の URL は開きません: {url}"),
    }
}

//! 画像から正方形を切り抜いて、ゲームのアイコンにする画面。
//!
//! 画像を選ぶとダイアログが開き、正方形の枠をドラッグで動かし、右下の取っ手か
//! マウスホイールで大きさを変えられる。右に角丸のプレビューを出す。

use std::path::PathBuf;
use std::sync::Arc;

use image::imageops::{self, FilterType};

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::{ActiveTheme as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, AppContext as _, Context, CursorStyle, DragMoveEvent, ImageSource,
    InteractiveElement as _, IntoElement, ObjectFit, ParentElement as _, PathPromptOptions, Pixels,
    Point, Render, RenderImage, ScrollEvent, StatefulInteractiveElement as _, Styled as _,
    StyledImage as _, Window, div, img, px,
};

use crate::icons::{self, Selection};

/// 画像を見せる範囲の一辺の上限。
const VIEW_MAX: f32 = 380.;
/// 枠の一辺の下限 (表示上の px)。
const MIN_SELECTION: f32 = 24.;
/// 大きさを変える取っ手の一辺。
const HANDLE: f32 = 14.;
/// プレビューの一辺。
const PREVIEW: f32 = 88.;
/// プレビューの画像の一辺 (高精細な画面でもぼけないよう 2 倍で作る)。
const PREVIEW_PIXELS: u32 = 176;
/// プレビューを作るための縮小版の一辺の上限。
const THUMBNAIL_MAX: u32 = 720;

type OnDone = Box<dyn FnOnce(PathBuf, &mut Window, &mut App)>;

pub(crate) struct CropTarget {
    source_id: String,
    source_name: String,
    icons_dir: PathBuf,
    on_done: OnDone,
}

pub(super) const FILE_LABEL: &str = if cfg!(windows) {
    "画像・exe から"
} else {
    "画像から"
};

impl CropTarget {
    pub(crate) fn new(
        source_id: String,
        source_name: String,
        icons_dir: PathBuf,
        on_done: impl FnOnce(PathBuf, &mut Window, &mut App) + 'static,
    ) -> Self {
        Self {
            source_id,
            source_name,
            icons_dir,
            on_done: Box::new(on_done),
        }
    }

    pub(super) fn pick_file(self, window: &mut Window, cx: &mut App) {
        let receiver = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some(
                if cfg!(windows) {
                    "画像または exe ファイルを選ぶ"
                } else {
                    "画像を選ぶ"
                }
                .into(),
            ),
        });
        window
            .spawn(cx, async move |cx| {
                if let Ok(Ok(Some(paths))) = receiver.await
                    && let Some(path) = paths.into_iter().next()
                {
                    let _ = cx.update(|window, cx| self.open_file(path, window, cx));
                }
            })
            .detach();
    }

    pub(crate) fn open_file(self, path: PathBuf, window: &mut Window, cx: &mut App) {
        window
            .spawn(cx, async move |cx| {
                let result = cx
                    .background_executor()
                    .spawn(async move { CropImage::new(icons::load_file(&path)?) })
                    .await;
                let _ = cx.update(|window, cx| match result {
                    Ok(image) => self.open(image, window, cx),
                    Err(err) => {
                        window.push_notification(Notification::error(format!("{err:#}")), cx)
                    }
                });
            })
            .detach();
    }

    pub(super) fn open(self, image: CropImage, window: &mut Window, cx: &mut App) {
        let title = format!("「{}」のアイコン", self.source_name);
        let cropper = cx
            .new(|_| IconCropper::new(image, self.source_id, self.icons_dir, self.on_done, window));
        window.open_dialog(cx, move |dialog, _, _| {
            dialog
                .title(title.clone())
                .w(px(VIEW_MAX + PREVIEW + 120.))
                .child(cropper.clone())
        });
    }
}

/// 選択枠とプレビューは同じ BGRA の縮小画像を使い、保存時だけ元の解像度で切り抜く。
pub(super) struct CropImage {
    original: Arc<image::DynamicImage>,
    thumbnail: image::RgbaImage,
    display: Arc<RenderImage>,
}

impl CropImage {
    pub(super) fn new(image: image::DynamicImage) -> anyhow::Result<Self> {
        anyhow::ensure!(image.width() > 0 && image.height() > 0, "画像が空です");
        let mut thumbnail = image.thumbnail(THUMBNAIL_MAX, THUMBNAIL_MAX).to_rgba8();
        for pixel in thumbnail.pixels_mut() {
            pixel.0.swap(0, 2);
        }
        let display = Arc::new(RenderImage::new(vec![image::Frame::new(thumbnail.clone())]));
        Ok(Self {
            original: Arc::new(image),
            thumbnail,
            display,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CropDrag {
    Move,
    Resize,
}

/// ドラッグ中に何も描かないための空の見た目。
struct DragGhost;

impl Render for DragGhost {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
    }
}

pub struct IconCropper {
    image: CropImage,
    /// 画像を見せている大きさ。
    view: (f32, f32),
    selection: Selection,
    /// 枠を動かし始めたとき、枠の中のどこをつかんだか。
    grab: Point<Pixels>,
    source_id: String,
    icons_dir: PathBuf,
    saving: bool,
    error: Option<String>,
    on_done: Option<OnDone>,
    /// 切り抜いた範囲のプレビュー。gpui は親の角丸で子を切り抜かないので、
    /// 実際に切り抜いた画像を作って角丸で描く。
    preview: Option<Arc<RenderImage>>,
}

impl IconCropper {
    fn new(
        image: CropImage,
        source_id: String,
        icons_dir: PathBuf,
        on_done: OnDone,
        window: &mut Window,
    ) -> Self {
        let (width, height) = (
            image.original.width() as f32,
            image.original.height() as f32,
        );
        // 小さな画像も扱いやすいよう、表示範囲いっぱいまで拡大して見せる。
        let scale = (VIEW_MAX / width).min(VIEW_MAX / height);
        let view = (width * scale, height * scale);
        let mut cropper = Self {
            image,
            view,
            selection: Selection::centered(view.0, view.1),
            grab: Point::default(),
            source_id,
            icons_dir,
            saving: false,
            error: None,
            on_done: Some(on_done),
            preview: None,
        };
        cropper.refresh_preview(window);
        cropper
    }

    /// 枠の範囲からプレビューを作り直す。古いプレビューは GPU から外す。
    fn refresh_preview(&mut self, window: &mut Window) {
        let thumbnail = &self.image.thumbnail;
        let scale = thumbnail.width() as f32 / self.view.0;
        let (x, y, size) = self
            .selection
            .to_source(scale, thumbnail.width(), thumbnail.height());
        let cropped = imageops::crop_imm(thumbnail, x, y, size, size).to_image();
        let pixels = imageops::resize(
            &cropped,
            PREVIEW_PIXELS,
            PREVIEW_PIXELS,
            FilterType::Triangle,
        );
        let preview = Arc::new(RenderImage::new(vec![image::Frame::new(pixels)]));
        if let Some(old) = self.preview.replace(preview) {
            let _ = window.drop_image(old);
        }
    }

    fn on_drag_move(
        &mut self,
        event: &DragMoveEvent<CropDrag>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let relative = event.event.position - event.bounds.origin;
        let (x, y) = (f32::from(relative.x), f32::from(relative.y));
        let (view_width, view_height) = self.view;
        let current = self.selection;
        self.selection = match event.drag(cx) {
            CropDrag::Move => Selection {
                x: x - f32::from(self.grab.x),
                y: y - f32::from(self.grab.y),
                size: current.size,
            }
            .clamp(view_width, view_height, MIN_SELECTION),
            CropDrag::Resize => {
                // 左上を固定して、右下を引っ張った分だけ広げる。
                let limit = (view_width - current.x).min(view_height - current.y);
                let size = (x - current.x)
                    .max(y - current.y)
                    .clamp(MIN_SELECTION.min(limit), limit);
                Selection { size, ..current }
            }
        };
        self.refresh_preview(window);
        cx.notify();
    }

    fn on_scroll(&mut self, event: &ScrollEvent, window: &mut Window, cx: &mut Context<Self>) {
        let delta = f32::from(event.delta.pixel_delta(px(16.)).y);
        if delta == 0.0 {
            return;
        }
        // 中心を保ったまま、上に回すと大きく、下に回すと小さくする。
        let current = self.selection;
        let size = current.size * (1.0 + delta * 0.004);
        let center = (
            current.x + current.size / 2.0,
            current.y + current.size / 2.0,
        );
        self.selection = Selection {
            x: center.0 - size / 2.0,
            y: center.1 - size / 2.0,
            size,
        }
        .clamp(self.view.0, self.view.1, MIN_SELECTION);
        self.refresh_preview(window);
        cx.notify();
    }

    fn reset(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.selection = Selection::centered(self.view.0, self.view.1);
        self.refresh_preview(window);
        cx.notify();
    }

    fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.saving {
            return;
        }
        let scale = self.image.original.width() as f32 / self.view.0;
        let rect = self.selection.to_source(
            scale,
            self.image.original.width(),
            self.image.original.height(),
        );
        let image = self.image.original.clone();
        let icons_dir = self.icons_dir.clone();
        let source_id = self.source_id.clone();
        self.saving = true;
        self.error = None;
        cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { icons::save_cropped(&image, rect, &icons_dir, &source_id) })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.saving = false;
                match result {
                    Ok(path) => {
                        if let Some(on_done) = this.on_done.take() {
                            on_done(path, window, cx);
                        }
                        window.close_dialog(cx);
                    }
                    Err(err) => this.error = Some(format!("{err:#}")),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// 枠の外を暗くする帯。
    fn shade(x: f32, y: f32, width: f32, height: f32) -> impl IntoElement {
        div()
            .absolute()
            .left(px(x))
            .top(px(y))
            .w(px(width.max(0.)))
            .h(px(height.max(0.)))
            .bg(gpui_kit::black().opacity(0.55))
    }
}

impl Render for IconCropper {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (view_width, view_height) = self.view;
        let selection = self.selection;
        let weak = cx.entity().downgrade();
        let (source_x, source_y, source_size) = selection.to_source(
            self.image.original.width() as f32 / view_width,
            self.image.original.width(),
            self.image.original.height(),
        );

        let crop_area = div()
            .id("crop-area")
            .relative()
            .flex_shrink_0()
            .w(px(view_width))
            .h(px(view_height))
            .overflow_hidden()
            .bg(theme.muted)
            .on_drag_move::<CropDrag>(cx.listener(Self::on_drag_move))
            .on_scroll(cx.listener(Self::on_scroll))
            .child(
                img(self.image.display.clone())
                    .absolute()
                    .left_0()
                    .top_0()
                    .w(px(view_width))
                    .h(px(view_height))
                    .object_fit(ObjectFit::Fill),
            )
            .child(Self::shade(0., 0., view_width, selection.y))
            .child(Self::shade(
                0.,
                selection.y + selection.size,
                view_width,
                view_height - selection.y - selection.size,
            ))
            .child(Self::shade(0., selection.y, selection.x, selection.size))
            .child(Self::shade(
                selection.x + selection.size,
                selection.y,
                view_width - selection.x - selection.size,
                selection.size,
            ))
            .child(
                div()
                    .id("crop-selection")
                    .absolute()
                    .left(px(selection.x))
                    .top(px(selection.y))
                    .size(px(selection.size))
                    .border_2()
                    .border_color(gpui_kit::white())
                    .rounded(px(selection.size * 0.12))
                    .cursor(CursorStyle::OpenHand)
                    .on_drag(CropDrag::Move, move |_, offset, _, cx| {
                        let _ = weak.update(cx, |this, _| this.grab = offset);
                        cx.new(|_| DragGhost)
                    }),
            )
            .child(
                div()
                    .id("crop-handle")
                    .absolute()
                    .left(px(selection.x + selection.size - HANDLE / 2.))
                    .top(px(selection.y + selection.size - HANDLE / 2.))
                    .size(px(HANDLE))
                    .rounded_full()
                    .bg(gpui_kit::white())
                    .border_2()
                    .border_color(theme.primary)
                    .cursor(CursorStyle::ResizeUpLeftDownRight)
                    .on_drag(CropDrag::Resize, |_, _, _, cx| cx.new(|_| DragGhost)),
            );

        // 保存後と同じく角丸で見せる。gpui は親の角丸で子を切り抜かないので、
        // 切り抜いた画像そのものを角丸で描く。
        let preview = div()
            .size(px(PREVIEW))
            .flex_shrink_0()
            .rounded(px(PREVIEW * 0.24))
            .bg(theme.muted)
            .when_some(self.preview.clone(), |this, preview| {
                this.child(
                    img(ImageSource::Render(preview))
                        .size_full()
                        .rounded(px(PREVIEW * 0.24)),
                )
            });

        v_flex()
            .gap_3()
            .child(
                div()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .child("枠をドラッグして位置を、右下の丸かマウスホイールで大きさを決めます。"),
            )
            .child(
                h_flex().items_start().gap_4().child(crop_area).child(
                    v_flex()
                        .gap_2()
                        .items_center()
                        .child(preview)
                        .child(
                            div()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child("プレビュー"),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child(format!("{source_size}px 四方")),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child(format!("({source_x}, {source_y})")),
                        ),
                ),
            )
            .when_some(self.error.clone(), |this, error| {
                this.child(div().text_sm().text_color(theme.danger).child(error))
            })
            .child(
                h_flex()
                    .gap_2()
                    .justify_end()
                    .child(
                        Button::new("crop-reset")
                            .ghost()
                            .label("枠を戻す")
                            .on_click(cx.listener(|this, _, window, cx| this.reset(window, cx))),
                    )
                    .child(
                        Button::new("crop-cancel")
                            .outline()
                            .label("キャンセル")
                            .on_click(|_, window, cx| window.close_dialog(cx)),
                    )
                    .child(
                        Button::new("crop-save")
                            .primary()
                            .label("切り抜いて使う")
                            .loading(self.saving)
                            .on_click(cx.listener(|this, _, window, cx| this.save(window, cx))),
                    ),
            )
    }
}

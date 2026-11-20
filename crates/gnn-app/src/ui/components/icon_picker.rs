//! カタログと手元の編集画面で共通のアイコン設定。

use std::sync::Arc;

use anyhow::{Context as _, Result, ensure};
use futures::AsyncReadExt as _;
use gnn_fetch::image_cache::Refreshed;
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Sizable as _, WindowExt as _, h_flex, v_flex,
};
use gpui_kit::http_client::{AsyncBody, HttpClient};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, AppContext as _, Context, Entity, IntoElement, ParentElement as _, Render, RenderOnce,
    SharedString, Styled as _, Window, div,
};

use crate::services::{MAX_IMAGE_BYTES, Services};
use crate::state::AppState;
use crate::ui::icon_cropper::{CropImage, CropTarget, FILE_LABEL};

pub fn icon_buttons(
    app: &Entity<AppState>,
    id: Option<&str>,
    name: &str,
    icon: Option<&str>,
    cx: &App,
) -> impl IntoElement + use<> {
    let saved = id.is_some();
    let has_override = id.is_some_and(|id| app.read(cx).local_icon(id).is_some());
    let id = id.unwrap_or_default().to_owned();
    let name = name.to_owned();
    let app = app.clone();
    let refresh = icon
        .map(str::trim)
        .filter(|url| saved && (url.starts_with("https://") || url.starts_with("http://")))
        .map(|url| RefreshIcon {
            services: app.read(cx).services().clone(),
            name: name.clone(),
            url: url.to_owned(),
        });
    let target = {
        let id = id.clone();
        let app = app.clone();
        move |cx: &App| {
            let app = app.clone();
            let id = id.clone();
            let icons_dir = app.read(cx).services().paths().icons_dir();
            CropTarget::new(
                id.clone(),
                name.clone(),
                icons_dir,
                move |path, window, cx| {
                    let result = app.update(cx, |app, cx| {
                        app.set_icon(&id, Some(path.display().to_string()), cx)
                    });
                    let note = match result {
                        Ok(()) => Notification::success("アイコンを設定しました"),
                        Err(err) => {
                            Notification::error(format!("アイコンを保存できません: {err:#}"))
                        }
                    };
                    window.push_notification(note, cx);
                },
            )
        }
    };
    h_flex()
        .gap_2()
        .flex_wrap()
        .child(
            Button::new("pick-icon-file")
                .small()
                .outline()
                .icon(IconName::ImagePlus)
                .label(FILE_LABEL)
                .disabled(!saved)
                .on_click({
                    let target = target.clone();
                    move |_, window, cx| target(cx).pick_file(window, cx)
                }),
        )
        .child(
            Button::new("pick-icon-url")
                .small()
                .outline()
                .label("URL から")
                .disabled(!saved)
                .on_click(move |_, window, cx| UrlDialog::open(target(cx), window, cx)),
        )
        .when_some(refresh, |row, button| row.child(button))
        .when(has_override, |row| {
            row.child(
                Button::new("clear-icon")
                    .small()
                    .ghost()
                    .label("外す")
                    .on_click(move |_, window, cx| {
                        if let Err(err) = app.update(cx, |app, cx| app.set_icon(&id, None, cx)) {
                            window.push_notification(
                                Notification::error(format!("アイコンを外せません: {err:#}")),
                                cx,
                            );
                        }
                    }),
            )
        })
}

#[derive(IntoElement)]
struct RefreshIcon {
    services: Services,
    name: String,
    url: String,
}

impl RenderOnce for RefreshIcon {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let busy = window.use_keyed_state(
            SharedString::from(format!("refresh-icon:{}", self.url)),
            cx,
            |_, _| false,
        );
        Button::new("refresh-icon")
            .small()
            .outline()
            .icon(IconName::RefreshCw)
            .label("アイコンを再取得")
            .loading(*busy.read(cx))
            .on_click(move |_, window, cx| {
                if *busy.read(cx) {
                    return;
                }
                busy.update(cx, |busy, cx| {
                    *busy = true;
                    cx.notify();
                });
                let receiver = self.services.refresh_icon(self.url.clone());
                let busy = busy.clone();
                let name = self.name.clone();
                window
                    .spawn(cx, async move |cx| {
                        let result = receiver.await;
                        let _ = cx.update(|window, cx| {
                            busy.update(cx, |busy, cx| {
                                *busy = false;
                                cx.notify();
                            });
                            let note = match result {
                                Ok(Ok(Refreshed::Unchanged)) => {
                                    Notification::info(format!("{name} のアイコンは最新です"))
                                }
                                Ok(Ok(_)) => Notification::success(format!(
                                    "{name} のアイコンを更新しました"
                                )),
                                Ok(Err(err)) => Notification::error(format!(
                                    "{name} のアイコンを取得できません: {err:#}"
                                )),
                                Err(_) => return,
                            };
                            window.push_notification(note, cx);
                        });
                    })
                    .detach();
            })
    }
}

struct UrlDialog {
    input: Entity<InputState>,
    target: Option<CropTarget>,
    loading: bool,
    error: Option<String>,
}

impl UrlDialog {
    fn open(target: CropTarget, window: &mut Window, cx: &mut App) {
        let input =
            cx.new(|cx| InputState::new(window, cx).placeholder("https://example.com/icon.png"));
        let view = cx.new(|_| Self {
            input,
            target: Some(target),
            loading: false,
            error: None,
        });
        window.open_dialog(cx, move |dialog, _, _| {
            let dismissed = view.clone();
            dialog
                .title("アイコン画像の URL")
                .child(view.clone())
                .on_close(move |_, _, cx| dismissed.update(cx, |view, _| view.target = None))
        });
    }

    fn load(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.loading {
            return;
        }
        let url = self.input.read(cx).value().trim().to_owned();
        let client = cx.http_client();
        self.loading = true;
        self.error = None;
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { load_url(client, &url).await })
                .await;
            // 閉じたダイアログへの応答では切り抜き画面を開かない。
            let _ = this.update_in(cx, |this, window, cx| {
                this.loading = false;
                match result {
                    Ok(image) => {
                        if let Some(target) = this.target.take() {
                            window.close_dialog(cx);
                            target.open(image, window, cx);
                        }
                    }
                    Err(err) => this.error = Some(format!("{err:#}")),
                }
                cx.notify();
            });
        })
        .detach();
    }
}

impl Render for UrlDialog {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .gap_3()
            .child(Input::new(&self.input).disabled(self.loading))
            .when_some(self.error.clone(), |column, message| {
                column.child(div().text_sm().text_color(cx.theme().danger).child(message))
            })
            .child(
                h_flex()
                    .gap_2()
                    .justify_end()
                    .child(
                        Button::new("cancel-url")
                            .outline()
                            .label("キャンセル")
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.target = None;
                                window.close_dialog(cx);
                            })),
                    )
                    .child(
                        Button::new("load-url")
                            .primary()
                            .label("画像を読み込む")
                            .loading(self.loading)
                            .on_click(cx.listener(|this, _, window, cx| this.load(window, cx))),
                    ),
            )
    }
}

async fn load_url(client: Arc<dyn HttpClient>, input: &str) -> Result<CropImage> {
    let url = url::Url::parse(input).context("画像の URL を入力してください")?;
    gnn_core::net::check_public_url(&url).map_err(anyhow::Error::msg)?;
    ensure!(
        !gnn_fetch::ImageCache::is_internal(url.as_str()),
        "配信元の画像 URL を入力してください"
    );
    // アバターと同じ HTTP クライアントでキャッシュ・名前解決・リダイレクトの検査を通す。
    let uri = crate::http_client::display_uri(url.as_str());
    let response = client
        .get(&uri, AsyncBody::empty(), true)
        .await
        .context("画像を取得できません")?;
    ensure!(
        response.status().is_success(),
        "画像を取得できません (HTTP {})",
        response.status()
    );
    let mut bytes = Vec::new();
    response
        .into_body()
        .take(MAX_IMAGE_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .await?;
    ensure!(bytes.len() <= MAX_IMAGE_BYTES, "画像が大きすぎます");
    let image = image::load_from_memory(&bytes).context("対応する画像形式で読み込めません")?;
    CropImage::new(image)
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::future::BoxFuture;
    use gpui_kit::http_client::{Request, Response, http};

    struct ImageResponse(&'static [u8]);

    impl HttpClient for ImageResponse {
        fn user_agent(&self) -> Option<&http::HeaderValue> {
            None
        }

        fn proxy(&self) -> Option<&url::Url> {
            None
        }

        fn send(
            &self,
            request: Request<AsyncBody>,
        ) -> BoxFuture<'static, Result<Response<AsyncBody>>> {
            // 拡張子のない URL。問い合わせは画像を特定するために必要で、フラグメントは送らない。
            assert_eq!(request.uri(), "https://example.com/art?id=1");
            let body = AsyncBody::from(self.0);
            Box::pin(async move { Ok(Response::new(body)) })
        }
    }

    #[tokio::test]
    async fn loads_image_bytes_and_rejects_pages_and_non_public_urls() {
        let url = "https://example.com/art?id=1#preview";
        let icon = Arc::new(ImageResponse(include_bytes!(
            "../../../assets/icons/app.ico"
        )));
        assert!(load_url(icon.clone(), url).await.is_ok());
        assert!(
            load_url(Arc::new(ImageResponse(b"<html></html>")), url)
                .await
                .is_err()
        );
        for invalid in [
            "",
            "file:///C:/icon.png",
            "http://127.0.0.1/art",
            "https://legacy-image.invalid/art",
        ] {
            assert!(load_url(icon.clone(), invalid).await.is_err());
        }
    }
}

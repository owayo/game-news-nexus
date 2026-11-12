#!/usr/bin/env bash
# scripts/bundle-macos.sh を、アプリの代わりの小さな実行ファイルで確かめる (CI と手元。mise run test-bundle)。
#
# Rust のビルドを待たずに、.app の中身 (Info.plist の値・アイコン・ライセンス)・ad-hoc 署名・DMG を確かめる。
# 作るものは一時フォルダに置いて最後に消す (/Applications やキーチェーンは触らない)。
# macOS 標準の bash 3.2 で動く書き方に限る。
set -euo pipefail

app_name='Game News Nexus'
bundle_id=io.github.owayo.game-news-nexus
licenses_dir=crates/gnn-app/assets/licenses

fail() {
    printf '検査に失敗しました: %s\n' "$*" >&2
    exit 1
}

project_root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
cd -- "$project_root"
[[ $(uname -s) == Darwin ]] || fail 'macOS で実行してください (Windows のインストーラーは CI の mise run test-installer で確かめます)'

work=$(mktemp -d "${TMPDIR:-/tmp}/gnn-bundle-test.XXXXXX")
trap 'rm -rf -- "$work"' EXIT

# アプリの代わり。--help を含め、どんな引数でも 0 で終わる
printf '%s\n' '#include <stdio.h>' 'int main(void) { puts("fixture"); return 0; }' >"$work/main.c"
mkdir -p "$work/bin"
cc -arch arm64 -mmacosx-version-min=11.0 -o "$work/bin/game-news-nexus" "$work/main.c"

version=$(awk -F '"' '
    /^\[/ { in_package = ($0 ~ /^\[workspace\.package\][[:space:]]*(#.*)?$/); next }
    in_package && /^version[[:space:]]*=/ { print $2; exit }
' Cargo.toml)
app="$work/bin/$app_name.app"
dmg="$work/dist/game-news-nexus-$version-macos-arm64.dmg"
plist="$app/Contents/Info.plist"

# 2 回目は、1 回目の .app を置き換える道筋を通る
for round in 1 2; do
    CODESIGN_IDENTITY=- bash scripts/bundle-macos.sh --binary "$work/bin/game-news-nexus" --dmg --out-dir "$work/dist"
    [[ -d $app ]] || fail "$round 回目: .app ができていません"
    [[ -s $dmg ]] || fail "$round 回目: DMG ができていません: $dmg"
done
[[ -z $(find "$work/bin" -name '.bundle-macos.*' -print -quit) ]] || fail '作業用のフォルダが残っています'

plist_get() {
    /usr/libexec/PlistBuddy -c "Print :$1" "$plist"
}
[[ $(plist_get CFBundleIdentifier) == "$bundle_id" ]] || fail 'CFBundleIdentifier'
[[ $(plist_get CFBundleExecutable) == game-news-nexus ]] || fail 'CFBundleExecutable'
[[ $(plist_get CFBundleName) == "$app_name" ]] || fail 'CFBundleName'
[[ $(plist_get CFBundleShortVersionString) == "$version" ]] || fail 'CFBundleShortVersionString が Cargo.toml の版ではありません'
[[ $(plist_get CFBundleVersion) == "$version" ]] || fail 'CFBundleVersion が Cargo.toml の版ではありません'
[[ $(plist_get LSMinimumSystemVersion) == 11.0 ]] || fail 'LSMinimumSystemVersion が実行ファイルの minos (11.0) ではありません'
[[ -x "$app/Contents/MacOS/game-news-nexus" ]] || fail '実行ファイル'
cmp -s crates/gnn-app/assets/icons/app.icns "$app/Contents/Resources/app.icns" || fail 'アイコン (app.icns)'
cmp -s LICENSE "$app/Contents/Resources/LICENSE" || fail 'LICENSE'
for license in "$licenses_dir"/*; do
    [[ -f $license ]] || continue
    cmp -s "$license" "$app/Contents/Resources/licenses/$(basename -- "$license")" ||
        fail "第三者のライセンスが入っていません: $(basename -- "$license")"
done

codesign --verify --strict "$app" || fail '署名'
signature=$(codesign -dv "$app" 2>&1)
[[ $signature == *"Identifier=$bundle_id"* ]] || fail "署名の Identifier が $bundle_id ではありません"
[[ $signature == *'Signature=adhoc'* ]] || fail 'ad-hoc 署名ではありません'
for key in LSUIElement LSBackgroundOnly NSSupportsAutomaticTermination; do
    if plist_get "$key" >/dev/null 2>&1; then
        fail "Info.plist に $key があります (Dock の表示はアプリが切り替える)"
    fi
done

# Info.plist を埋め込んだ実行ファイル (-sectcreate __TEXT __info_plist) は断る
mkdir -p "$work/embedded"
cp packaging/macos/Info.plist "$work/embedded.plist"
cc -arch arm64 -mmacosx-version-min=11.0 -Wl,-sectcreate,__TEXT,__info_plist,"$work/embedded.plist" \
    -o "$work/embedded/game-news-nexus" "$work/main.c"
if CODESIGN_IDENTITY=- bash scripts/bundle-macos.sh --binary "$work/embedded/game-news-nexus" >/dev/null 2>&1; then
    fail 'Info.plist を埋め込んだ実行ファイルから .app を作ってしまいました'
fi
[[ ! -e "$work/embedded/$app_name.app" ]] || fail 'Info.plist を埋め込んだ実行ファイルの .app が残っています'

printf '%s\n' "macOS の .app と DMG の検査が通りました ($dmg)"

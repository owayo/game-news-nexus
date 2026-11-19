#!/usr/bin/env bash
# macOS のアプリ (Game News Nexus.app) を作って署名する。--dmg を付けると配布用の DMG も作る。
#
# 使い方: scripts/bundle-macos.sh [--target <ターゲット>] [--binary <実行ファイル>] [--app <.app>]
#                                 [--dmg] [--out-dir <フォルダ>]
#   --target   cargo build --release --target <ターゲット> の出力 (<target>/<ターゲット>/release) を使う
#   --binary   使う実行ファイルを直接指定する。既定は <target>/release/game-news-nexus (mise run release の出力)
#   --app      作る .app。既定は実行ファイルと同じフォルダの "Game News Nexus.app"
#   --dmg      /Applications へのリンクを入れた DMG も作る
#   --out-dir  DMG を置くフォルダ。既定は <target>/dist。名前は game-news-nexus-<版>-macos-arm64.dmg
# <target> は cargo metadata の target_directory (CARGO_TARGET_DIR などで場所を変えていても追う)。
#
# 版は Cargo.toml の [workspace.package] の version、Info.plist の LSMinimumSystemVersion は実行ファイルの
# 最小の macOS (LC_BUILD_VERSION の minos) で埋める。署名は環境変数 CODESIGN_IDENTITY の ID、なければ
# キーチェーンにある名前が「… local dev」の署名用 ID、それもなければ ad-hoc (-)。配布物は ad-hoc で作る
# (Release のワークフローは CODESIGN_IDENTITY=- を渡す)。Apple の公証 (notarization) はしない。
#
# 古い .app は、新しい .app を別の場所で作って確かめてから置き換える (署名済みの実行ファイルをその場で
# 上書きすると、macOS が覚えている署名と食い違って起動できなくなるため)。
# macOS 標準の bash 3.2 で動く書き方に限る (GitHub の macOS のランナーも /bin/bash は 3.2)。
set -euo pipefail

app_name='Game News Nexus'
binary_name=game-news-nexus
bundle_id=io.github.owayo.game-news-nexus
licenses_dir=crates/gnn-app/assets/licenses

die() {
    printf '%s\n' "$*" >&2
    exit 1
}

usage() {
    sed -n '2,/^set -euo/p' "${BASH_SOURCE[0]}" | sed -e '$d' -e 's/^# \{0,1\}//'
}

# 引数のパスは、呼び出したときのフォルダからの相対パスとして読む
start_dir=$(pwd)
absolute() {
    case "$1" in
        /*) printf '%s\n' "$1" ;;
        *) printf '%s\n' "$start_dir/$1" ;;
    esac
}

target=
binary=
app=
make_dmg=false
out_dir=
while (($# > 0)); do
    case "$1" in
        --target) target=${2:?--target にターゲットを指定してください}; shift 2 ;;
        --binary) binary=$(absolute "${2:?--binary に実行ファイルを指定してください}"); shift 2 ;;
        --app) app=$(absolute "${2:?--app に作る .app を指定してください}"); shift 2 ;;
        --dmg) make_dmg=true; shift ;;
        --out-dir) out_dir=$(absolute "${2:?--out-dir にフォルダを指定してください}"); shift 2 ;;
        -h | --help) usage; exit 0 ;;
        *) die "不明な引数です: $1 (scripts/bundle-macos.sh --help)" ;;
    esac
done

project_root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
cd -- "$project_root"

[[ $(uname -s) == Darwin ]] || die 'macOS で実行してください (Windows のインストーラーは mise run installer で作ります)'

# cargo の出力先。cargo が使えなければ ./target とみなす
target_directory() {
    local dir
    dir=$(cargo metadata --format-version 1 --no-deps 2>/dev/null |
        sed -n 's/.*"target_directory":"\([^"]*\)".*/\1/p' || true)
    printf '%s\n' "${dir:-$project_root/target}"
}

if [[ -z $binary ]]; then
    if [[ -n $target ]]; then
        binary="$(target_directory)/$target/release/$binary_name"
    else
        binary="$(target_directory)/release/$binary_name"
    fi
fi
[[ -f $binary && -x $binary ]] ||
    die "実行ファイルがありません: $binary (先に mise run release か cargo build --release -p gnn-app を実行してください)"
binary=$(cd -- "$(dirname -- "$binary")" && pwd)/$(basename -- "$binary")
app=${app:-$(dirname -- "$binary")/$app_name.app}

version=$(awk -F '"' '
    /^\[/ { in_package = ($0 ~ /^\[workspace\.package\][[:space:]]*(#.*)?$/); next }
    in_package && /^version[[:space:]]*=/ { print $2; exit }
' Cargo.toml)
[[ $version =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || die "Cargo.toml の [workspace.package] の version を読めません: '$version'"

# 配るのは Apple Silicon (arm64) だけ
archs=$(lipo -archs "$binary" 2>/dev/null || true)
[[ " $archs " == *" arm64 "* ]] || die "Apple Silicon (arm64) 向けの実行ファイルではありません: $binary (${archs:-不明})"
# 実行ファイルのロードコマンド。最小の macOS と、Info.plist が埋め込まれていないかを読む
load_commands=$(otool -arch arm64 -l "$binary" 2>/dev/null || true)
min_macos=$(awk '
    $1 == "cmd" { cmd = $2 }
    found { next }
    cmd == "LC_BUILD_VERSION" && $1 == "minos" { print $2; found = 1 }
    cmd == "LC_VERSION_MIN_MACOSX" && $1 == "version" { print $2; found = 1 }
' <<<"$load_commands")
[[ $min_macos =~ ^[0-9]+(\.[0-9]+)*$ ]] || die "実行ファイルの最小の macOS を読めません: $binary"
# Info.plist は .app に置くだけで、実行ファイルには埋め込まない (-sectcreate __TEXT __info_plist や embed_plist)。
# 埋め込むと、.app の外で起動したときに通知を止める gpui の判定が効かず、通知の初期化でアプリが落ちる
[[ $load_commands != *"sectname __info_plist"* ]] ||
    die "実行ファイルに Info.plist が埋め込まれています (__TEXT,__info_plist): $binary"

# 第三者のライセンス (Rust のクレートの一覧と IPAdic の辞書など) は配布物に必ず入れる
has_licenses=false
for license in "$licenses_dir"/*; do
    if [[ -f $license ]]; then
        has_licenses=true
        break
    fi
done
$has_licenses || die "同梱する第三者のライセンスがありません: $licenses_dir"
# 実行ファイルに組み込んだクレートのライセンスの一覧 (mise run licenses が作り、CI が古くないかを確かめる)
[[ -s $licenses_dir/THIRD_PARTY_LICENSES.md ]] ||
    die "Rust のクレートのライセンスの一覧がありません: $licenses_dir/THIRD_PARTY_LICENSES.md (mise run licenses で作ってください)"

identity=${CODESIGN_IDENTITY:-}
if [[ -z $identity ]]; then
    identity=$(security find-identity -v -p codesigning 2>/dev/null | awk -F '"' '/ local dev"/ { print $2; exit }' || true)
fi
identity=${identity:--}
if [[ $identity == - ]]; then
    printf '%s\n' 'ad-hoc 署名で作ります (公証していないので、ほかの Mac では初回の起動に許可が要ります)'
else
    printf '署名: %s\n' "$identity"
fi

plist_get() {
    /usr/libexec/PlistBuddy -c "Print :$1" "$2"
}

plist_set() {
    /usr/libexec/PlistBuddy -c "Set :$1 $2" "$3"
}

app_parent=$(dirname -- "$app")
mkdir -p -- "$app_parent"
staging=$(mktemp -d "$app_parent/.bundle-macos.XXXXXX")
mount_dir=

detach() {
    local attempt
    for attempt in 1 2 3 4 5; do
        if hdiutil detach -quiet "$1"; then
            return 0
        fi
        sleep "$attempt"
    done
    hdiutil detach -force -quiet "$1"
}

cleanup() {
    if [[ -n $mount_dir ]]; then
        detach "$mount_dir" || true
    fi
    rm -rf -- "$staging"
}
trap cleanup EXIT

new_app="$staging/$app_name.app"
contents="$new_app/Contents"
mkdir -p "$contents/MacOS" "$contents/Resources/licenses"
install -m 755 "$binary" "$contents/MacOS/$binary_name"
cp packaging/macos/Info.plist "$contents/Info.plist"
plist_set CFBundleShortVersionString "$version" "$contents/Info.plist"
plist_set CFBundleVersion "$version" "$contents/Info.plist"
plist_set LSMinimumSystemVersion "$min_macos" "$contents/Info.plist"
plutil -lint -s "$contents/Info.plist" || die "Info.plist の書き方が正しくありません: $contents/Info.plist"
[[ $(plist_get CFBundleIdentifier "$contents/Info.plist") == "$bundle_id" ]] ||
    die "packaging/macos/Info.plist の CFBundleIdentifier が $bundle_id ではありません"
[[ $(plist_get CFBundleExecutable "$contents/Info.plist") == "$binary_name" ]] ||
    die "packaging/macos/Info.plist の CFBundleExecutable が $binary_name ではありません"
# Dock の表示はアプリが自分で切り替える (ウィンドウがあれば Regular、なければ Accessory。crates/gnn-app/src/macos.rs)
# ので、Dock から隠すキーや、システムに黙って終わらせるキーは入れない
for key in LSUIElement LSBackgroundOnly NSSupportsAutomaticTermination; do
    if plist_get "$key" "$contents/Info.plist" >/dev/null 2>&1; then
        die "packaging/macos/Info.plist に $key を入れないでください (Dock の表示はアプリが切り替えます)"
    fi
done
# アプリのコードと同じ値か。自動起動の LaunchAgent はバンドル ID を名前に使い、メニューはアプリ名を使う
code_label=$(sed -n 's/^[[:space:]]*pub const LABEL: &str = "\([^"]*\)";.*$/\1/p' crates/gnn-app/src/autostart.rs | sed -n '1p')
[[ $code_label == "$bundle_id" ]] ||
    die "crates/gnn-app/src/autostart.rs の LaunchAgent の LABEL (${code_label:-見つかりません}) が Info.plist の CFBundleIdentifier ($bundle_id) と違います"
code_app_name=$(sed -n 's/^[[:space:]]*pub const APP_NAME: &str = "\([^"]*\)";.*$/\1/p' crates/gnn-app/src/shortcut.rs | sed -n '1p')
[[ $code_app_name == "$(plist_get CFBundleName "$contents/Info.plist")" ]] ||
    die "crates/gnn-app/src/shortcut.rs の APP_NAME (${code_app_name:-見つかりません}) が Info.plist の CFBundleName と違います"
cp crates/gnn-app/assets/icons/app.icns "$contents/Resources/app.icns"
cp LICENSE "$contents/Resources/LICENSE"
cp -R "$licenses_dir/." "$contents/Resources/licenses/"
# 拡張属性 (取得したファイルの quarantine など) が残っていると codesign が断る
xattr -cr "$new_app"

codesign --force --timestamp=none --sign "$identity" "$new_app"
codesign --verify --strict --verbose=2 "$new_app"
# 署名した実行ファイルが起動できるか (--help は画面を出さずに説明を出して終わる)
"$contents/MacOS/$binary_name" --help >/dev/null ||
    die "作ったアプリの実行ファイルを起動できません: $contents/MacOS/$binary_name"

rm -rf -- "$app"
mv -- "$new_app" "$app"
printf 'アプリ: %s (版 %s、macOS %s 以降)\n' "$app" "$version" "$min_macos"

if ! $make_dmg; then
    exit 0
fi

out_dir=${out_dir:-$(target_directory)/dist}
mkdir -p -- "$out_dir"
dmg="$(cd -- "$out_dir" && pwd)/$binary_name-$version-macos-arm64.dmg"
dmg_root="$staging/dmg"
mkdir -p "$dmg_root"
ditto "$app" "$dmg_root/$app_name.app"
ln -s /Applications "$dmg_root/Applications"
new_dmg="$staging/$binary_name.dmg"
# GitHub の macOS のランナーでは hdiutil create がときどき "Resource busy" で落ちるので、間を空けて試し直す
attempt=1
until hdiutil create -quiet -volname "$app_name" -srcfolder "$dmg_root" -fs HFS+ -format UDZO -ov "$new_dmg"; do
    ((attempt < 5)) || die 'DMG を作れません (hdiutil create)'
    printf 'hdiutil create に失敗しました (%d 回目)。%d 秒待って試し直します\n' "$attempt" "$((attempt * 5))" >&2
    sleep $((attempt * 5))
    attempt=$((attempt + 1))
done
hdiutil verify -quiet "$new_dmg"

# 開いた DMG の中身が、署名の通るアプリと /Applications へのリンクだけか
mount_point=$(mktemp -d "$staging/mount.XXXXXX")
hdiutil attach -quiet -nobrowse -readonly -noautoopen -mountpoint "$mount_point" "$new_dmg"
mount_dir=$mount_point
codesign --verify --strict "$mount_dir/$app_name.app"
[[ -L "$mount_dir/Applications" && $(readlink "$mount_dir/Applications") == /Applications ]] ||
    die 'DMG に /Applications へのリンクがありません'
entries=$(cd -- "$mount_dir" && find . -mindepth 1 -maxdepth 1 ! -name '.*' | LC_ALL=C sort | tr '\n' ' ')
[[ $entries == "./Applications ./$app_name.app " ]] || die "DMG の中身が想定と違います: $entries"
detach "$mount_dir"
mount_dir=

mv -f -- "$new_dmg" "$dmg"
printf 'DMG: %s\n' "$dmg"
shasum -a 256 "$dmg"

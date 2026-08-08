# デスクトップアプリ

`minato-desktop` は、メニューバーに常駐する GPUI 製の小さなアプリケーション
です。稼働中の環境、その URL、ログを確認できます。

常時開いておくことは想定していません。現在の状況を確認し、必要な環境を開く
ためのものです。

## 起動

```console
$ cargo build --release -p minato-desktop
$ ./target/release/minato-desktop
```

## ビルド

GUI は CLI より前提条件が多く、ビルドも複雑です。

**Xcode Command Line Tools があれば十分です。** 完全な Xcode は不要です。
`runtime_shaders` を有効にしているため、Metal のシェーダは実行時にコンパイル
されます。

bindgen がシステムヘッダを見つけられない場合は、`PATH` の先頭に別のツール
チェーンが存在しています。WASI SDK が典型的な原因です。

```console
$ export PATH=$(echo $PATH | tr ':' '\n' | grep -v wasi-sdk | paste -sd: -)
$ unset WASI_SDK_PATH
$ export LIBCLANG_PATH=/Library/Developer/CommandLineTools/usr/lib
$ cargo build -p minato-desktop
```

症状は CoreMedia などのフレームワークが見つからないというもので、原因は
macOS のフレームワークを認識しない clang が使われていることです。

## 画面構成

- **workspace のサイドバー。** 各サービスの状態が継続的に更新されます
- **詳細ペイン。** URL のコピーとブラウザでの表示、起動・停止ボタン
- **ログビューア。** 選択中の workspace のログを表示します
- **メニューバーのアイコン。** メニューから稼働中のサービスに直接遷移できます

システムのライト / ダーク設定に追従します。タイトルバーから手動で切り替える
こともできます。

## 実装していない機能

**daemon の起動は行いません。** daemon の管理は launchd の役割であり、GUI が
重複して管理すると責務が分散します。接続できない旨が表示された場合は、CLI から
daemon を起動するか、LaunchDaemon を設定して常時稼働させてください。

CLI と同じ daemon API を参照しているため、一方で確認できる情報はもう一方でも
確認できます。GUI のみが保持する状態はありません。

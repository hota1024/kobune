# デスクトップアプリ

`minato-desktop` はメニューバーに常駐する小さな GPUI アプリです。どの環境が
動いているか、URL、ログを見られます。

常時開いておくものではありません。今の状況を確認して、開くためのものです。

## 起動する

```console
$ cargo build --release -p minato-desktop
$ ./target/release/minato-desktop
```

## ビルドする

GUI は CLI より要求が多く、ビルドも面倒です。

**Xcode Command Line Tools で足ります。** 完全な Xcode は不要です。
`runtime_shaders` を有効にしているので、Metal のシェーダは実行時に
コンパイルされます。

bindgen がシステムヘッダを見つけられない場合、`PATH` の先頭に別のものが
います。WASI SDK が典型です。

```console
$ export PATH=$(echo $PATH | tr ':' '\n' | grep -v wasi-sdk | paste -sd: -)
$ unset WASI_SDK_PATH
$ export LIBCLANG_PATH=/Library/Developer/CommandLineTools/usr/lib
$ cargo build -p minato-desktop
```

症状は CoreMedia などが見つからないというもので、原因は macOS のフレーム
ワークを知らない clang です。

## 何が見えるか

- **workspace のサイドバー。** 各サービスの状態が常時更新されます
- **詳細ペイン。** URL のコピーとブラウザで開く、起動・停止ボタン
- **ログビューア。** 選んだ workspace のもの
- **メニューバーのアイコン。** メニューから動いているサービスに直接飛べます

システムのライト／ダーク設定に追従し、タイトルバーから手で切り替えることも
できます。

## しないこと

**daemon を起動しません。** daemon の面倒を見るのは launchd の仕事で、GUI が
二重に管理すると責務が重なります。繋がらないと表示されたら CLI から daemon を
起動するか、LaunchDaemon を設置して常に動くようにしてください。

CLI と同じ daemon API を読んでいるので、片方で見えるものはもう片方でも
見えます。GUI だけが知っている状態はありません。

# ビルド

[概要](README.md) · [ビルド](BUILD.md)

Windows x64、Visual Studio C++ Build Tools、Rust、WebView2 Runtime が必要です。

ビルドスクリプトは、このリポジトリの親ディレクトリに `JIZURA-main`（[JIZURA](https://github.com/852wa/JIZURA)）、`aviutl2-rs-main`（[aviutl2-rs](https://github.com/sevenc-nanashi/aviutl2-rs)）、`sdk`（AviUtl2 Plugin SDK）、`webview2`（WebView2 SDK）を配置する構成です。`build.ps1` は同じ親ディレクトリにある `.cargo`、`.rustup`、Windows SDK のローカルコピーを使用します。

```text
作業ディレクトリ/
├─ JIZURA-AviUtl2/
├─ JIZURA-main/
├─ aviutl2-rs-main/
├─ sdk/
├─ webview2/
├─ .cargo/
├─ .rustup/
├─ windows-sdk/
└─ windows-sdk-x64/
```

```powershell
cd JIZURA-AviUtl2
.\build.cmd
.\package.ps1
```

ビルドしたプラグインは `dist/JIZURA.aux2`、配布 ZIP は `release/JIZURA-AviUtl2-v0.3.1.zip` に作成されます。配布 ZIP の利用者には、これらのビルド用ディレクトリは必要ありません。

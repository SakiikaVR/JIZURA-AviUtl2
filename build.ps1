$ErrorActionPreference='Stop'
$root=Split-Path $PSScriptRoot -Parent
$upstream=Join-Path $PSScriptRoot 'upstream'
$out=Join-Path $PSScriptRoot 'dist'
$web=Join-Path $out 'web'
New-Item -ItemType Directory -Force $web | Out-Null
$utf8=New-Object Text.UTF8Encoding($false)
$scripts=Get-ChildItem (Join-Path $upstream 'src/*.js') | Sort-Object Name | ForEach-Object {
    $s=[IO.File]::ReadAllText($_.FullName)
    if($_.Name -eq '08_planner.js') {
        if(-not $s.Contains('  extra: false,')) { throw 'JIZURA default extra setting was not found' }
        $s=$s.Replace('  extra: false,','  extra: true,')
        if(-not $s.Contains('offset: 0.4,') -or -not $s.Contains('T.offset ?? 0.4')) { throw 'JIZURA default start time was not found' }
        $s=$s.Replace('offset: 0.4,','offset: 0,').Replace('T.offset ?? 0.4','T.offset ?? 0')
    }
    if($_.Name -eq '12_ui.js') {
        if(-not $s.Contains('S.project.timing.offset ?? 0.4')) { throw 'JIZURA start-time field was not found' }
        $s=$s.Replace('S.project.timing.offset ?? 0.4','S.project.timing.offset ?? 0')
        $s=$s.Replace('J.ui = S;', 'J.ui = S; J.aviSetProject = (p,a) => { pause(); S.audio=a; S.project=mergeProject(p); syncUI(); replan(); S.t=0; S.need=true; };')
    }
    if($_.Name -eq '02_fonts.js') {
        $s=$s.Replace('let _pid = 0;', 'let _pid = 0; J.aviResetGlyphs = () => { _pid=0; J.glyphs.clear(); J.metrics.clear(); };')
    }
    $s
}
$js=$scripts -join "`n"
$bridge=[IO.File]::ReadAllText((Join-Path $PSScriptRoot 'bridge.js'))
$css=[IO.File]::ReadAllText((Join-Path $upstream 'app/style.css'))
$body=[IO.File]::ReadAllText((Join-Path $upstream 'app/body.html'))
$exportPanel=[regex]::new('<div class="easy-sec">(?=\s*<h3>[^<]*</h3>\s*<div class="fields">)')
if($exportPanel.Matches($body).Count -ne 1) { throw 'JIZURA easy export panel was not found' }
$body=$exportPanel.Replace($body,'<div class="easy-sec" id="aviExportPanel" hidden inert>',1)
$body=$body.Replace('<button id="btnAE"','<button id="btnAE" hidden inert')
$body=$body.Replace('<button role="tab" data-tab="out"','<button role="tab" data-tab="out" hidden inert')
$body=$body.Replace('<div class="tabpane" data-pane="out" hidden>','<div class="tabpane" data-pane="out" hidden inert>')
$css += "`n#btnAE,#aviExportPanel,.tabs [data-tab='out'],.tabpane[data-pane='out']{display:none!important}.tabs{grid-template-columns:repeat(3,1fr)}`n"
$diagnostics="<script>window.addEventListener('error',function(e){var w=window.chrome&&chrome.webview;if(w)w.postMessage(JSON.stringify({type:'error',error:'UI error: '+(e.message||'unknown')+' ('+(e.filename||'')+':'+(e.lineno||0)+')'}));});window.addEventListener('unhandledrejection',function(e){var w=window.chrome&&chrome.webview;if(w)w.postMessage(JSON.stringify({type:'error',error:'UI promise error: '+String(e.reason&&e.reason.stack||e.reason)}));});</script>"
$mux=[IO.File]::ReadAllText((Join-Path $upstream 'vendor/mp4-muxer.min.js'))
[IO.File]::WriteAllText((Join-Path $web 'engine.js'),$js,$utf8)
[IO.File]::WriteAllText((Join-Path $web 'bridge.js'),$bridge,$utf8)
$engineVersion=(Get-FileHash (Join-Path $web 'engine.js') -Algorithm SHA256).Hash.Substring(0,12)
$bridgeVersion=(Get-FileHash (Join-Path $web 'bridge.js') -Algorithm SHA256).Hash.Substring(0,12)
[IO.File]::WriteAllText((Join-Path $web 'editor.html'),"<!doctype html><html lang='ja'><meta charset='utf-8'><style>$css</style><body>$diagnostics$body<script>$mux</script><script src='engine.js?v=$engineVersion'></script><script src='bridge.js?v=$bridgeVersion'></script></body></html>",$utf8)
[IO.File]::WriteAllText((Join-Path $web 'render.html'),"<!doctype html><meta charset='utf-8'><script src='engine.js?v=$engineVersion'></script><script src='bridge.js?v=$bridgeVersion'></script>",$utf8)
Copy-Item (Join-Path $upstream 'LICENSE') (Join-Path $out 'LICENSE-JIZURA.txt')
Copy-Item (Join-Path $upstream 'THIRD_PARTY_NOTICES.md') $out
Copy-Item (Join-Path $upstream 'vendor/LICENSE.mp4-muxer.txt') $out
Copy-Item (Join-Path $root 'sdk/license.txt') (Join-Path $out 'LICENSE-AviUtl2-SDK.txt')
Copy-Item (Join-Path $root 'aviutl2-rs-main/LICENSE') (Join-Path $out 'LICENSE-aviutl2-rs.txt')
Copy-Item (Join-Path $root 'webview2/LICENSE.txt') (Join-Path $out 'LICENSE-WebView2.txt')
$env:RUSTUP_HOME=Join-Path $root '.rustup'
$env:CARGO_HOME=Join-Path $root '.cargo'
$sdkInclude=Join-Path $root 'windows-sdk/c/Include/10.0.26100.0'
$env:INCLUDE="$sdkInclude\ucrt;$sdkInclude\shared;$sdkInclude\um;$sdkInclude\winrt;"+$env:INCLUDE
$env:LIB="$root\windows-sdk-x64\c\um\x64;$root\windows-sdk-x64\c\ucrt\x64;"+$env:LIB
& (Join-Path $root '.cargo/bin/cargo.exe') build --release --offline --manifest-path (Join-Path $PSScriptRoot 'Cargo.toml')
if($LASTEXITCODE -ne 0) { throw 'Rust build failed' }
Copy-Item (Join-Path $PSScriptRoot 'target/release/jizura_aviutl2.dll') (Join-Path $out 'JIZURA.aux2') -Force

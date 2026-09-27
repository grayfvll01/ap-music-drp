# Live check of the tray menu: clicks every setting in the running app (via
# WM_COMMAND) and checks the next update sent to Discord reflects it, then
# restores it. Needs `log = true` in config.ini and a song playing:
#   powershell -ExecutionPolicy Bypass -File scripts\test-menu.ps1
$ErrorActionPreference = 'Stop'
Add-Type -Namespace W -Name U -MemberDefinition @'
[DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern IntPtr FindWindowW(string c, IntPtr n);
[DllImport("user32.dll")] public static extern bool PostMessageW(IntPtr h, uint m, IntPtr w, IntPtr l);
'@
$dir = Join-Path $env:APPDATA 'AppleMusicDiscordPresence'
$log = Join-Path $dir 'log.txt'
$cfg = Join-Path $dir 'config.ini'
$h = [W.U]::FindWindowW("AppleMusicDiscordPresence", [IntPtr]::Zero)
if ($h -eq [IntPtr]::Zero) { throw 'app window not found' }

function Sets { @(Get-Content $log | Where-Object { $_ -match '\] set: ' }) }
function Setting($key) { $m = Select-String -Path $cfg -Pattern "^$key = (.*)$"; if ($m) { $m.Matches[0].Groups[1].Value } else { '(default)' } }
# Clicks menu item $id, then waits (up to 30 s: Discord allows 5 updates per
# 20 s) for an update to Discord that passes $test.
function ClickExpect($id, $what, [scriptblock]$test) {
    $n = (Sets).Count
    [void][W.U]::PostMessageW($h, 0x0111, [IntPtr]$id, [IntPtr]0)
    $deadline = (Get-Date).AddSeconds(30)
    while ((Get-Date) -lt $deadline) {
        Start-Sleep -Milliseconds 250
        foreach ($l in @(Sets | Select-Object -Skip $n)) { if (& $test $l) { "  ok   $what"; return } }
    }
    "  FAIL $what"; $script:fail++
}
$fail = 0
if ((Sets).Count -eq 0) { throw 'nothing sent to Discord yet (is Apple Music playing?)' }

$cases = @(
    @{ id = 20; key = 'artwork';         a = { param($l) $l -match 'Purple211' };             b = { param($l) $l -match 'large_image' -and $l -notmatch 'Purple211' } },
    @{ id = 21; key = 'show_progress';   a = { param($l) $l -notmatch 'timestamps' };         b = { param($l) $l -match 'timestamps' } },
    @{ id = 23; key = 'links';           a = { param($l) $l -notmatch 'details_url' };        b = { param($l) $l -match 'details_url' } },
    @{ id = 24; key = 'button_listen';   a = { param($l) $l -match 'Listen on Apple Music' }; b = { param($l) $l -notmatch 'buttons' } },
    @{ id = 25; key = 'button_songlink'; a = { param($l) $l -match 'song\.link/i/' };         b = { param($l) $l -notmatch 'buttons' } }
)
foreach ($c in $cases) {
    $before = Setting $c.key
    ClickExpect $c.id "$($c.key) switched from $before" $c.a
    ClickExpect $c.id "$($c.key) switched back" $c.b
    if ((Setting $c.key) -ne $before) { "  FAIL $($c.key) not restored"; $fail++ }
}

# Switches whose effect needs a paused song or the network: check the file.
foreach ($c in @(@{ id = 22; key = 'show_paused' }, @{ id = 26; key = 'update_check' })) {
    $before = Setting $c.key
    [void][W.U]::PostMessageW($h, 0x0111, [IntPtr]$c.id, [IntPtr]0); Start-Sleep 1
    $mid = Setting $c.key
    [void][W.U]::PostMessageW($h, 0x0111, [IntPtr]$c.id, [IntPtr]0); Start-Sleep 1
    $after = Setting $c.key
    if ($mid -ne $before -and $mid -in 'true', 'false' -and $after -ne $mid) { "  ok   $($c.key): $before -> $mid -> $after" } else { "  FAIL $($c.key): $before -> $mid -> $after"; $fail++ }
}

foreach ($i in 0, 1, 2) {
    ClickExpect (40 + $i) "status text choice $i" ([scriptblock]::Create("param(`$l) `$l -match '""status_display_type"":$i,'"))
}

ClickExpect 10 'Show on Discord off: cleared' { param($l) $l -match '\(cleared\)' }
ClickExpect 10 'Show on Discord on: back' { param($l) $l -match '"details"' }

"settings after the test:"
Select-String -Path $cfg -Pattern '^(artwork|show_progress|show_paused|links|button_listen|button_songlink|update_check|status_display) = ' | ForEach-Object { "  " + $_.Line }
if ($fail) { "FAILED: $fail"; exit 1 } else { "ALL OK" }

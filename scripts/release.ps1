<#
Publishes a new version:

    scripts\release.ps1 1.2.3

1. Add a "## 1.2.3 - <date>" section to CHANGELOG.md (it becomes the release notes).
2. Run this script. It sets the version in Cargo.toml, runs the tests, commits,
   tags v1.2.3 and pushes.

GitHub Actions then builds the installer and publishes the release, and installed
copies of the app offer the update.
#>
param([Parameter(Mandatory)][string]$Version)
$ErrorActionPreference = 'Stop'
Set-Location (Split-Path $PSScriptRoot)

if ($Version -notmatch '^\d+\.\d+\.\d+$') { throw "Use a version like 1.2.3." }
if ((git rev-parse --abbrev-ref HEAD) -ne 'main') { throw "Releases are made from main." }
if (git tag -l "v$Version") { throw "v$Version already exists." }
$dirty = git status --porcelain | Where-Object { $_ -notmatch 'CHANGELOG\.md$' }
if ($dirty) { throw "Commit or stash your changes first (only CHANGELOG.md may be edited)." }
if (-not (Select-String -Path CHANGELOG.md -Pattern "^## $([regex]::Escape($Version))( |$)" -Quiet)) {
    throw "Add a '## $Version - <date>' section to CHANGELOG.md first."
}

# Only the [package] version (the first one), not dependency versions.
$toml = [IO.File]::ReadAllText("$PWD\Cargo.toml")
$toml = ([regex]'(?m)^version = "[^"]*"').Replace($toml, "version = `"$Version`"", 1)
[IO.File]::WriteAllText("$PWD\Cargo.toml", $toml)

cargo test
if ($LASTEXITCODE) { throw "Tests failed." }
cargo build --release   # also updates Cargo.lock
if ($LASTEXITCODE) { throw "Build failed." }

git add Cargo.toml Cargo.lock CHANGELOG.md
git commit -m "Release $Version"
git tag -a "v$Version" -m "v$Version"
git push origin main "v$Version"
if ($LASTEXITCODE) { throw "Push failed." }
"v$Version pushed. The release appears at https://github.com/grayfvll01/apple-music-discord-presence/releases in a few minutes."
